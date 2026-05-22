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

Lumo has three primitive types:

- `int` — 64-bit signed integer
- `bool` — either `true` or `false`
- `float` — 64-bit IEEE double

There are **no implicit conversions** between types. An `int` is never
automatically turned into a `float`, a `bool` is never treated as a number, and
so on. Every operation requires operands of the expected type.

## Literals

| Type    | Examples            |
| ------- | ------------------- |
| `int`   | `42`, `0`           |
| `float` | `3.14`, `0.5`, `2.0`|
| `bool`  | `true`, `false`     |

Float literals must have digits on **both** sides of the dot. `1.` and `.5` are
not valid; write `1.0` and `0.5` instead.

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

## Operators

### Arithmetic: `+ - * /`

Operate on two `int`s or two `float`s; both operands must be the same type, and
the result has that same type. Unary minus `-x` negates an `int` or a `float`.

```lumo
let a = 3 + 4;       # int
let b = 1.5 * 2.0;   # float
let c = -a;          # int
```

### Modulo: `%`

Defined for `int` only.

```lumo
let r = 10 % 3;      # 1
```

### Comparison: `== != < <= > >=`

Operate on two `int`s or two `float`s of the same type and produce a `bool`.

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

## Functions

```lumo
fn name(p: int, q: bool) -> float {
    # ...
}
```

- Every parameter must declare its type.
- The return type is optional and defaults to `int`. So `fn main() { ... }`
  returns an `int`.
- `int`, `bool`, and `float` may all be used as parameter and return types.
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

Execution starts at `fn main()`. The value `main` returns becomes the process
exit code.

## Built-ins

### `print`

Prints an `int`, `bool`, or `float`, followed by a newline. Booleans print as
`true` or `false`.

```lumo
print 42;       # 42
print true;     # true
print 3.14;     # 3.14
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
program     = { function } ;

function    = "fn" ident "(" [ params ] ")" [ "->" type ] block ;
params      = param { "," param } ;
param       = ident ":" type ;
type        = "int" | "bool" | "float" ;

block       = "{" { statement } "}" ;

statement   = let_stmt
            | assign_stmt
            | return_stmt
            | print_stmt
            | if_stmt
            | while_stmt
            | expr_stmt ;

let_stmt    = "let" ident "=" expr ";" ;
assign_stmt = ident "=" expr ";" ;
return_stmt = "return" expr ";" ;
print_stmt  = "print" expr ";" ;
if_stmt     = "if" "(" expr ")" block [ "else" block ] ;
while_stmt  = "while" "(" expr ")" block ;
expr_stmt   = expr ";" ;

(* Expressions, lowest to highest precedence. *)
expr        = or_expr ;
or_expr     = and_expr { "||" and_expr } ;
and_expr    = cmp_expr { "&&" cmp_expr } ;
cmp_expr    = add_expr { ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) add_expr } ;
add_expr    = mul_expr { ( "+" | "-" ) mul_expr } ;
mul_expr    = unary    { ( "*" | "/" | "%" ) unary } ;
unary       = ( "-" | "!" ) unary | primary ;
primary     = int_lit
            | float_lit
            | bool_lit
            | ident
            | call
            | "(" expr ")" ;
call        = ident "(" [ args ] ")" ;
args        = expr { "," expr } ;

int_lit     = digit { digit } ;
float_lit   = digit { digit } "." digit { digit } ;
bool_lit    = "true" | "false" ;
ident       = letter { letter | digit | "_" } ;
```
