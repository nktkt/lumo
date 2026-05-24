# Lumo Compiler Internals

A contributor-facing guide to how the Lumo compiler is put together. If you want
to *use* the language, read [`docs/language.md`](./language.md) instead. This
document is about the *implementation*: the stages, the data that flows between
them, and where to look (and what to touch) when you change things.

Lumo is a small statically typed language compiled to native code via LLVM,
using the [`inkwell`](https://crates.io/crates/inkwell) crate (LLVM 22
bindings). All compiler code lives in `src/`.

---

## 1. Overview

Compilation is a straight, single-pass-per-stage pipeline. Each stage consumes
the output of the previous one and either produces the next representation or
returns a `Diagnostic` (a positioned error).

```text
  source text (&str)
        │
        ▼
   ┌─────────┐   Vec<Token>     ┌─────────┐   Program (AST)   ┌──────────┐
   │  lexer  │ ───────────────▶ │ parser  │ ────────────────▶ │ type     │
   │ lexer.rs│                  │parser.rs│                   │ checker  │
   └─────────┘                  └─────────┘                   │typeck.rs │
                                                              └──────────┘
                                                                    │ Program
                                                                    │ (now known well-typed)
                                                                    ▼
                                              ┌──────────────────────────────┐
                                              │      code generation          │
                                              │         codegen.rs            │
                                              │   AST ─▶ LLVM IR (inkwell)    │
                                              └──────────────────────────────┘
                                                                    │ LLVM module
                                                  (optional) ┌──────┴───────┐
                                                             │ optimization │  -O1/-O2/-O3
                                                             │ pass manager │  (driven from main.rs)
                                                             └──────┬───────┘
                                                                    ▼
                                          ┌────────────┬────────────┬─────────────┐
                                          │  JIT run   │ native exe │  IR text     │
                                          │ (lumo run) │(lumo build)│(lumo emit-ir)│
                                          └────────────┴────────────┴─────────────┘
```

The driver in `main.rs` wires these together. The compile core is:

```rust
let tokens  = lexer::lex(&src)?;     // text  -> tokens
let program = parser::parse(tokens)?; // tokens -> AST
typeck::check(&program)?;             // AST    -> (verified) AST
cg.compile(&program)?;                // AST    -> LLVM IR
```

Two cross-cutting modules are used everywhere:

- `span.rs` — `Span { start, end }`, byte offsets into the source, attached to
  every token and AST node so errors can point at exact source ranges.
- `diagnostics.rs` — `Diagnostic`, the single error type that every stage
  returns and that the driver renders.

A shared `types.rs` defines the three Lumo value types and is used by both the
type checker and codegen, so the two stages agree on what a type *is*.

---

## 2. The stages

### 2.1 Lexer (`lexer.rs`)

- **Consumes:** the raw source `&str`.
- **Produces:** `Result<Vec<Token>, Diagnostic>`, terminated by a `Tok::Eof`.

A hand-written tokenizer scans the source by character index. Each `Token`
carries a `Tok` kind and a `Span`:

```rust
pub struct Token { pub kind: Tok, pub span: Span }
```

It recognizes:

- integer and float literals (a `.` followed by a digit makes it a float; `1.`
  and `1.x` are *not* floats),
- identifiers and keywords (`fn`, `let`, `if`, `else`, `while`, `return`,
  `print`, `true`, `false`),
- line comments starting with `#` (skipped to end of line),
- two-character operators (`==`, `!=`, `<=`, `>=`, `&&`, `||`, `->`) checked
  before one-character operators,
- one-character punctuation and operators.

**Errors:** an illegal character (`E0001`) or a malformed/oversized number
literal (`E0003`).

### 2.2 Parser (`parser.rs`)

- **Consumes:** `Vec<Token>`.
- **Produces:** `Result<Program, Diagnostic>` where `Program = Vec<Function>`.

A recursive-descent parser with precedence climbing for expressions. Operator
precedence, low to high:

```text
||  <  &&  <  comparison (== != < <= > >=)  <  + -  <  * / %  <  unary (- !)  <  primary
```

Each precedence level is its own method (`parse_or`, `parse_and`,
`parse_comparison`, `parse_additive`, `parse_multiplicative`, `parse_unary`,
`parse_primary`), each calling the next-higher level for its operands. Primaries
are literals, parenthesized expressions, variables, and function calls.

Functions have typed parameters and an optional return type:

```text
fn f(x: int, y: bool) -> bool { ... }
```

The return type defaults to `int` when `->` is omitted. Statements include
`let`, assignment, `print`, `return`, `if`/`else` (with `else if` desugared to a
single `if` statement inside the `else` block), `while`, and bare expression
statements. Every node records a `Span` (built by merging child spans where
needed) for later diagnostics.

**Errors:** all syntax errors use code `E0002`, except an unknown/missing type
name in an annotation, which is `E0300`.

### 2.3 Type checker (`typeck.rs`)

- **Consumes:** the `Program` (AST).
- **Produces:** `Result<(), Diagnostic>`. It does not transform the AST; it
  verifies it. After it returns `Ok`, the program is well-typed.

This is the single source of type-error diagnostics. It runs in two phases:

1. **Collect signatures.** Walk all functions and build a map of name →
   `(param types, return type)`. This is done first so calls can refer to
   functions defined later (forward references and mutual recursion). It rejects
   duplicate function definitions and a missing `main`.
2. **Check bodies.** For each function, seed a scope with the parameters
   (rejecting duplicate parameter names), then check every statement and
   expression against the type rules.

The rules it enforces include: arithmetic and comparison operands must be
numeric and the two sides must match; `%` is `int`-only; `&&`/`||` and `if`/
`while` conditions are `bool`; assignments must match the variable's type;
`return` must match the function's declared return type; calls must have the
right arity and argument types. The shared `Type` enum and its `is_numeric()`
helper come from `types.rs`.

**Errors:** name/arity errors are `E01xx`, type errors are `E02xx`, and type
annotation problems are `E03xx` (see the table in §4).

### 2.4 Code generation (`codegen.rs`)

- **Consumes:** the type-checked `Program`.
- **Produces:** an LLVM module (built into `CodeGen`), then one of: JIT
  execution, an object file linked into an executable, or IR text.

Codegen **trusts** the type checker. It performs no type checking of its own —
it is pure lowering. Because the program is known to be well-typed, codegen can
use `unwrap` / `unreachable!` freely on shape it knows must hold.

The flow in `compile`:

1. Declare C `printf` (used to implement `print`).
2. Declare every function signature up front (forward references / mutual
   recursion), recording each return type.
3. Generate each function body.
4. Run `module.verify()` as a self-check; a failure here is an internal
   compiler bug, not a user error.

The interesting parts of lowering (value model, control flow, short-circuit
logic, `print`) are described in §3.

The backends live on `CodeGen`:

- `jit_run()` — build a JIT execution engine and call `main` immediately
  (`lumo run`).
- `build_executable(out)` — emit an object file with the target machine, then
  shell out to `clang` to link a native executable (`lumo build`).
- `ir_string()` — return the textual IR (`lumo emit-ir`).

### 2.5 Optimization (driven from `main.rs`)

There is **no separate optimizer module.** Optimization is invoked from the CLI
driver after codegen, via inkwell's LLVM pass manager. The driver accepts an
optimization-level flag, `-O0` / `-O1` / `-O2` / `-O3` (default `-O0`). When the
level is greater than zero, a pass pipeline runs over the freshly generated
module before output.

This is why `lumo emit-ir -O2` shows optimized IR: passes such as `mem2reg`
promote the stack `alloca` slots into SSA registers, so the `alloca`/`load`/
`store` pattern emitted by codegen largely disappears.

### 2.6 Driver (`main.rs`)

The CLI entry point. It reads the source file, runs the compile core
(lex → parse → typeck → codegen), and on any `Diagnostic` prints the rendered
error to stderr and exits non-zero. On success it dispatches on the subcommand
(`run` / `build` / `emit-ir`) and applies the optimization level.

---

## 3. The value model in codegen

Every Lumo value is lowered to a pair:

```rust
(BasicValueEnum<'ctx>, Type)
```

The first element is the LLVM value; the second is the Lumo `Type` that travels
*alongside* it so codegen knows which LLVM instruction to emit and which `print`
format to use. The mapping to LLVM types is:

| Lumo type | LLVM type | notes                |
|-----------|-----------|----------------------|
| `Int`     | `i64`     | signed               |
| `Bool`    | `i1`      | `true`/`false`       |
| `Float`   | `f64`     | IEEE double          |

`gen_expr` returns this pair for every expression, threading the `Type` upward.

### Alloca-based variables

Local variables and parameters live in stack slots created with `alloca`.
Reading a variable is a `load`; writing is a `store`. Parameters are copied into
their own `alloca` on function entry so they are treated uniformly with locals.

This keeps codegen simple — there is no SSA bookkeeping in the front end. The
LLVM `mem2reg` pass (run at `-O1` and above) promotes these slots into registers
later, so the redundant memory traffic is optimized away.

A re-`let` of an existing name simply allocates a fresh slot and rebinds the
name, which is why a re-`let` may change a variable's type.

### Dispatching int vs float

Arithmetic and comparison operators are not logical, so they go through
`gen_arith_or_cmp`, which branches on the operand `Type`:

```text
if ty == Float:  build_float_add / _sub / _mul / _div, build_float_compare (OEQ, OLT, ...)
else (Int):      build_int_add  / _sub / _mul, signed_div, signed_rem (%), build_int_compare (EQ, SLT, ...)
```

`%` exists only on the int path (the type checker guarantees `int` operands).
Unary `-` likewise picks `build_float_neg` vs `build_int_neg` from the type, and
unary `!` is a bitwise `not` on the `i1`.

### Control flow and short-circuit logic

`if` and `while` are lowered to basic blocks (`then`/`else`/`ifcont`, and
`while.cond`/`while.body`/`while.end`). Codegen tracks whether the current block
is still "open" (has no terminator yet) before adding branches, so a block ended
by a `return` does not get a stray jump appended.

`&&` and `||` short-circuit. The left operand is evaluated, then a conditional
branch decides whether to evaluate the right operand at all; the two paths merge
at a block whose result is selected by a `phi` node. For `&&`, skipping the
right side yields `false`; for `||`, it yields `true`.

### `print`

`print` is implemented by calling C `printf` with a format chosen from the
value's `Type`: `%lld` for `int`, `%g` for `float`, and `%s` for `bool` (the
value selects between the global strings `"true"` and `"false"` with a
`select`). Format strings and the literals are interned in a small cache so each
is created once per module.

---

## 4. Error handling and diagnostics

Every stage returns `Result<_, Diagnostic>`. A `Diagnostic` is:

```rust
pub struct Diagnostic {
    pub code: Option<&'static str>, // e.g. "E0101"
    pub message: String,
    pub span: Option<Span>,         // source range to underline
}
```

It is built fluently, e.g.:

```rust
Diagnostic::error("未定義の変数: x")
    .with_code("E0101")
    .at(some_span)
```

Errors propagate up through the `?` operator in the compile core in `main.rs`.
The driver catches the single `Err(diag)`, calls `diag.render(&src, path)`, and
exits non-zero. There is no error recovery: the first error stops compilation.

`render` turns the byte-offset `Span` into a 1-based line/column, slices out the
offending source line, and draws a caret (`^`) underline beneath the offending
range:

```text
error[E0101]: 未定義の変数: x
  --> examples/bad.lum:2:12
  |
2 |     return x;
  |            ^
```

### Error-code ranges

| Range    | Stage / category          | Examples                                            |
|----------|---------------------------|-----------------------------------------------------|
| `E000x`  | lexing                    | `E0001` illegal character, `E0003` bad number       |
| `E0002`  | parsing                   | unexpected token / syntax error                     |
| `E01xx`  | names / arity / modules   | `E0100` no `main`, `E0101` undefined var, `E0102` undefined fn, `E0103` duplicate fn, `E0104` wrong arity, `E0105` import not found |
| `E02xx`  | types                     | `E0200` type mismatch, `E0201` non-bool condition, `E0202` wrong return type |
| `E03xx`  | type annotations          | `E0300` unknown type, `E0301` duplicate parameter   |

(The codes within each range above are the ones currently emitted; new codes
should stay inside the matching range.)

---

## 5. Testing

End-to-end tests live in `tests/integration.rs`. They build the `lumo` binary
(via `CARGO_BIN_EXE_lumo`) and run it on small programs.

There are two kinds of cases, both stored under `tests/cases/`:

- **Golden-output cases.** For a case `foo`, the test runs `lumo run
  tests/cases/foo.lum` and asserts the process succeeds and its stdout exactly
  equals the contents of `tests/cases/foo.out`. Helper: `run_ok`.
- **Error cases.** The test runs the program and asserts a non-zero exit *and*
  that stderr contains the expected `Exxxx` code. Helper: `run_err`. This checks
  that the right diagnostic fires without pinning the exact wording of the
  message.

To add a test: drop a `.lum` file (and, for success cases, a matching `.out`
file) into `tests/cases/`, then add a `#[test]` function that calls `run_ok` or
`run_err` with the case name.

### CI

`.github/workflows/ci.yml` runs on every push to `main` and on pull requests,
on both `ubuntu-latest` and `macos-latest`, with LLVM 22 installed (Homebrew
`llvm@22` on macOS, the apt.llvm.org `llvm.sh 22` script on Linux). The job
runs, in order:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets -- -D warnings` (warnings are errors)
3. `cargo build`
4. `cargo test`

Keep the tree warning-free and formatted, or CI will fail.

---

## 6. How to add a new feature

Lumo's stages are deliberately layered, so most features touch the same files in
the same order. As a worked example, suppose you want to add a new operator (say
a bitwise/shift operator, or a builtin like `abs`). Walk the pipeline:

1. **Lexer (`lexer.rs`).** Add the new `Tok` variant and teach the scanner to
   produce it. Put multi-character operators in the two-char table *before* the
   single-char fallthrough.
2. **AST (`ast.rs`).** Add a node for it — a new `BinOp`/`UnOp` variant for an
   operator, or rely on `ExprKind::Call` for a builtin. Make sure spans are
   carried.
3. **Parser (`parser.rs`).** Wire the token into the grammar at the correct
   precedence level (or add a level). For a builtin you may not need parser
   changes at all if it parses as an ordinary call.
4. **Type checker (`typeck.rs`).** Add the type rule: what operand types are
   allowed and what type the result is. Emit a positioned `Diagnostic` with a
   code in the right range for any misuse. This is the *only* place type errors
   should originate.
5. **Codegen (`codegen.rs`).** Lower it to LLVM IR. Dispatch on the operand
   `Type` if behavior differs for int vs float, and remember to return the
   correct result `(BasicValueEnum, Type)`. Trust the type checker — don't
   re-validate types here.
6. **Tests (`tests/cases/` + `tests/integration.rs`).** Add a golden-output
   case for the happy path and at least one error case proving the new type rule
   rejects bad input with the expected `Exxxx` code.
7. **Docs.** Update [`docs/language.md`](./language.md) for the user-facing
   behavior, and this file if you changed the architecture (a new error-code,
   stage, or invariant).

A feature is not "done" until it has a regression test and CI is green.

---

## 7. Further reading

- [`docs/language.md`](./language.md) — the user-facing language reference
  (syntax, types, semantics, examples).
- [`ROADMAP.md`](../ROADMAP.md) — where Lumo is headed and which features are
  planned but not yet built.
