# Lumo Cookbook

A collection of small, task-oriented recipes for the **Lumo** language. Each
recipe is a complete, runnable program: copy it into a file (say `recipe.lum`)
and run it with the JIT:

```sh
cargo run -- run recipe.lum
```

Every program starts at `fn main()`, and the value `main` returns becomes the
process exit code (`0` means success). The expected output of each program is
shown in `#` comments next to the relevant `print`.

For the full story see the [tutorial](tutorial.md) and the
[language reference](language.md).

---

### How do I print a value and build a message from a number?

Use `print` to write a value, and `str(...)` with `+` to splice a number into a
string (there are no implicit conversions, so the number must be converted
first).

```lumo
fn main() {
    print 42;                       # 42
    let n = 7;
    print "answer = " + str(n);     # answer = 7
    print "pi ~ " + str(3.14);      # pi ~ 3.14
    return 0;
}
```

---

### How do I sum an array of ints?

Walk the array by index with a `for` loop, using `len` for the bound, and add
each element to a running total.

```lumo
fn sum(ns: [int]) -> int {
    let total = 0;
    for (let i = 0; i < len(ns); i = i + 1) {
        total = total + ns[i];
    }
    return total;
}

fn main() {
    let nums = [4, 8, 15, 16, 23, 42];
    print sum(nums);    # 108
    return 0;
}
```

---

### How do I find the maximum of an array of ints?

Start with the first element, then fold the rest in with the built-in `max(a, b)`
(use `min` for the smallest).

```lumo
fn largest(ns: [int]) -> int {
    let best = ns[0];
    for (let i = 1; i < len(ns); i = i + 1) {
        best = max(best, ns[i]);
    }
    return best;
}

fn main() {
    let nums = [3, 9, 2, 14, 7];
    print largest(nums);    # 14
    return 0;
}
```

---

### How do I iterate with `for` and use `break` and `continue`?

`break` leaves the loop immediately; `continue` skips to the next iteration.

```lumo
fn main() {
    for (let i = 0; i < 100; i = i + 1) {
        if (i == 10) { break; }        # stop entirely at 10
        if (i % 2 == 0) { continue; }  # skip even numbers
        print i;                       # prints 1, 3, 5, 7, 9
    }
    return 0;
}
```

---

### How do I write a recursive function (factorial)?

A recursive function calls itself. Always handle the base case first so the
recursion terminates.

```lumo
fn factorial(n: int) -> int {
    if (n <= 1) {
        return 1;                   # base case
    }
    return n * factorial(n - 1);    # recursive case
}

fn main() {
    print factorial(5);    # 120
    return 0;
}
```

---

### How do I reverse a string?

Strings are immutable, so build a fresh one. Read bytes with `s[i]`, turn each
back into a one-character string with `chr`, and concatenate from the end toward
the start.

```lumo
fn reverse(s: string) -> string {
    let r = "";
    for (let i = len(s) - 1; i >= 0; i = i - 1) {
        r = r + chr(s[i]);
    }
    return r;
}

fn main() {
    print reverse("Lumo");    # omuL
    return 0;
}
```

---

### How do I uppercase a string?

Read each byte; if it falls in the lowercase ASCII range `a`–`z` (bytes 97–122),
subtract 32 to shift it to uppercase, then rebuild the string with `chr` and `+`.

```lumo
fn upper(s: string) -> string {
    let r = "";
    for (let i = 0; i < len(s); i = i + 1) {
        let c = s[i];
        if (c >= 97 && c <= 122) {
            c = c - 32;
        }
        r = r + chr(c);
    }
    return r;
}

fn main() {
    print upper("hello, lumo");    # HELLO, LUMO
    return 0;
}
```

---

### How do I parse a non-negative integer from a string?

Each digit byte equals its value plus 48 (`'0'` is byte 48), so `s[i] - 48`
gives the digit. Accumulate left to right with `n * 10 + digit`.

```lumo
fn parse_int(s: string) -> int {
    let n = 0;
    for (let i = 0; i < len(s); i = i + 1) {
        n = n * 10 + (s[i] - 48);    # '0' is byte 48
    }
    return n;
}

fn main() {
    print parse_int("2026");    # 2026
    return 0;
}
```

---

### How do I define and use a struct, and an array of structs?

Declare the struct at the top level. Construct values with `Name { field: ... }`,
read fields with `.`, and store many of them in a `[Point]`. Indexing yields the
struct, so `ps[i].x` reads (and could mutate) a field in place.

```lumo
struct Point {
    x: int,
    y: int,
}

fn main() {
    let ps = [
        Point { x: 1, y: 2 },
        Point { x: 3, y: 4 },
        Point { x: 5, y: 6 },
    ];

    let total = 0;
    for (let i = 0; i < len(ps); i = i + 1) {
        total = total + ps[i].x + ps[i].y;
    }
    print total;    # 21
    return 0;
}
```

---

### How do I build and traverse a linked list with `null`?

A self-referential struct (a field of its own type) makes a linked list; a
`null` link marks the end. Use a recursive function to compute its length,
checking for `null` before following a link.

```lumo
struct Node {
    val: int,
    next: Node,
}

fn length(list: Node) -> int {
    if (list == null) {                 # reached the end
        return 0;
    }
    return 1 + length(list.next);
}

fn main() {
    # build  1 -> 2 -> 3 -> null
    let list = Node {
        val: 1,
        next: Node {
            val: 2,
            next: Node { val: 3, next: null },
        },
    };
    print length(list);    # 3
    return 0;
}
```

---

### How do I use mutual recursion (even / odd)?

Two functions can call each other. Each peels off one step until it hits a base
case of `0`.

```lumo
fn even(n: int) -> bool {
    if (n == 0) { return true; }
    return odd(n - 1);
}

fn odd(n: int) -> bool {
    if (n == 0) { return false; }
    return even(n - 1);
}

fn main() {
    print even(10);    # true
    print odd(10);     # false
    return 0;
}
```

---

### How do I convert between int and float (compute an average)?

There are no implicit conversions, so use `float(...)` to widen ints before
dividing, and `int(...)` to truncate a float back to an int.

```lumo
fn average(ns: [int]) -> float {
    let total = 0;
    for (let i = 0; i < len(ns); i = i + 1) {
        total = total + ns[i];
    }
    return float(total) / float(len(ns));
}

fn main() {
    let nums = [2, 4, 9];
    print average(nums);            # 5
    print int(average(nums));       # 5    (truncates 5.0)
    return 0;
}
```

---

## Where to go next

- **[tutorial.md](tutorial.md)** — a guided walkthrough from "hello world" to
  arrays, structs, and recursive data.
- **[language.md](language.md)** — the authoritative reference: every type,
  operator, built-in, precedence rule, and the formal grammar.
</content>
</invoke>
