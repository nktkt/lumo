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
| [`greet.lum`](greet.lum) | Heap strings — `+` concatenation that allocates new strings and `==` / `!=` value comparison, used to build a full name and check a password. |
| [`parse.lum`](parse.lum) | String indexing `s[i]` (a bounds-checked byte as `int`) with `len()` — a digit-scanning `parse_int` and a two-pointer palindrome check. |
| [`table.lum`](table.lum) | The `str()` built-in — converting ints, floats, and bools to text so numbers can be mixed into output with `+` in a squares table. |
| [`arrays.lum`](arrays.lum) | Array types `[int]` with literals, indexed read/write `a[i]`, and `len()` — a `max` helper and an in-place bubble sort. |
| [`sort.lum`](sort.lum) | Lexicographic string ordering — `<` / `>` comparisons on strings driving an insertion sort over an array of fruit names. |
| [`primes.lum`](primes.lum) | Growable arrays — starting from an empty literal (`let primes: [int] = [];`) and using the `push(a, x)` built-in to collect an unknown number of primes. |
| [`stats.lum`](stats.lum) | The math built-ins — `mean` and standard deviation over a `[float]` using `sqrt`, `pow`, `min`, and `max`, with `for`-`in` loops over the array. |
| [`cipher.lum`](cipher.lum) | The `chr()` built-in with string indexing `s[i]` and `+` — a Caesar cipher that shifts and rebuilds lowercase letters, then decrypts. |
| [`structs.lum`](structs.lum) | Structs — `struct` declarations, `Name { field: value }` construction, `obj.field` read/write, nested struct-typed fields, and field mutation. |
| [`points.lum`](points.lum) | Arrays of structs `[Point]` — indexing yields a reference, so `ps[i].x` reads and mutates in place while computing a centroid. |
| [`sum_stdin.lum`](sum_stdin.lum) | Reading input — `read_line()` returning a line or `null` at end of input, looped to parse and sum integers piped on stdin. |
| [`wordcount.lum`](wordcount.lum) | Maps — tally word frequencies from stdin in a `{string: int}`, then print each unique word and count using `has`, `keys`, a sort, and a `for`-`in` loop. |
| [`csv.lum`](csv.lum) | The string toolkit — `split` `name,score` lines from stdin, validate with `is_int` and parse with `int(s)`, total them in a map, sort, and rebuild a line with `join`. |
| [`linked_list.lum`](linked_list.lum) | `null` as a nullable reference plus a self-referential `struct Node` — a recursive linked list with prepend, traversal, and in-place reversal. |
| [`save_load.lum`](save_load.lum) | File I/O — `write_file` to persist a `name,score` table to disk, then `read_file` (returning `null` if absent) to load it back, `split` it into records, and report the leader. |
| [`matrix.lum`](matrix.lum) | Nested collections — `[[int]]` matrices with 2D literals, chained indexing (`m[i][j]`), and growing rows with `push`, driving a full matrix-multiply. |
| [`tags.lum`](tags.lum) | String methods — `split` a messy tag list, then `trim` + `to_lower` each piece, and answer membership (`==`) and substring (`contains`) queries. |
| [`slug.lum`](slug.lum) | String rewriting — `replace` to swap spaces/strip punctuation and `repeat` to draw a title underline, building a "slugify" routine with `trim` + `to_lower`. |
| [`rpn.lum`](rpn.lum) | Stack operations — `push`/`pop` to evaluate reverse-Polish-notation expressions, tokenized with `split` and parsed with `is_int`/`int`. |
