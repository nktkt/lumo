# Getting Started with Lumo

Welcome! **Lumo** is a small, statically typed programming language. Your code
is compiled to native machine code through [LLVM](https://llvm.org/) (the
compiler itself is written in Rust using the `inkwell` bindings).

If you already know another language — Python, JavaScript, Go, Rust, C — you'll
feel right at home. Lumo keeps the core ideas familiar: functions, variables,
`if`/`while`/`for`, and the usual operators. It just happens to be small, fast,
and strict about types.

This tutorial walks you from "hello world" to writing your own functions. Every
section ends with a tiny program you can actually run. Let's go!

---

## Running your code

A Lumo program lives in a file ending in `.lum`. Save your program (say, as
`hello.lum`) and then ask the compiler to run it:

```sh
cargo run -- run hello.lum
```

`run` uses a **JIT** (just-in-time) compiler: it compiles your program in
memory and executes it immediately. That's the quickest way to try things out.

There are a few other commands:

| Command                              | What it does                                |
| ------------------------------------ | ------------------------------------------- |
| `cargo run -- run hello.lum`         | Compile in memory and run right away (JIT)  |
| `cargo run -- build hello.lum`       | Produce a standalone native executable      |
| `cargo run -- emit-ir hello.lum`     | Print the generated LLVM IR (for the curious)|

You can also pick an optimization level by adding `-O0`, `-O1`, `-O2`, or `-O3`:

```sh
cargo run -- run -O2 hello.lum
```

`-O0` is the default and compiles fastest; `-O3` produces the fastest-running
code. They all behave identically — only speed differs.

One rule before we write anything: **every program needs a `main` function.**
Execution starts there, and whatever `main` returns becomes the program's
**exit code** (the number your shell sees when the program finishes). By
convention, returning `0` means "success".

---

## Hello, world!

Here's the traditional first program:

```lumo
fn main() {
    print "Hello, world!";
    return 0;
}
```

Run it:

```sh
cargo run -- run hello.lum
```

You'll see:

```
Hello, world!
```

A few things to notice:

- `fn main() { ... }` declares the entry point.
- `print` writes a value and then a newline.
- `return 0;` ends `main` and sets the exit code to `0`.
- Statements end with a semicolon (`;`).

Comments start with `#` and run to the end of the line:

```lumo
fn main() {
    print "Hi";   # this is a comment
    return 0;
}
```

---

## Variables and types

Lumo has four built-in types:

| Type     | What it holds                | Example literals              |
| -------- | ---------------------------- | ----------------------------- |
| `int`    | 64-bit whole number          | `42`, `0`, `-7`               |
| `float`  | 64-bit decimal number        | `3.14`, `0.5`, `2.0`          |
| `bool`   | a truth value                | `true`, `false`               |
| `string` | a piece of text              | `"hello"`, `"line\n"`         |

A small but important rule about floats: a float literal needs **digits on both
sides of the dot**. Write `2.0`, not `2.`; write `0.5`, not `.5`.

You declare a variable with `let`. Lumo **infers** the type from the value you
give it, so you don't have to write the type yourself:

```lumo
let count = 0;        # int
let ratio = 0.5;      # float
let ready = true;     # bool
let name = "Lumo";    # string
```

To change a variable later, assign to it with `=`. The new value must have the
**same type** as the original — Lumo will not let you swap an `int` for a
`float`:

```lumo
let n = 1;
n = n + 1;   # ok: still an int
# n = 2.0;   # error: cannot put a float in an int variable
```

Lumo never converts between types automatically. An `int` is never silently
turned into a `float`, a `bool` is never treated as a number, and so on. If you
mix types, the compiler tells you (see [Errors](#when-things-go-wrong) below).

### About strings

Strings are written with double quotes and support a few **escape sequences**:

| Escape | Meaning            |
| ------ | ------------------ |
| `\n`   | newline            |
| `\t`   | tab                |
| `\\`   | a literal backslash|
| `\"`   | a literal quote    |

```lumo
print "Tabs\tand\nnewlines";
print "She said \"hi\"";
```

For now, strings are **immutable literals**. You can store them in variables,
pass them to functions, return them, and print them — but there is no string
concatenation or string comparison yet. (Those may come later!)

### Try it

```lumo
fn main() {
    let greeting = "Welcome to Lumo";
    let version = 1;
    let pi = 3.14;
    let ready = true;

    print greeting;
    print version;
    print pi;
    print ready;
    return 0;
}
```

Output:

```
Welcome to Lumo
1
3.14
true
```

(Notice that `print` shows a `bool` as `true` or `false`.)

---

## Operators

### Arithmetic: `+ - * /`

These work on **two `int`s** or **two `float`s** — both operands must be the
same type, and the result has that type. There's also unary minus (`-x`) to
negate a number.

```lumo
let a = 3 + 4;        # int, 7
let b = 1.5 * 2.0;    # float, 3.0
let c = -a;           # int, -7
```

### Modulo: `%`

The remainder operator works on `int` only:

```lumo
let r = 10 % 3;       # 1
```

### Comparison: `== != < <= > >=`

Compare two `int`s or two `float`s of the same type, and you get a `bool`:

```lumo
let same = (7 == 7);     # true
let bigger = 2.0 > 1.0;  # true
```

### Logical: `&& || !`

These work on `bool` values. `&&` (and) and `||` (or) **short-circuit** — they
stop evaluating as soon as the answer is known. `!` flips a `bool`.

```lumo
let ok  = ready && (count > 0);
let any = ready || bigger;
let no  = !ready;
```

### Don't mix types

Every operator wants matching types. Expressions like `1 + 2.0`, `true + 1`, or
`1 < 2.0` are all compile errors. When you really do need an `int` next to a
`float`, write the values in the same type to begin with (remember: no automatic
conversions).

### Try it

```lumo
fn main() {
    let x = 17;
    let y = 5;

    print x + y;     # 22
    print x - y;     # 12
    print x * y;     # 85
    print x / y;     # 3   (integer division)
    print x % y;     # 2
    print x > y;     # true
    return 0;
}
```

---

## Control flow

### `if` / `else`

The condition must be a `bool`. The `else` branch is optional.

```lumo
if (count > 0) {
    print "positive";
} else {
    print "zero or negative";
}
```

### `while`

Repeat a block while a condition holds:

```lumo
let i = 0;
while (i < 5) {
    print i;
    i = i + 1;
}
```

### `for`

A C-style loop: `for (init; cond; step) { ... }`. The `init` runs once at the
start, `cond` is checked before each pass, and `step` runs after each pass. Both
`init` and `step` are optional, but the two semicolons are not.

```lumo
for (let i = 0; i < 5; i = i + 1) {
    print i;
}
```

### `break` and `continue`

Inside any loop, `break` exits the loop immediately, and `continue` jumps to the
next iteration:

```lumo
for (let i = 0; i < 100; i = i + 1) {
    if (i == 10) { break; }        # stop entirely at 10
    if (i % 2 == 0) { continue; }  # skip even numbers
    print i;                       # prints 1, 3, 5, 7, 9
}
```

### Try it

A countdown that stops early:

```lumo
fn main() {
    let n = 10;
    while (n > 0) {
        if (n == 3) {
            print "liftoff soon...";
            break;
        }
        print n;
        n = n - 1;
    }
    return 0;
}
```

---

## Functions

You've already met `main`. Here's the full shape of a function:

```lumo
fn name(p: int, q: float) -> bool {
    # ...
    return true;
}
```

The rules:

- Every parameter must declare its type: `p: int`.
- The return type after `->` is **optional** and defaults to `int`. So
  `fn main() { ... }` returns an `int`.
- `int`, `float`, `bool`, and `string` can all be parameters and return values.
- `return expr;` ends the function, and `expr`'s type must match the declared
  return type.
- **Recursion** is supported — a function may call itself.

A simple function with a non-default return type:

```lumo
fn is_even(n: int) -> bool {
    return n % 2 == 0;
}

fn main() {
    print is_even(4);   # true
    print is_even(7);   # false
    return 0;
}
```

### Worked example: factorial (recursion)

`factorial(n)` is `n * (n-1) * ... * 1`. It's a classic recursion: the answer
for `n` is built from the answer for `n - 1`.

```lumo
fn factorial(n: int) -> int {
    if (n <= 1) {
        return 1;          # base case
    }
    return n * factorial(n - 1);   # recursive case
}

fn main() {
    let n = 5;
    print factorial(n);    # 120
    return 0;
}
```

### Worked example: FizzBuzz (strings + ints)

This mixes everything: a loop, `if`/`else if`-style chaining, `%`, and printing
both strings and ints. Since `else if` is just an `if` inside an `else` block,
we nest them.

```lumo
fn main() {
    for (let i = 1; i <= 15; i = i + 1) {
        if (i % 15 == 0) {
            print "FizzBuzz";
        } else {
            if (i % 3 == 0) {
                print "Fizz";
            } else {
                if (i % 5 == 0) {
                    print "Buzz";
                } else {
                    print i;
                }
            }
        }
    }
    return 0;
}
```

Output:

```
1
2
Fizz
4
Buzz
Fizz
7
8
Fizz
Buzz
11
Fizz
13
14
FizzBuzz
```

---

## When things go wrong

Lumo is strict on purpose: catching mistakes at compile time means fewer
surprises at run time. When something is wrong, the compiler prints an
**`error[Exxxx]`** code, points at the source line, and underlines the exact
spot with a `^` caret.

For example, this program mixes an `int` and a `float`:

```lumo
fn main() {
    let x = 1 + 2.0;   # int + float is not allowed
    return 0;
}
```

The compiler refuses to build it and explains why:

```
error[E0201]: type mismatch: cannot apply '+' to int and float
  --> hello.lum:2:13
  |
2 |     let x = 1 + 2.0;
  |             ^^^^^^^
```

The message tells you *what* went wrong, the `-->` line tells you *where*, and
the caret shows you the precise expression to fix. Error codes are grouped by
compiler phase (`E000x` for lexing, `E002` for parsing, `E01xx` for
names/arity, `E02xx` for types, `E03xx` for type annotations), so once you've
seen a code a few times you'll recognize the category at a glance.

---

## Where to go next

You now know enough Lumo to write real programs. From here:

- **[`docs/language.md`](language.md)** — the language **reference**: every type,
  operator, statement, precedence table, and the formal grammar.
- **[`docs/internals.md`](internals.md)** — the **compiler internals**: how Lumo
  goes from source text to LLVM IR to native code, if you'd like to peek under
  the hood (or contribute!).

Happy hacking with Lumo!
