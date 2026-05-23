# Lumo

A tiny programming language that compiles to native code through **LLVM**.
Written in Rust using [inkwell](https://github.com/TheDan64/inkwell) (safe LLVM bindings).

```
source (.lum)
  -> lexer    src/lexer.rs    text  -> tokens
  -> parser   src/parser.rs   tokens -> AST   (recursive descent)
  -> codegen  src/codegen.rs  AST    -> LLVM IR
  -> LLVM     optimization + machine code  (JIT or native executable)
```

You write the front end; LLVM handles optimization and code generation.

## Requirements

- Rust (stable)
- LLVM 22 (Homebrew: `brew install llvm@22`)

`.cargo/config.toml` sets `LLVM_SYS_221_PREFIX` to `/opt/homebrew/opt/llvm@22`,
so `inkwell`/`llvm-sys` can find LLVM. Adjust the path if your install differs.

## Build

```sh
cargo build --release
```

## Usage

```sh
# Run immediately via JIT
cargo run -- run examples/fib.lum

# Inspect the generated LLVM IR
cargo run -- emit-ir examples/fib.lum

# Build a native executable, then run it
cargo run -- build examples/fib.lum
./fib
```

### Optimization

Pass `-O0`/`-O1`/`-O2`/`-O3` (default `-O0`) to any command to run LLVM's
optimization pipeline on the module:

```sh
# See how mem2reg, inlining, etc. transform the IR
cargo run -- emit-ir -O2 examples/fib.lum

# Optimized native build
cargo run -- build -O2 examples/fib.lum
```

## Language (v0.16, in progress)

See **[docs/language.md](docs/language.md)** for the full reference, or
**[docs/tutorial.md](docs/tutorial.md)** for a gentle introduction. In brief:

- Types: `int` (64-bit), `bool`, `float` (64-bit), `string`, arrays `[T]`, and user-defined `struct`s — no implicit conversions
- Literals: `42`, `true` / `false`, `3.14`, `"text"`, `[1, 2, 3]`, `Point { x: 1, y: 2 }`, `null`
- Strings: `+` concatenates (heap-allocated), `==`/`!=` compare by value, `s[i]` reads a byte (bounds-checked)
- Arrays: `a[i]` read/write (bounds-checked), `len(a)`; heap-allocated, scalar elements
- Structs: `struct Point { x: int, y: int }`, field access `p.x` (read/write), nestable
- `null` for reference types (string/array/struct) → recursive data structures (linked lists, trees); null deref is caught at runtime
- Built-ins: `int(x)` / `float(x)` conversions, `len(x)`, `str(x)` (stringify int/float/bool for `+` building)
- Variables are lexically block-scoped, with shadowing; optional type annotations (`let x: T = ...`)
- Arithmetic: `+ - * /` on two ints or two floats, `%` (int only), unary minus `-x`
- Comparison: `== != < <= > >=` (yields a `bool`)
- Logical: `&&`, `||` (short-circuit), `!` — operate on `bool`
- Variables: `let x = expr;` and assignment `x = expr;` (assignment must keep the same type)
- Control flow: `if/else`, `while`, `for (init; cond; step)`, `break`, `continue` — conditions must be `bool`
- Functions with **typed signatures**: `fn name(p: int, q: float) -> bool { ... }`
  - Parameter types are required; the return type is optional and defaults to `int`
  - `int`, `bool`, and `float` may all be passed and returned; recursion and mutual recursion supported
  - A dedicated type-checking pass (`src/typeck.rs`) runs before code generation
- Built-in: `print expr;` (prints an `int`, `bool`, or `float`, followed by a newline)
- Comments: `# to end of line`
- Entry point: `fn main()`
- Errors are reported with a code, source location, and a caret (e.g. `error[E0201]: ...`)

### Example

```
fn fib(n: int) -> int {
    if (n < 2) { return n; }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    let i = 0;
    while (i < 15) {
        print fib(i);
        i = i + 1;
    }
    return 0;
}
```

## Project layout

| File | Responsibility |
|------|----------------|
| `src/lexer.rs`  | Tokenizer: source text into tokens |
| `src/parser.rs` | Recursive-descent parser: tokens into an AST |
| `src/ast.rs`    | AST node definitions |
| `src/types.rs`  | The `Type` enum (`int` / `bool` / `float`) shared by typeck and codegen |
| `src/typeck.rs` | Type-checking pass: name resolution + type rules, with diagnostics |
| `src/codegen.rs`| Lowers the (type-checked) AST to LLVM IR; optimization, JIT, object emission, IR printing |
| `src/diagnostics.rs` | Error rendering with source spans and carets |
| `src/main.rs`   | CLI driver (`run` / `build` / `emit-ir`, `-O` flag) |

## Documentation

- **[docs/tutorial.md](docs/tutorial.md)** — a getting-started tutorial (start here)
- **[docs/language.md](docs/language.md)** — the language reference (for users)
- **[docs/internals.md](docs/internals.md)** — compiler architecture (for contributors)
- **[CHANGELOG.md](CHANGELOG.md)** — release history
- **[ROADMAP.md](ROADMAP.md)** — the long-range plan
- **[docs/rfcs/](docs/rfcs/)** — design RFCs (e.g. the memory-model proposal)

## Roadmap

Lumo is heading from a proof of concept toward a small, fast, statically typed
systems language with first-class tooling. See **[ROADMAP.md](ROADMAP.md)** for
the long-range plan (types, a type system, modules, LSP/formatter/REPL,
incremental + parallel compilation, WASM, and a path to 1.0).

Nearest steps: source spans & rich diagnostics, a test harness + CI, then
`bool`/`float` types.

## License

MIT
