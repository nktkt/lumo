# Changelog

All notable changes to the **Lumo** programming language compiler are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com), and this project uses semantic-ish versioning while pre-1.0: anything may change.

## [Unreleased]

- _Nothing yet._

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
