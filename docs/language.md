# The Lumo Language

Lumo is a small, statically typed programming language compiled with LLVM. It
has three primitive types, simple control flow, first-class functions, and no
implicit conversions. This document describes the language as it currently
stands.

## A Complete Example

```lumo
# Approximate a square root with a fixed number of iterations.
fn sqrt(x: float, steps: int) -> float {
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
        print sqrt(n, 10);
    } else {
        print 0.0;
    }
    return 0;
}
```

Running this with `lumo run example.lum` prints the approximation and exits with
code `0` (the value returned by `main`).

## Types

Lumo has four primitive types and two compound types:

- `int` — 64-bit signed integer
- `bool` — either `true` or `false`
- `float` — 64-bit IEEE double
- `string` — an immutable text value
- `[T]` — an array of `T`, where `T` is `int`, `bool`, `float`, or `string`
  (see [Arrays](#arrays))
- a user-defined `struct` (see [Structs](#structs))

There are **no implicit conversions** between types. An `int` is never
automatically turned into a `float`, a `bool` is never treated as a number, and
so on. Every operation requires operands of the expected type. To convert
between numbers explicitly, use the [`int()` / `float()` built-ins](#conversions).

`string` values are immutable (you cannot modify one in place), but you can
build new ones: `+` **concatenates** two strings into a fresh heap string, and
`==` / `!=` compare strings by value. Ordering comparisons (`<`, `<=`, `>`,
`>=`) are not defined for strings. Concatenated strings are heap-allocated and
currently reclaimed only at program exit; see
[RFC 0001](rfcs/0001-memory-model.md) for the planned memory management.

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

### Comparison: `== != < <= > >=`

Operate on two `int`s or two `float`s of the same type and produce a `bool`.
`==` and `!=` also compare two `string`s by value; ordering (`<`, `<=`, `>`,
`>=`) is not defined for strings.

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

An array type is written `[T]` where `T` is a scalar (`int`, `bool`, `float`, or
`string`). Arrays of arrays are not supported yet.

- **Literal:** `[e1, e2, ...]` — at least one element, all the same type. The
  element type is inferred. (There is no empty-array literal yet.)
- **Index:** `a[i]` reads, `a[i] = v;` writes. The index is an `int`.
- **Length:** `len(a)` returns the number of elements as an `int`.

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

Arrays are heap-allocated (laid out as a length followed by the elements) and,
like concatenated strings, are currently reclaimed only at program exit — see
[RFC 0001](rfcs/0001-memory-model.md). Indexing is **bounds-checked**: an
out-of-range index (including a negative one) prints `lumo: array index out of
bounds` to stderr and exits with status 101.

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
reclaimed only at program exit; see [RFC 0001](rfcs/0001-memory-model.md).
Arrays of structs are not supported yet.

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

### Conversions

`int(x)` and `float(x)` convert between the two numeric types — the only way to
mix `int` and `float`, since there are no implicit conversions.

- `float(i)` widens an `int` to a `float`.
- `int(f)` truncates a `float` toward zero to an `int`.
- Each accepts a numeric argument; `int(int)` and `float(float)` are no-ops.

```lumo
let total = 7;
let count = 2;
print float(total) / float(count);  # 3.5  (float division)
print int(3.9);                     # 3    (truncates)
```

`int`, `float`, `bool`, `string`, and `len` are reserved names — you cannot
define a function with one of them.

### `len`

`len(x)` returns, as an `int`, the length of a `string` (its number of bytes) or
an array (its number of elements).

```lumo
print len("hello");     # 5
print len([1, 2, 3]);   # 3
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
