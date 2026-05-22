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

## Language (v0.1)

- All values are 64-bit integers (`i64`)
- Arithmetic: `+ - * / %`, unary minus `-x`
- Comparison: `== != < <= > >=` (yields `1`/`0`)
- Variables: `let x = expr;` and assignment `x = expr;`
- Control flow: `if (cond) { } else { }`, `while (cond) { }`
- Functions: `fn name(args...) { ... return expr; }` — recursion and mutual recursion supported
- Built-in: `print expr;` (prints an integer followed by a newline)
- Comments: `# to end of line`
- Entry point: `fn main()`

### Example

```
fn fib(n) {
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
| `src/codegen.rs`| Lowers the AST to LLVM IR; JIT, object emission, IR printing |
| `src/main.rs`   | CLI driver (`run` / `build` / `emit-ir`) |

## Roadmap

Lumo is heading from a proof of concept toward a small, fast, statically typed
systems language with first-class tooling. See **[ROADMAP.md](ROADMAP.md)** for
the long-range plan (types, a type system, modules, LSP/formatter/REPL,
incremental + parallel compilation, WASM, and a path to 1.0).

Nearest steps: source spans & rich diagnostics, a test harness + CI, then
`bool`/`float` types.

## License

MIT
