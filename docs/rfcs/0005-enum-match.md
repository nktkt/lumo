# RFC 0005 — Sum types (`enum`) and pattern matching (`match`)

- **Status:** Implemented (v0.45.0).
- **Author:** Lumo contributors
- **Created:** 2026-05-24
- **Targets roadmap phase:** Phase 3/7 — type system (sum types are the missing
  half of the algebraic-types story; structs are the product half).
- **Tracking issue:** _TBD_
- **Depends on:** structs (`v0.13`, the heap-aggregate machinery this reuses) and
  the GC (`v0.44`, so enum payloads are reclaimed like any heap value).

> **Implemented in v0.45.0**, with two refinements to the surface below: the
> `match` scrutinee is **parenthesized** — `match (x) { ... }`, consistent with
> `if`/`while`/`for`, which also resolves the `match x { ... }`-vs-struct-literal
> parse ambiguity — and an arm body may be **either a single statement or a
> `{ ... }` block** (`Variant(a) => { ...; return v; }`). Variant construction
> reuses identifier/call syntax and is resolved in the type checker, so no new
> expression form was needed. Everything else (global variant names, positional
> payloads, `match`-as-statement, exhaustiveness, `{ i64 tag, slot×maxArity }`
> representation lowered to an LLVM `switch`) shipped as designed.

> **This is a request for comments, not a settled decision.** Lumo has *product*
> types (`struct`: "an `x` **and** a `y`") but no *sum* types ("an `int` **or** a
> `string`"). That gap forces error handling into `panic`/sentinels and makes
> `Option`/`Result`, variant trees, and state machines inexpressible. This RFC
> designs `enum` + `match`. They touch every layer, so settling the surface and
> representation first matters.

---

## Summary

Add **`enum`** (tagged unions) and **`match`** (pattern matching with
exhaustiveness checking):

```lumo
enum Shape {
    Circle(float),           # radius
    Rect(float, float),      # width, height
    Unit,                    # no payload
}

fn area(s: Shape) -> float {
    match s {
        Circle(r) => return 3.14159 * r * r;
        Rect(w, h) => return w * h;
        Unit => return 1.0;
    }
}

fn main() {
    print area(Circle(2.0));     # 12.56636
    print area(Rect(3.0, 4.0));  # 12
    return 0;
}
```

The headline choices are **(1) how variants are named** (bare vs. qualified) and
**(2) `match` as a statement vs. an expression**. v1 proposes **bare,
globally-unique variant names** and **`match` as a statement**, both for the
smallest surface that is forward-compatible.

---

## Motivation

Today, "one of several shapes of data" has no good encoding:

- **Error handling** is `panic` (fatal) or sentinels (`-1`, `null`, `""`) that
  callers must remember to check and that can't carry a message. A `Result`-style
  `enum Parsed { Ok(int), Err(string) }` makes failure a value you must handle.
- **Optional values** beyond references: `null` only works for reference types,
  so "maybe an `int`" needs a sentinel. `enum OptInt { Some(int), None }` is
  honest.
- **Variant data** — an expression AST, a JSON value, a token, a state machine —
  is exactly a sum of products, the natural shape `enum` + `struct` give.

`match` then makes consuming such data **exhaustive**: the compiler rejects code
that forgets a case — squarely on-brand with "errors are a feature."

---

## Guiding constraints / requirements

- **R1 — Reuse the heap-aggregate machinery.** Enums should lower like structs (a
  GC-allocated object) and reuse the universal 8-byte-slot value model, not invent
  a new representation.
- **R2 — Exhaustiveness is the point.** A `match` that omits a variant (without a
  `_`) is a compile error; that safety is the main reason to add `match`.
- **R3 — Small, additive surface.** Two keywords (`enum`, `match`) and a pattern
  grammar; everything else (construction, the value model, the GC) is existing
  machinery. No change to how `struct`/`null` work.
- **R4 — Forward-compatible.** v1's choices must not foreclose generics
  (`enum Option<T>`), `match` as an expression, or richer patterns (literals,
  guards, nested) later.

---

## Detailed design

### Axis 1 — Declaration syntax

```lumo
enum Name {
    VariantA,                 # no payload
    VariantB(int),            # one positional field
    VariantC(string, int),    # several positional fields
}
```

Positional payloads (not named fields) in v1 — simplest, and `match` binds them
positionally. Named-field variants (`Move { x: int, y: int }`) are a future
extension. Payload types may be any existing type (scalars, strings, arrays,
maps, structs, other enums), stored as 8-byte slots like struct fields (R1).

### Axis 2 — Variant naming (the first key choice)

| Option | Construct | Match | Notes |
| --- | --- | --- | --- |
| **A2a — bare, globally unique** *(recommend v1)* | `Circle(2.0)` | `Circle(r) =>` | Variant names must be unique across all enums (like Lumo's already-global `fn`/`struct` names). Shortest; resolved by a global variant→enum table. |
| A2b — qualified | `Shape.Circle(2.0)` | `Shape.Circle(r) =>` | No global collisions, but verbose and needs `Type.Variant` parsing (overlaps field access). |

**Recommend A2a.** It is the least syntax and reuses existing forms entirely:
`Unit` parses as a variable reference and `Circle(2.0)` as a call — **typeck**
resolves an identifier/call to a variant constructor when the name is a known
variant (otherwise it's a variable/function, as today). So **construction needs
no parser change**. The cost — variant names global and unique — matches Lumo's
existing global namespaces and is enforced with a clear duplicate-name error. A
future move to optional qualification (A2b) is additive.

### Axis 3 — `match` (the second key choice)

```lumo
match scrutinee {
    Pattern => statement_or_block ,
    ...
}
```

- **v1: `match` is a statement** (each arm is a block or single statement), like
  `if`. Simplest, and arms commonly `return`/assign. `match` **as an expression**
  (arms yield a value) is the natural follow-up and is additive (R4).
- **Patterns (v1):** a variant name with positional bindings — `Circle(r)`,
  `Rect(w, h)`, `Unit` — plus the wildcard `_`. Bindings are fresh variables
  scoped to that arm, typed by the variant's payload. Literal patterns, nested
  patterns, and guards (`if`) are future extensions.
- **Exhaustiveness (R2):** the arms must cover **every** variant of the
  scrutinee's enum, or include a `_`. A missing variant is an error
  (listing the missing ones); a duplicate or unreachable arm is an error.

### Axis 4 — Runtime representation (R1)

An enum value is a **GC-allocated object** `{ i64 tag, slot_0, … slot_{k-1} }`
where `tag` is the variant's index (declaration order) and `k` is the **maximum
payload arity** across the enum's variants. Each `slot_i` is an 8-byte slot
holding that variant's *i*-th field via the existing `to_slot_i64` /
`slot_to_value` helpers (ints/floats/bools by bit pattern, references by
pointer). This mirrors structs (a heap pointer to an LLVM struct type) with a
leading tag, so:

- **Construction** `Circle(2.0)`: `lumo_alloc(8 + k*8)`, store `tag = index(Circle)`,
  store each argument into its slot. Returns the pointer (the enum value).
- **`match`**: load `tag`, `build_switch` to the matching arm's block; in each
  arm, load the bound fields from their slots (typed by the variant) into locals,
  then run the arm. The `_` arm is the switch default.
- **GC:** payload slots holding references are ordinary pointer words the
  collector traces (R1, like struct fields).

`Type` gains an `Enum(&'static str)` variant (interned name, `Copy`-preserving,
exactly like `Struct`). Enum values are reference types (nullable, compared by
identity) — consistent with structs.

### At-a-glance (recommended v1)

| Decision | Choice |
| --- | --- |
| Declaration | `enum E { A, B(T), C(T, U) }` (positional payloads) |
| Variant names | bare, globally unique; construction reuses ident/call syntax |
| `match` | statement; arms are `Pattern => stmt/block` |
| Patterns | variant + positional bindings, and `_` |
| Exhaustiveness | all variants or `_`, else a compile error |
| Representation | GC object `{ i64 tag, slot×maxArity }`; reuses slot helpers |
| Type | `Type::Enum(name)`, a nullable reference type like `Struct` |

---

## Drawbacks

- **Global variant names.** Two enums can't both have a `Pending`. Mitigated by a
  clear error; lifted later by optional qualification (A2b).
- **Fixed-size payload.** Every value of an enum is sized to its largest variant,
  wasting space for small variants. Fine at Lumo's scale; a future packed/boxed
  layout is possible behind the same surface.
- **Statement-only `match` in v1.** You write `match` + `return`/assign rather
  than `let x = match …`. Additive to fix later.
- **More surface to learn**, and `match` exhaustiveness can feel strict — but that
  strictness is the feature.

## Alternatives considered

- **Structs + an `int` tag, by hand (status quo).** No exhaustiveness, no payload
  typing, error-prone — exactly what `enum` removes.
- **Qualified variants from day one (A2b).** Cleaner namespacing but more syntax
  now; deferred as an additive option.
- **`match` as an expression first.** More powerful but bigger (every arm must
  yield a unifiable value, plus block-as-expression machinery Lumo lacks);
  statement-first keeps v1 small.
- **Untagged unions / C-style.** Unsafe, no exhaustiveness — against Lumo's
  identity.

## Phased rollout plan

1. **`enum` types + construction.** Lexer `enum` keyword; parser for declarations;
   `Type::Enum`; a variant registry in `typeck` (global uniqueness, payload
   types); typeck resolves bare `Variant` / `Variant(args)` to constructors;
   codegen lowers a variant to the tagged GC object. (No `match` yet — values can
   be built and passed around.)
2. **`match`.** Lexer `match` keyword; parser for `match`/arms/patterns; typeck
   binds pattern fields and **checks exhaustiveness**; codegen lowers to a
   `switch` on the tag with per-arm field binding.
3. **Quality / ergonomics.** Good "non-exhaustive match" and "unknown variant"
   diagnostics; a `Result`/`Option` example; docs.
4. **Later (additive):** `match` as an expression, literal/nested patterns and
   guards, named-field variants, and — with [generics](#) — `enum Option<T>`.

Each step ships golden-file tests (construction + each `match` path, plus
non-exhaustive/duplicate-arm error cases).

## Unresolved questions

1. **Bare vs. qualified variants** (Axis 2) — the main reviewer call. v1 leans
   bare+global for minimal syntax; is the collision risk acceptable?
2. **`match` arm syntax** — `Pattern => { block }` only, or also
   `Pattern => single_stmt;`? (Proposed: allow both, like `if` bodies vs. a
   single statement.)
3. **Statement vs. expression `match`** — ship statement-only now and add
   expression form later, or design the expression form up front?
4. **`null` and enums.** Enums are reference types, so `null` is assignable.
   Should a dedicated `Option`-like enum be encouraged over `null` for new code?
