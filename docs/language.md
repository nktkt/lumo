# The Lumo Language

Lumo is a small, statically typed programming language compiled with LLVM. It
has three primitive types, simple control flow, first-class functions, and no
implicit conversions. This document describes the language as it currently
stands.

## A Complete Example

```lumo
# Approximate a square root with Newton's method, a fixed number of steps.
# (There is also a built-in `sqrt`; this just shows hand-written control flow.)
fn newton_sqrt(x: float, steps: int) -> float {
    let guess = x;
    let i = 0;
    while (i < steps) {
        guess = (guess + x / guess) / 2.0;
        i = i + 1;
    }
    return guess;
}

fn main() {
    let n = 16.0;
    let positive = n > 0.0;
    if (positive) {
        print newton_sqrt(n, 10);
    } else {
        print 0.0;
    }
    return 0;
}
```

Running this with `lumo run example.lum` prints the approximation and exits with
code `0` (the value returned by `main`).

## Types

Lumo has four primitive types and three compound types:

- `int` — 64-bit signed integer
- `bool` — either `true` or `false`
- `float` — 64-bit IEEE double
- `string` — an immutable text value
- `[T]` — an array of `T`, where `T` is any type: a scalar, a struct, or another
  collection (`[[int]]`, `[{string: int}]`) (see [Arrays](#arrays))
- `{string: V}` — a map from string keys to values of any type `V`, including
  arrays and maps (`{string: [int]}`) (see [Maps](#maps) and
  [Nested collections](#nested-collections))
- a user-defined `struct` (see [Structs](#structs))

There are **no implicit conversions** between types. An `int` is never
automatically turned into a `float`, a `bool` is never treated as a number, and
so on. Every operation requires operands of the expected type. To convert
between numbers explicitly, use the [`int()` / `float()` built-ins](#conversions).

`string` values are immutable (you cannot modify one in place), but you can
build new ones: `+` **concatenates** two strings into a fresh heap string, and
all comparisons (`== != < <= > >=`) work on strings — `==`/`!=` by value and the
ordering operators lexicographically (by byte). Concatenated strings are
heap-allocated and currently reclaimed only at program exit; see
[RFC 0001](rfcs/0001-memory-model.md) for the planned memory management.

You can read individual bytes with `s[i]`, which returns the i-th byte as an
`int` (0–255), bounds-checked like an array. `len(s)` gives the byte length.
Because strings are immutable, `s[i] = ...` is a compile error. Together with
[`str`](#str) (build strings from values) this is enough for simple text
processing:

```lumo
# (To actually parse a number, use the built-in int(s) / float(s); this just
# illustrates byte indexing.)
fn digit_sum(s: string) -> int {
    let total = 0;
    for (let i = 0; i < len(s); i = i + 1) {
        total = total + (s[i] - 48);   # '0' is byte 48
    }
    return total;
}
print digit_sum("2026");   # 10
```

## Literals

| Type     | Examples                 |
| -------- | ------------------------ |
| `int`    | `42`, `0`                |
| `float`  | `3.14`, `0.5`, `2.0`     |
| `bool`   | `true`, `false`          |
| `string` | `"hi"`, `"line\n"`, `""` |

Float literals must have digits on **both** sides of the dot. `1.` and `.5` are
not valid; write `1.0` and `0.5` instead.

String literals are enclosed in double quotes and support the escapes `\n`,
`\t`, `\r`, `\\`, `\"`, and `\0`. A string literal may not span multiple lines.

Comments start with `#` and run to the end of the line:

```lumo
let x = 1;  # this is a comment
```

## Variables

Declare a variable with `let`. Its type is inferred from the initializer:

```lumo
let count = 0;       # int
let ratio = 0.5;     # float
let ready = true;    # bool
```

Reassign an existing variable with `=`. The new value must have the same type as
the variable:

```lumo
let n = 1;
n = n + 1;   # ok: int = int
# n = 2.0;   # error: cannot assign float to an int variable
```

### Scope

Variables are **lexically block-scoped**: a `let` is visible only until the end
of the enclosing `{ ... }` block (the body of a function, `if`/`else`, `while`,
or `for`). A `for`'s init variable is scoped to the loop. Using a name outside
its block is an error.

A `let` may **shadow** a variable from an outer scope; the outer binding is
restored when the inner block ends:

```lumo
let x = 1;
if (true) {
    let x = 2;   # a new variable that shadows the outer x
    print x;     # 2
}
print x;         # 1
```

## Operators

### Arithmetic: `+ - * /`

Operate on two `int`s or two `float`s; both operands must be the same type, and
the result has that same type. Unary minus `-x` negates an `int` or a `float`.
Additionally, `+` **concatenates two `string`s**.

```lumo
let a = 3 + 4;             # int
let b = 1.5 * 2.0;         # float
let c = -a;                # int
let s = "foo" + "bar";     # string -> "foobar"
```

### Modulo: `%`

Defined for `int` only.

```lumo
let r = 10 % 3;      # 1
```

Integer `/` and `%` by zero abort at runtime with `lumo: division by zero`
(exit 101). Float division by zero follows IEEE rules (`1.0 / 0.0` is infinity),
not an error.

### Comparison: `== != < <= > >=`

Operate on two `int`s or two `float`s of the same type and produce a `bool`.
They also compare two `string`s: `==`/`!=` by value, and `<`/`<=`/`>`/`>=`
lexicographically (so an array of strings can be sorted).

```lumo
let same = (a == 7);     # bool
let bigger = 2.0 > 1.0;  # bool
```

### Logical: `&& || !`

Operate on `bool` values. `&&` and `||` short-circuit; `!` negates.

```lumo
let ok = ready && (count > 0);
let any = ready || bigger;
let no = !ready;
```

### Type rules

Mixing types in an operator is a type error. For example, `1 + 2.0`,
`true + 1`, and `1 < 2.0` are all rejected.

### Precedence

From lowest to highest binding:

```
||  <  &&  <  comparison  <  + -  <  * / %  <  unary (-, !)  <  primary
```

`primary` covers literals, variables, function calls, and parenthesized
expressions. Use parentheses to override the default precedence.

## Control Flow

### `if` / `else`

The condition must be a `bool`. The `else` branch is optional.

```lumo
if (count > 0) {
    print count;
} else {
    print 0;
}
```

### `while`

Repeats the body while the condition (a `bool`) holds.

```lumo
let i = 0;
while (i < 5) {
    print i;
    i = i + 1;
}
```

### `for`

`for (init; cond; step) { ... }`. The `init` and `step` clauses are optional;
`cond` is a `bool`. `init` runs once before the loop; `step` runs after each
iteration. (`init` may declare a variable with `let`.)

```lumo
for (let i = 0; i < 5; i = i + 1) {
    print i;
}
```

### `for`-`in`

`for (x in collection) { ... }` iterates a collection, binding `x` to each item:

- over an **array** `[T]`, `x` is each element (type `T`);
- over a **map** `{string: V}`, `x` is each **key** (a `string`) — look the value
  up with `m[x]`. The order is unspecified.

The loop variable is scoped to the loop. `break` and `continue` work as usual.
(The array's length is read once, at the start of the loop.)

```lumo
let xs = [10, 20, 30];
let sum = 0;
for (v in xs) { sum = sum + v; }      # 60

let counts = {"a": 2, "b": 5};
for (k in counts) {
    print k + " = " + str(counts[k]);
}
```

### `break` and `continue`

`break` leaves the nearest enclosing loop. `continue` skips to the next
iteration — the `step` clause of a `for`, or the condition of a `while`. Using
either outside a loop is an error.

```lumo
for (let i = 0; i < 100; i = i + 1) {
    if (i == 10) { break; }
    if (i % 2 == 0) { continue; }
    print i;   # odd numbers below 10
}
```

## Functions

```lumo
fn name(p: int, q: bool) -> float {
    # ...
}
```

- Every parameter must declare its type.
- The return type is optional and defaults to `int`. So `fn main() { ... }`
  returns an `int`.
- `int`, `bool`, `float`, `string`, and arrays may all be used as parameter and
  return types.
- `return expr;` returns a value, whose type must match the declared return
  type.
- Recursion and mutual recursion are supported.

```lumo
fn even(n: int) -> bool {
    if (n == 0) { return true; }
    return odd(n - 1);
}

fn odd(n: int) -> bool {
    if (n == 0) { return false; }
    return even(n - 1);
}
```

## Arrays

An array type is written `[T]` where `T` is any type: a scalar (`int`, `bool`,
`float`, `string`), a struct (e.g. `[Point]`), or **another collection** —
arrays nest (`[[int]]`, a matrix) and can hold maps (`[{string: int}]`). See
[Nested collections](#nested-collections).

- **Literal:** `[e1, e2, ...]` — elements all the same type; the element type is
  inferred. The **empty literal `[]`** is allowed only when a `let` gives the
  array type, e.g. `let xs: [int] = [];` — that's how you start a growable array.
- **Index:** `a[i]` reads, `a[i] = v;` writes. The index is an `int`.
- **Slice:** `a[lo:hi]` returns a new array with elements `lo..hi` (see
  [Slicing](#slicing)).
- **Length:** `len(a)` returns the number of elements as an `int`.
- **Grow:** `push(a, x)` appends `x` and returns the array (see below).
- **Shrink:** `pop(a)` removes the last element and returns it (see below).

```lumo
let xs = [10, 20, 30];
xs[1] = 99;
print xs[1];        # 99
print len(xs);      # 3

fn sum(ns: [int]) -> int {
    let total = 0;
    for (let i = 0; i < len(ns); i = i + 1) {
        total = total + ns[i];
    }
    return total;
}
```

### Growable arrays: `push`

`push(a, x)` appends `x` to array `a` (the types must match) and returns the
array, so the common pattern is `a = push(a, x);`. Arrays grow automatically —
start from `[]` and push as many elements as you like, which is what you need to
collect an unknown number of inputs:

```lumo
let xs: [int] = [];
for (let i = 0; i < 5; i = i + 1) {
    xs = push(xs, i * i);
}
print len(xs);      # 5  -> [0, 1, 4, 9, 16]
```

Under the hood an array is a small header `{len, cap, data}`; `push` doubles the
`data` block (via `realloc`) when it fills up. Because the header is what an
array value points to, growth stays visible through every alias of the array —
`let b = a; a = push(a, x);` leaves `b` seeing the new element too.

Arrays are heap-allocated and, like concatenated strings, are currently
reclaimed only at program exit — see [RFC 0001](rfcs/0001-memory-model.md).
Indexing is **bounds-checked**: an out-of-range index (including a negative one)
prints `lumo: index out of bounds` to stderr and exits with status 101.

### Shrinking arrays: `pop`

`pop(a)` removes the **last** element of `a` and returns it (typed as the element
type), shrinking `a` in place — no reassignment needed. Together with `push` this
makes an array a stack:

```lumo
let stack: [int] = [];
stack = push(stack, 1);
stack = push(stack, 2);
print pop(stack);   # 2
print pop(stack);   # 1
print len(stack);   # 0
```

`pop` on an **empty** array aborts at runtime (`lumo: pop from empty array`,
exit 101) — check `len(a) > 0` first if it might be empty.

### Slicing

`seq[lo:hi]` returns the half-open range `lo..hi` of an array or string — for an
array a **new array** (an independent copy), for a string a **new string**. Both
bounds are optional: `lo` defaults to `0` and `hi` to the length.

```lumo
let a = [10, 20, 30, 40, 50];
print a[1:3];     # (a new [20, 30])
print a[:2];      # [10, 20]
print a[3:];      # [40, 50]
print a[:];       # a full copy

print "hello"[1:4];   # ell
print "hello"[2:];    # llo
```

Slicing is **bounds-checked like `substr`**: it requires `0 <= lo <= hi <= len`,
and otherwise aborts (`lumo: slice out of range`, exit 101). An array slice is a
copy, so mutating it does not affect the original. (A string slice is exactly
`substr(s, lo, hi - lo)`.)

## Maps

A map (associative array) type is written `{string: V}`. **Keys are always
`string`**; the value type `V` may be any type — a scalar (`int`, `bool`,
`float`, `string`), a struct, or **another collection**, so maps can hold arrays
(`{string: [int]}`) or nest (`{string: {string: int}}`). See
[Nested collections](#nested-collections) and [RFC 0002](rfcs/0002-map-type.md).

- **Literal:** `{"a": 1, "b": 2}` — keys are string expressions, values all the
  same type (inferred). The **empty literal `{}`** needs a map annotation, like
  `[]`: `let m: {string: int} = {};`.
- **Read:** `m[k]` returns the value for key `k`. **A missing key aborts** at
  runtime (`lumo: key not found`, exit 101) — guard with `has` first.
- **Write:** `m[k] = v;` inserts or overwrites (mutates in place; visible through
  aliases, since a map value points at a stable header).
- **`has(m, k)`** → `bool`; **`len(m)`** → number of entries; **`delete(m, k)`**
  removes a key if present; **`keys(m)`** → a fresh `[string]` of the keys in
  **unspecified order**.

```lumo
# count word frequencies
let counts: {string: int} = {};
let words = ["a", "b", "a"];
for (let i = 0; i < len(words); i = i + 1) {
    let w = words[i];
    if (has(counts, w)) { counts[w] = counts[w] + 1; } else { counts[w] = 1; }
}
print counts["a"];          # 2
print len(counts);          # 2

let ks = keys(counts);      # ["a", "b"] in some order
```

Maps are backed by a separately-chained hash table (FNV-1a hashing) that grows
automatically — when the load factor exceeds 0.75 the bucket array is rehashed
into one twice as large, so lookups stay near O(1) as the map fills. Entries —
like all heap data — are reclaimed only at program exit (RFC 0001).

## Nested collections

Arrays and maps can hold **other arrays and maps** as their element/value type,
to any depth — the element type `[T]` and value type `{string: V}` accept a
collection just as readily as a scalar:

| Type | What it is |
| --- | --- |
| `[[int]]` | a 2D array (matrix) — a list of `[int]` rows |
| `{string: [int]}` | a map from a key to a list (group-by) |
| `[{string: int}]` | a list of records |
| `{string: {string: int}}` | a nested map (e.g. table → row → value) |

Indexing **chains**: `grid[i][j]` indexes the row `grid[i]` and then column `j`,
and assignment works at any depth because the inner collection is a reference:

```lumo
let grid = [[1, 2, 3], [4, 5, 6]];
print grid[1][2];           # 6
grid[1][0] = 40;            # mutates the inner row in place
print grid[1][0];           # 40

# build a ragged matrix with push
let rows: [[int]] = [];
for (let i = 0; i < 3; i = i + 1) {
    let row: [int] = [];
    for (let j = 0; j <= i; j = j + 1) { row = push(row, j); }
    rows = push(rows, row);
}
print len(rows[2]);         # 3

# map of arrays (group-by) and a map of maps
let groups: {string: [int]} = {};
groups["even"] = [2, 4, 6];
print groups["even"][1];    # 4

let table: {string: {string: int}} = {};
table["a"] = {"score": 10};
table["a"]["score"] = 15;
print table["a"]["score"];  # 15
```

Because every collection value is a pointer, nesting costs nothing extra in the
value model — `[[int]]` stores each row as a pointer in the outer array's slots.
The empty-literal rule still applies at each level: `let rows: [[int]] = [];`
needs the annotation, and `null` cannot be an element or value type (its type
can't be inferred). See [`examples/matrix.lum`](../examples/matrix.lum) for
matrix multiplication built on `[[int]]`.

## Structs

Declare a struct at the top level (alongside functions):

```lumo
struct Point {
    x: int,
    y: int,
}
```

- **Construct:** `Point { x: 3, y: 4 }` — every field must be given exactly once
  (field order doesn't matter), with a matching type.
- **Access:** `p.x` reads a field, `p.x = v;` writes one.
- A struct may be a parameter type, return type, or another struct's field type
  (struct fields are references, so structs can nest).

```lumo
fn dist_sq(p: Point) -> int {
    return p.x * p.x + p.y * p.y;
}

struct Rect { lo: Point, hi: Point }   # structs can contain structs
```

Structs are heap-allocated and assigned by reference (two variables bound to the
same struct value alias the same data). Like other heap values they are
reclaimed only at program exit; see [RFC 0001](rfcs/0001-memory-model.md). An
array of structs (`[Point]`) is allowed; indexing yields the struct, so
`ps[i].x` reads and `ps[i].x = v;` mutates it in place.

## null

`null` is a value compatible with any **reference type** (`string`, an array, or
a struct). It lets reference variables be "empty" — which, combined with a
self-referential struct, gives recursive data structures like linked lists and
trees.

- Assign or pass `null` wherever a reference type is expected.
- Compare with `==` / `!=`: `node == null`, `s != null`.
- A bare `let x = null;` can't infer a type — give one: `let x: Node = null;`.
- Reading a field or index through `null` is caught at runtime: it prints
  `lumo: null reference` to stderr and exits with status 101.

```lumo
struct Node { val: int, next: Node }

fn length(list: Node) -> int {
    if (list == null) { return 0; }
    return 1 + length(list.next);
}

let list = Node { val: 1, next: Node { val: 2, next: null } };
print length(list);   # 2
```

### Variable type annotations

A `let` may carry an explicit type — required when the initializer is `null`,
optional otherwise:

```lumo
let prev: Node = null;
let n: int = 0;
```

Execution starts at `fn main()`. The value `main` returns becomes the process
exit code.

## Built-ins

### `print`

Prints an `int`, `bool`, `float`, or `string`, followed by a newline. Booleans
print as `true` or `false`.

```lumo
print 42;       # 42
print true;     # true
print 3.14;     # 3.14
print "hi";     # hi
```

### Conversions and parsing

`int(x)` and `float(x)` convert between the two numeric types — the only way to
mix `int` and `float`, since there are no implicit conversions — **or parse a
`string`** into a number.

- `float(i)` widens an `int` to a `float`; `int(f)` truncates a `float` toward
  zero. `int(int)` / `float(float)` are no-ops.
- `int(s)` / `float(s)` parse a `string` (the whole string must be a valid
  number, optional sign and — for `float` — decimal/exponent). An unparseable
  string aborts at runtime (`lumo: int() got a non-integer string`).
- **`is_int(s)`** and **`is_float(s)`** return whether a `string` parses, so you
  can check before converting (the same guard pattern as `has` for maps).

```lumo
print float(7) / float(2);   # 3.5   (numeric conversion)
print int(3.9);              # 3     (truncates)
print int("42") + 1;         # 43    (parse)

let s = "100";
if (is_int(s)) {
    print int(s) * 2;        # 200
}
```

`int`, `float`, `bool`, `string`, `len`, `str`, `chr`, `read_line`, `push`, `pop`,
`sqrt`, `pow`, `abs`, `min`, `max`, `floor`, `ceil`, `has`, `keys`, `delete`,
`substr`, `split`, `join`, `is_int`, `is_float`, `read_file`, `write_file`,
`to_upper`, `to_lower`, `trim`, `find`, `contains`, `replace`, and `repeat` are
reserved names — you cannot define a function with one of them.

### `read_line`

`read_line()` reads the next line from standard input and returns it as a
`string` with the trailing newline removed, or `null` at end of input. The idiom
is to loop until `null`:

```lumo
let line = read_line();
while (line != null) {
    print line;            # echo stdin
    line = read_line();
}
```

(Lines longer than 4095 bytes come back in chunks.)

### `read_file` / `write_file`

Whole-file I/O for persisting state between runs:

- **`write_file(path, content)`** writes the `string` `content` to the file at
  `path`, **replacing** any existing contents, and returns a `bool` — `true` on
  success, `false` if the file could not be opened or the write was short.
- **`read_file(path)`** returns the file's entire contents as a `string`, or
  `null` if it cannot be opened. As with `read_line`, guard the result against
  `null` before using it.

```lumo
if (write_file("/tmp/notes.txt", "hello\nworld\n")) {
    let text = read_file("/tmp/notes.txt");
    if (text != null) {
        for (line in split(text, "\n")) {
            print line;          # hello / world / (trailing empty)
        }
    }
}
```

Reading splits naturally with [`split`](#substr--split--join), and writing pairs
with [`join`](#substr--split--join) to round-trip line-oriented data — see
[`examples/save_load.lum`](../examples/save_load.lum). Files are read and written
as raw bytes; there is no text encoding or buffering beyond the C library's.

### `chr`

`chr(b)` returns a one-character `string` for the byte value `b` (an `int`,
taken modulo 256). It is the inverse of [string indexing](#types): with `s[i]`
to read bytes and `+` to concatenate, you can transform strings.

```lumo
print chr(65);   # A

fn upper(s: string) -> string {
    let r = "";
    for (let i = 0; i < len(s); i = i + 1) {
        let c = s[i];
        if (c >= 97 && c <= 122) { c = c - 32; }
        r = r + chr(c);
    }
    return r;
}
print upper("hi");   # HI
```

### `substr` / `split` / `join`

The string toolkit for slicing and parsing text:

- **`substr(s, start, count)`** → the substring of `count` bytes starting at byte
  `start`. Aborts at runtime (`lumo: substr out of range`) if `start`/`count` are
  negative or run past the end.
- **`split(s, sep)`** → a `[string]` of the pieces of `s` separated by `sep`.
  Consecutive or edge separators produce empty pieces (`split(",a,", ",")` is
  `["", "a", ""]`); an empty `sep` returns `[s]`.
- **`join(parts, sep)`** → a `string`: the inverse of `split`, concatenating a
  `[string]` with `sep` between elements.

```lumo
print substr("hello world", 6, 5);     # world

let fields = split("a,b,c", ",");      # ["a", "b", "c"]
for (f in fields) { print f; }

print join(fields, " | ");             # a | b | c
print join(split("x-y-z", "-"), "+");  # x+y+z  (round trip)
```

To turn a parsed field into a number, use `int(s)` / `float(s)` (guarded by
`is_int` / `is_float`) — see [Conversions and parsing](#conversions-and-parsing).

### `to_upper` / `to_lower` / `trim` / `find` / `contains` / `replace` / `repeat`

String methods for normalizing, searching, and rewriting text:

- **`to_upper(s)`** / **`to_lower(s)`** → a new `string` with ASCII letters
  upper/lower-cased; every other byte is passed through unchanged.
- **`trim(s)`** → a new `string` with leading and trailing ASCII whitespace
  (space, tab, `\n`, `\r`) removed. Interior whitespace is left alone.
- **`find(s, sub)`** → the byte index of the first occurrence of `sub` in `s`, or
  `-1` if it does not occur. An empty `sub` returns `0`.
- **`contains(s, sub)`** → a `bool`, true when `sub` occurs in `s` (i.e.
  `find(s, sub) >= 0`).
- **`replace(s, from, to)`** → a new `string` with every occurrence of `from`
  replaced by `to`. `from` may be longer or shorter than `to` (the result grows
  or shrinks); an empty `from` returns `s` unchanged. (It is exactly
  `join(split(s, from), to)`.)
- **`repeat(s, n)`** → a new `string` of `s` concatenated `n` times; `n <= 0`
  gives the empty string.

```lumo
print to_upper("Hello");          # HELLO
print trim("  hi  ");             # hi   (no surrounding spaces)
print find("hello", "ll");        # 2
print contains("hello", "ell");   # true
print replace("a.b.c", ".", "/"); # a/b/c
print repeat("=", 10);            # ==========

# normalize input before comparing
let answer = to_lower(trim(read_line()));
if (answer == "yes") { print "confirmed"; }
```

Like `+` concatenation and `substr`, these all return freshly allocated strings —
the input is never modified.

### `len`

`len(x)` returns, as an `int`, the length of a `string` (its number of bytes),
an array (its number of elements), or a map (its number of entries).

```lumo
print len("hello");     # 5
print len([1, 2, 3]);   # 3
```

### `push`

`push(a, x)` appends `x` (whose type must match the array's element type) to
array `a` and returns the array. Reassign the result — `a = push(a, x);` — and
arrays grow on demand, so you can build one up from `[]`. See
[Growable arrays](#growable-arrays-push).

```lumo
let xs: [int] = [];
xs = push(xs, 1);
xs = push(xs, 2);
print len(xs);          # 2
```

### `has` / `delete` / `keys`

Map operations: `has(m, k)` tests membership (`bool`), `delete(m, k)` removes a
key if present, and `keys(m)` returns the keys as a `[string]` (unspecified
order). See [Maps](#maps).

### `str`

`str(x)` converts an `int`, `float`, or `bool` to a `string` (a `string` passes
through unchanged). Combined with `+`, it lets you build text from values:

```lumo
let n = 42;
print "answer = " + str(n);   # answer = 42
print str(3.14);              # 3.14
print str(true);              # true
```

### Math

A handful of numeric built-ins:

| Function     | Signature                  | Notes                                  |
| ------------ | -------------------------- | -------------------------------------- |
| `sqrt(x)`    | `float -> float`           | square root                            |
| `pow(b, e)`  | `(float, float) -> float`  | `b` raised to the power `e`            |
| `floor(x)`   | `float -> float`           | round down to a whole number           |
| `ceil(x)`    | `float -> float`           | round up to a whole number             |
| `abs(x)`     | `int -> int` / `float -> float` | absolute value (keeps the type)   |
| `min(a, b)`  | numeric, both same type    | smaller of two `int`s or two `float`s  |
| `max(a, b)`  | numeric, both same type    | larger of two `int`s or two `float`s   |

```lumo
print sqrt(2.0);                       # 1.41421
print pow(2.0, 10.0);                  # 1024
print abs(-7);                         # 7
print max(min(5, 10), 3);             # 5

# distance between two points
let dx = 3.0;
let dy = 4.0;
print sqrt(pow(dx, 2.0) + pow(dy, 2.0));   # 5
```

## Diagnostics

Errors are reported with an error code, a source location, and a caret
underline pointing at the offending span. Error codes are grouped by phase:

| Range   | Phase             | Examples                                                        |
| ------- | ----------------- | -------------------------------------------------------------- |
| `E000x` | Lexing            | invalid characters or malformed tokens                         |
| `E002`  | Parsing           | syntax errors                                                  |
| `E01xx` | Names / arity     | undefined variable or function, wrong arity, duplicate function, missing `main` |
| `E02xx` | Types             | type mismatch, non-`bool` condition, returning the wrong type   |
| `E03xx` | Type annotations  | unknown type name, duplicate parameter                          |

## CLI

| Command                      | Description                          |
| ---------------------------- | ------------------------------------ |
| `lumo run <file.lum>`        | JIT-compile and run immediately      |
| `lumo build <file.lum>`      | Produce a native executable          |
| `lumo emit-ir <file.lum>`    | Print the generated LLVM IR          |

## Grammar (informal)

The following EBNF-ish sketch outlines the syntax. `{ X }` means zero or more
repetitions and `[ X ]` means optional.

```ebnf
program     = { struct_def | function } ;

struct_def  = "struct" ident "{" [ param { "," param } [ "," ] ] "}" ;
function    = "fn" ident "(" [ params ] ")" [ "->" type ] block ;
params      = param { "," param } ;
param       = ident ":" type ;
type        = "int" | "bool" | "float" | "string" | "[" type "]" | ident ;

block       = "{" { statement } "}" ;

statement   = let_stmt
            | assign_stmt
            | return_stmt
            | print_stmt
            | if_stmt
            | while_stmt
            | for_stmt
            | forin_stmt
            | break_stmt
            | continue_stmt
            | expr_stmt ;

let_stmt    = "let" ident [ ":" type ] "=" expr ";" ;
assign_stmt = lvalue "=" expr ";" ;   (* lvalue is a variable or array index *)
return_stmt = "return" expr ";" ;
print_stmt  = "print" expr ";" ;
if_stmt     = "if" "(" expr ")" block [ "else" block ] ;
while_stmt  = "while" "(" expr ")" block ;
for_stmt    = "for" "(" [ simple ] ";" expr ";" [ simple ] ")" block ;
forin_stmt  = "for" "(" ident "in" expr ")" block ;
break_stmt  = "break" ";" ;
continue_stmt = "continue" ";" ;
expr_stmt   = expr ";" ;

(* a statement without a trailing semicolon, used in for-clauses *)
simple      = "let" ident "=" expr | ident "=" expr | expr ;

(* Expressions, lowest to highest precedence. *)
expr        = or_expr ;
or_expr     = and_expr { "||" and_expr } ;
and_expr    = cmp_expr { "&&" cmp_expr } ;
cmp_expr    = add_expr { ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) add_expr } ;
add_expr    = mul_expr { ( "+" | "-" ) mul_expr } ;
mul_expr    = unary    { ( "*" | "/" | "%" ) unary } ;
unary       = ( "-" | "!" ) unary | postfix ;
postfix     = primary { "[" expr "]" | "." ident } ;   (* indexing / field access *)
array_lit   = "[" [ expr { "," expr } ] "]" ;
struct_lit  = ident "{" [ field_init { "," field_init } [ "," ] ] "}" ;
field_init  = ident ":" expr ;
primary     = int_lit
            | float_lit
            | bool_lit
            | null_lit
            | str_lit
            | array_lit
            | struct_lit
            | ident
            | call
            | "(" expr ")" ;
call        = ident "(" [ args ] ")" ;
args        = expr { "," expr } ;

int_lit     = digit { digit } ;
float_lit   = digit { digit } "." digit { digit } ;
bool_lit    = "true" | "false" ;
null_lit    = "null" ;
str_lit     = '"' { char | escape } '"' ;
ident       = letter { letter | digit | "_" } ;
```
