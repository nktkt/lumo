# Changelog

All notable changes to the **Lumo** programming language compiler are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com), and this project uses semantic-ish versioning while pre-1.0: anything may change.

## [Unreleased]

- _Nothing yet._

## [0.20.0]

### Added

- `read_line()` built-in: reads a line from standard input, returning a `string` (trailing newline stripped) or `null` at end of input — enabling programs that process real input.
- `docs/cookbook.md`: task-oriented recipes.

## [0.18.1]

### Fixed

- Integer division/modulo by zero now aborts cleanly (`lumo: division by zero`, exit 101) instead of triggering undefined behavior (a SIGFPE crash). Float division by zero is unaffected (IEEE infinity). Found by a code-review pass.

## [0.18.0]

### Added

- `chr(b)` built-in: returns a one-character string for a byte value (int). Together with `s[i]` and `+`, this completes string manipulation (e.g. uppercasing, reversing, ciphers).

## [0.17.0]

### Added

- Arrays of structs: an array element type may now be a struct (e.g. `[Point]`). Indexing yields the struct by reference, so `ps[i].x` reads and `ps[i].x = v;` mutates in place.
- Trailing commas are now allowed in array literals (matching struct literals/defs).

## [0.16.0]

### Added

- String indexing `s[i]`: returns the i-th byte as an `int` (bounds-checked), enabling text processing together with `len` and `str`. Strings remain immutable, so `s[i] = ...` is a compile error (`E0207`).

### Changed

- The out-of-bounds runtime message is now `lumo: index out of bounds` (shared by arrays and strings).

## [0.15.0]

### Added

- `str(x)` built-in: converts `int` / `float` / `bool` to a `string` (a `string` passes through). With `+`, this lets you build text from values, e.g. `"answer = " + str(n)`.

## [0.14.0]

### Added

- `null`: a value compatible with any reference type (string, array, struct), enabling recursive data structures (linked lists, trees) via self-referential structs. Compare with `==` / `!=`.
- Optional type annotations on `let` (`let x: T = ...`), required when initializing with `null`.
- Reading a field/index through `null` is caught at runtime (`lumo: null reference`, exit 101).

### Notes

- New diagnostics: `E0208` (cannot infer the type of bare `null`). The runtime `lumo_bounds_fail` was generalized to `lumo_panic(msg, len)`, shared by the bounds and null checks.

## [0.13.0]

### Added

- User-defined structs: `struct Name { field: Type, ... }`, construction `Name { field: value, ... }`, and field access `obj.field` (read and write). Structs are heap-allocated, can be passed/returned, and can nest (struct-typed fields).
- Assignment targets now include struct fields.

### Notes

- New diagnostics: `E0303` (unknown struct), `E0304` (duplicate struct), `E0305` (field access on a non-struct), `E0306` (no such field), `E0307` (duplicate/missing field). Arrays of structs are not supported yet.

## [0.12.0]

### Added

- Array bounds checking: an out-of-range index (or a negative one) prints `lumo: array index out of bounds` to stderr and exits with status 101, instead of being undefined behavior.

## [0.11.0]

### Added

- Arrays: types `[int]`/`[bool]`/`[float]`/`[string]`, literals `[a, b, c]`, indexing `a[i]` (read and write), and `len(a)`. Heap-allocated via the runtime; scalar element types only (no nested arrays).
- `len(s)` also returns the length of a `string`.
- Assignment now targets an lvalue (a variable or an array element).

### Notes

- No array bounds checking yet (out-of-range indexing is undefined behavior); memory is reclaimed at program exit. See `docs/rfcs/0001-memory-model.md`.

## [0.10.0]

### Added

- A minimal runtime (`lumo_alloc`, currently backed by libc `malloc`) — Lumo's first heap allocation.
- String concatenation with `+` (allocates a new heap string) and string equality with `==` / `!=`.

### Notes

- Heap memory is reclaimed only at program exit for now; an arena/region scheme is planned (see `docs/rfcs/0001-memory-model.md`).

## [0.9.0]

### Added

- Lexical block scoping: variables are scoped to their enclosing `{ }` block, and an inner `let` may shadow an outer one. A `for`'s init variable is scoped to the loop.
- `docs/rfcs/0001-memory-model.md`: a design RFC for the future heap/memory strategy (strings, arrays).

### Changed

- Using a variable outside its block is now an error (`E0101`) — previously such variables leaked to the whole function.

## [0.8.0]

### Added

- `int(x)` and `float(x)` conversion built-ins (`float(int)` widens, `int(float)` truncates toward zero) — the way to mix `int` and `float`.
- `examples/README.md` cataloging the example programs.
- `E0302`: `int`/`float`/`bool`/`string` are reserved and cannot be used as function names.

## [0.7.0]

### Added

- `string` type with immutable string literals (`"..."`) supporting `\n`, `\t`, `\r`, `\\`, `\"`, `\0` escapes. Strings can be stored, passed, returned, and printed (no concatenation or comparison yet).
- `docs/tutorial.md`, a getting-started guide.
- `E0004` diagnostics for unterminated strings and invalid escapes.

## [0.6.0]

### Added

- `for (init; cond; step) { ... }` loops, with optional `init`/`step` clauses.
- `break` and `continue` statements (with `E0203` when used outside a loop).
- `CHANGELOG.md`.

## [0.5.0]

### Added

- `-O0`/`-O1`/`-O2`/`-O3` optimization flag on every command; runs LLVM's `default<On>` pass pipeline (e.g. `emit-ir -O2` shows mem2reg promoting stack slots to SSA).
- `docs/internals.md`, a contributor-facing compiler architecture guide.

## [0.4.0]

### Added

- `float` type (64-bit): float literals, float arithmetic and comparisons, float parameters/returns, printed with `%g`.
- `docs/language.md`, the user-facing language reference.

### Changed

- Arithmetic and comparisons now accept two ints OR two floats (same type); `%` remains int-only. No implicit int/float conversion.

## [0.3.0]

### Added

- `bool` type with `true`/`false` literals.
- Logical operators `&&`, `||` (short-circuit) and `!`.
- Typed function signatures: `fn f(x: int) -> bool` (parameter types required; return type optional, defaults to `int`); `bool`/`int` can cross function boundaries.
- A dedicated type-checking pass (`typeck`) run before code generation, with type diagnostics.

### Changed

- Comparisons now produce a `bool` (previously `1`/`0` ints).

## [0.2.0]

### Added

- Source spans tracked through the lexer, parser, and AST.
- Rich diagnostics: error codes, a source snippet, and a caret pointing at the offending span.
- End-to-end integration test harness (golden-output and error-code cases).
- GitHub Actions CI (fmt + clippy + build + test on Ubuntu and macOS with LLVM 22).
- `CONTRIBUTING.md` and issue/PR templates.

## [0.1.0]

### Added

- Initial proof of concept: an `i64`-only language with arithmetic (`+ - * / %`), comparisons, `if`/`else`, `while`, recursive functions, and `print`.
- Three CLI commands: `run` (JIT), `build` (native executable via clang), `emit-ir` (print LLVM IR).
- Implemented in Rust using inkwell (LLVM 22).
