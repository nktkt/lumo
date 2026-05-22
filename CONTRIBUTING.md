# Contributing to Lumo

Thanks for your interest in Lumo! This guide covers everything you need to get
a development environment running and to get your changes merged.

## Prerequisites

- **Rust (stable)** — install via [rustup](https://rustup.rs/).
- **LLVM 22** — Lumo links against LLVM 22 through the `inkwell`/`llvm-sys`
  crates. On macOS:

  ```sh
  brew install llvm@22
  ```

`.cargo/config.toml` sets `LLVM_SYS_221_PREFIX` to `/opt/homebrew/opt/llvm@22`
so `llvm-sys` can find your LLVM install automatically. If your LLVM lives
somewhere else, adjust that path (or override the environment variable).

## Build

```sh
cargo build
```

## Run

Run a `.lum` program immediately via the JIT:

```sh
cargo run -- run examples/fib.lum
```

Other subcommands:

```sh
cargo run -- emit-ir examples/fib.lum   # print the generated LLVM IR
cargo run -- build examples/fib.lum     # build a native executable
```

## Test

```sh
cargo test
```

## Code style

- Format your code before committing:

  ```sh
  cargo fmt
  ```

- Keep `cargo clippy` warning-free:

  ```sh
  cargo clippy --all-targets
  ```

CI runs `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings`,
so anything that fails locally will fail CI too.

## Commits & pull requests

- Write clear, descriptive commit messages that explain the *why*, not just the *what*.
- Keep PRs focused; one logical change per PR is easiest to review.
- **CI must be green** before a PR can be merged (build, tests, fmt, and clippy
  on macOS and Linux).
- Update documentation when behavior changes.

## Where to start

See **[ROADMAP.md](ROADMAP.md)** for the long-range plan. The best place to jump
in right now is **Phase 1 (Foundations)** — spans & rich diagnostics, the test
harness, and CI/tooling. Issues labeled for Phase 1 are the most welcoming for a
first contribution.
