# Lumo Examples

This directory holds small example programs written in **Lumo**, a tiny language
compiled with LLVM. Each `.lum` file is self-contained and demonstrates a few
language features.

## Running an example

From the project root, run any program with:

```sh
cargo run -- run examples/<name>.lum
```

You can also `build` it to a native executable or `emit-ir` to inspect the
generated LLVM IR:

```sh
cargo run -- build examples/<name>.lum
cargo run -- emit-ir examples/<name>.lum
```

Add `-O2` to enable optimizations, for example:

```sh
cargo run -- run examples/fib.lum -O2
```

## Learn more

- [Tutorial](../docs/tutorial.md) — a guided introduction to writing Lumo.
- [Language reference](../docs/language.md) — the full language description.

## Examples

Listed from simplest to most advanced:

| File | What it demonstrates |
| --- | --- |
| [`hello.lum`](hello.lum) | String literals: storing, passing, returning, and printing strings, including escape sequences (`\t`, `\n`); a `for` loop that prints a word per value. |
| [`fib.lum`](fib.lum) | Recursion (`fib`), `if`, a `while` loop, and `print` — computes the first 15 Fibonacci numbers. |
| [`math.lum`](math.lum) | Integer arithmetic with operator precedence and parentheses, the unary minus, modulo (`%`) in a `while`-based `gcd`, and a `square` helper function. |
| [`bool.lum`](bool.lum) | Booleans, logical operators (`&&`, `||`, `!`) with short-circuiting, comparisons producing `bool`, and typed function signatures returning `bool`. |
| [`float.lum`](float.lum) | 64-bit `float` arithmetic and comparisons, a typed `float -> float` function (`circle_area`), and float-only operations (no int/float mixing). |
| [`loops.lum`](loops.lum) | `for (init; cond; step)` loops, early `return`, and an `is_prime` check that drives a primes-below-30 listing. |
| [`average.lum`](average.lum) | The `int()` / `float()` conversion built-ins — summing ints in a loop, then `float()`-converting for true float division. |
