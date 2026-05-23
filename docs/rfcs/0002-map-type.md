# RFC 0002 — A built-in `map` (associative array) type for Lumo

- **Status:** Implemented (`v0.24.0`, with automatic resize in `v0.24.1`). The
  full recommended design below shipped, including `delete`, `keys`, and Step 5
  resizing (load-factor 0.75 rehash). Remaining follow-ups: int keys and
  `for k in m` iteration.
- **Author:** Lumo contributors
- **Created:** 2026-05-23
- **Targets roadmap phase:** Phase 4 — Memory & runtime (`v0.5`), the
  "begin the standard library: **collections**" task.
- **Tracking issue:** _TBD_
- **Depends on:** the heap runtime (`lumo_alloc`) and the growable-array
  machinery from `v0.22` (RFC 0001 context). Interacts with the eventual
  reclamation story (RFC 0001) — see [Memory](#memory--interaction-with-rfc-0001).

> **This is a request for comments, not a settled decision.** The goal is to lay
> out the design space for an associative container, make a concrete
> *recommendation*, and give reviewers enough to push back before any code lands.
> Maps touch every layer of the compiler (lexer → parser → typeck → codegen +
> runtime), so getting the surface and the representation right *before*
> implementing matters more here than for a typical feature.

---

## Summary

Lumo can today build sequences (`[T]`, growable via `push`) but has **no
key→value lookup**. The natural next collection is an **associative map**:
count word frequencies, deduplicate, group records by a field, memoize, build an
index. All of these currently force an O(n) linear scan over parallel arrays.

This RFC proposes a built-in `map` type with **string keys** in v1, values of
any existing Lumo type, written `{string: V}`, with literals `{}` /
`{"a": 1, "b": 2}`, indexed access `m[k]` / `m[k] = v`, and the built-ins
`has`, `len`, `delete`, and `keys`. It is backed by a **hash table implemented
as runtime IR** (the same place `lumo_alloc`/`lumo_panic` live), reusing the
"every value is an 8-byte slot" trick that already makes arrays type-agnostic.

The headline open questions are **(1) the literal/type syntax and its parser
ambiguity with blocks and struct literals**, and **(2) what `m[missing_key]`
does**.

---

## Motivation

The roadmap's Phase 4 explicitly lists "begin the standard library:
**collections**." Arrays (`v0.11`) and growable arrays (`v0.22`) covered the
sequence half; the associative half is the obvious counterpart and is, in
practice, the single most-requested data structure after the list.

Concretely, none of these are expressible today without hand-rolling a linear
scan over two parallel arrays:

```lumo
# What we want to be able to write:
fn word_count(words: [string]) -> {string: int} {
    let counts: {string: int} = {};
    for (let i = 0; i < len(words); i = i + 1) {
        let w = words[i];
        if (has(counts, w)) {
            counts[w] = counts[w] + 1;
        } else {
            counts[w] = 1;
        }
    }
    return counts;
}
```

The workaround — two arrays plus an O(n) `index_of` on every access — is O(n²)
for word counting and is exactly the kind of boilerplate a "small language that
scales" should absorb into a primitive. Maps also unblock later roadmap items:
a symbol table for a self-hosting compiler (Phase 10), config/environment lookup,
and the generic collection library of Phase 7 (a built-in map is the concrete
thing generics later abstract over).

---

## Guiding constraints / requirements

Following RFC 0001's lens approach:

- **R1 — Reuse what exists.** We already have `lumo_alloc`, runtime panics,
  `strcmp`/`strlen`, and a growable-array layout. A map should lean on these, not
  introduce a parallel universe.
- **R2 — Value model stays uniform.** Codegen lowers every expression to
  `(BasicValueEnum, Type)`; arrays already store any element in an 8-byte slot
  (int/float bit-pattern or pointer). A map's values should use the *same* slot
  trick so one runtime works for all `V`.
- **R3 — `typeck` stays a type checker, codegen trusts it.** No lifetime or
  ownership reasoning. The map type is just another `Type` variant.
- **R4 — Small, testable surface.** Prefer a handful of built-ins (`has`, `len`,
  `delete`, `keys`) over bespoke syntax. Each gets golden-file tests like every
  other feature.
- **R5 — Honest semantics.** Missing keys, iteration order, and reclamation must
  have a defined, documented behavior — no UB, matching "errors are a feature."
- **R6 — Don't foreclose generics.** Phase 7 wants a *generic* collection
  library. The v1 map is a built-in special case; its surface (`m[k]`, `len`,
  iteration) should look like what a future `Map<K, V>` would expose, so the
  generic version is a generalization rather than a redesign.
- **R7 — Keep `Type` `Copy`.** As with `Array(Elem)`, a `Map` variant must stay
  register-sized and `Copy` (intern names, carry a `Copy` value descriptor).

---

## Detailed design

The design splits into independent axes. Each is presented with options and a
recommendation; the **At-a-glance** table summarizes.

### Axis 1 — Key types

- **A1a. String keys only (v1).** Keys are `string`; hashing/equality reuse the
  bytes via `strlen`/`strcmp`. Covers the overwhelming majority of real uses
  (word counts, indices, config).
- **A1b. String *and* int keys.** Adds integer-keyed maps (`{int: V}`). Hashing
  an `i64` is trivial, equality is `==`. Modest extra work (a second key path in
  the runtime, or a generic key-compare callback).
- **A1c. Arbitrary keys.** Any hashable/comparable type. Requires a general
  hashing/equality story (and, for structs, structural hashing) — effectively
  needs the Phase 7 generics/traits machinery.

**Recommendation: A1a (string keys) for v1**, with the runtime written so a
second key type (int) can be added without restructuring (see
[Implementation](#axis-6--runtime-representation)). A1c waits for traits.

### Axis 2 — Value types

Values may be **any existing Lumo type** (`int`, `bool`, `float`, `string`,
arrays, structs, `null`, even nested maps). This is free under R2: the value is
stored in an 8-byte slot exactly like an array element (scalars by bit pattern,
references by pointer). `typeck` enforces that every value matches the declared
`V`; codegen stores/loads at type `V`.

### Axis 3 — Type syntax

The type of a map from `string` to `V`:

- **A3a. `{string: V}`** — brace + `K: V`. Reads like the literal; familiar from
  Python/TS type hints. **Ambiguity:** `{` also begins blocks; the *type* parser
  (`parse_type`) is separate from the expression/statement parsers, so in type
  position this is unambiguous. The literal collision is the real issue (Axis 4).
- **A3b. `map<V>` / `Map<V>`** — angle brackets. No brace clash, but introduces
  `<…>` syntax that nothing else in Lumo uses yet, and pre-empts the Phase 7
  generic syntax in a way we may regret.
- **A3c. `[string: V]`** — Swift-style, square brackets (consistent with array
  `[T]`). No new bracket; visually close to arrays, which is either a feature
  (consistency) or a bug (easy to confuse `[T]` with `[K: V]`).

**Recommendation: A3a `{string: V}`** for types, paired with `{…}` literals
(Axis 4) so the type and the value rhyme. A3c is the strongest alternative and
worth a vote; it sidesteps every brace-ambiguity concern at the cost of looking
like an array.

### Axis 4 — Literal syntax and the brace-ambiguity problem

Proposed literals: empty `{}` and non-empty `{"a": 1, "b": 2}` (trailing comma
allowed, matching arrays/structs).

This is the crux. Today `{` appears only as a **block** (after `fn`/`if`/`while`/
`for`) and inside **struct literals** (`Name { field: value }`). Introducing
brace *expressions* requires the parser to decide what a `{` means:

1. **Struct literal vs. map literal.** A struct literal is always **prefixed by a
   type name**: `Point { … }`. A bare `{ … }` with no leading identifier is
   therefore free to mean a map literal. This disambiguation is clean and
   already implicit in the grammar.
2. **Block vs. map literal.** Blocks appear only in *statement* position (and only
   attached to `fn`/`if`/`while`/`for` — Lumo has no bare block statement). Map
   literals appear only in *expression* position (after `=`, in a call argument,
   in a `return`). The parser already knows which position it is in, so `{` at the
   start of an expression → map literal; `{` where a block is expected → block.
   The one place to be careful is anywhere a statement could *start* with an
   expression — Lumo's statement starters are keywords (`let`/`if`/`while`/`for`/
   `return`/`print`/`break`/`continue`) or an assignment/`Expr` statement; an
   expression-statement beginning with `{` does not currently exist and we can
   simply **disallow a bare `{` as a statement starter** (it's a block only in the
   keyword-attached positions), removing the ambiguity by rule.
3. **Empty `{}`.** Unambiguous as a map literal in expression position, but — like
   the empty array literal `[]` (v0.22) — it **cannot infer its type**, so it is
   allowed only where an expected type is available: `let m: {string: int} = {};`.
   Bare `let m = {};` is an error (mirror of `E0206`).

The non-empty literal needs at least one `key: value` pair to infer `V` (the key
type is fixed to `string` in v1, so `{1: 2}` is a type error, not a different map
kind). The element value type is inferred from the first pair and checked against
the rest, exactly like array literals.

**Recommendation:** adopt `{}` / `{k: v, …}` literals with the two
disambiguation rules above (name-prefix ⇒ struct; statement-position `{` is
never a map). Empty literal requires an annotation. **This is the most
reviewer-sensitive part of the RFC** — if the parser rules feel too subtle,
Axis 3c + a `[:]`-style empty literal (Swift) is the fallback.

### Axis 5 — Operations and missing-key behavior

| Operation        | Surface              | Result                              |
| ---------------- | -------------------- | ----------------------------------- |
| Insert / update  | `m[k] = v;`          | sets/overwrites; `m` mutated in place |
| Lookup           | `m[k]`               | the value for `k` (see below)       |
| Membership       | `has(m, k)`          | `bool`                              |
| Size             | `len(m)`             | `int` (number of entries)           |
| Remove           | `delete(m, k)`       | removes `k` if present (no error if absent) |
| Keys             | `keys(m)`            | `[string]` (a fresh growable array) |

`m[k]` and `m[k] = v` **reuse the existing `Index` AST node** — indexing is
already overloaded for arrays (int index) and strings (byte read); adding "map
indexed by its key type" is a third case in the same typeck/codegen switch (R1).
Like arrays, a map value points at a stable header, so `m[k] = v` mutates in
place and is visible through aliases.

**Missing-key behavior (the second crux).** Options for `m[k]` when `k` is
absent:

- **A5a. Runtime panic** `lumo: key not found`, exit 101 — consistent with
  out-of-bounds array indexing and null-deref (all already panic via
  `lumo_panic`). Forces the programmer to guard with `has`. *Safe and
  consistent.*
- **A5b. Return the zero value of `V`** (`0`, `false`, `0.0`, `""`/`null`) and a
  separate way to tell "absent" from "present-but-zero." Convenient for counters
  (`counts[w] = counts[w] + 1` *almost* works) but silently hides bugs and has no
  good answer for `V` without a natural zero.
- **A5c. No `m[k]` read at all; require `get_or(m, k, default)`** and `has`.
  Explicit, but verbose and breaks the `m[k]` symmetry with arrays.

**Recommendation: A5a (panic on missing key), plus `has` for guarding.** It is
consistent with every other Lumo container access and never hides a bug. The
common counter idiom stays a one-liner with `has`:

```lumo
if (has(counts, w)) { counts[w] = counts[w] + 1; } else { counts[w] = 1; }
```

We may later add `get_or(m, k, default) -> V` as ergonomic sugar (it composes
cleanly and needs no new semantics).

### Axis 6 — Runtime representation

A **separately-chained hash table**, implemented as runtime IR alongside
`lumo_alloc`/`lumo_panic`, mirroring the double-indirect array design so growth
is alias-safe:

- **Map value → header** `{ i64 count, i64 nbuckets, ptr buckets }` (24 bytes).
  The map value a Lumo variable holds is the *header* pointer; it is stable, so
  resizes (which replace `buckets`) are visible through every alias — the same
  property that made `push` safe in v0.22.
- **`buckets`** is a heap array of `nbuckets` entry-pointers (each a chain head,
  or null).
- **Entry** `{ ptr key, i64 hash, i64 value, ptr next }`. `key` is the string
  pointer; `hash` is cached to skip `strcmp` on hash mismatch; `value` is the
  8-byte slot (bit-cast like array elements); `next` chains collisions.
- **Hash:** FNV-1a over the key bytes (`strlen` for length, a byte loop for the
  mix). A small, well-understood function; no crypto needs here.
- **Runtime functions** (one set, value-type-agnostic because values are 8 bytes):
  `lumo_map_new(nbuckets) -> ptr`, `lumo_map_put(map, key, hash, value)`,
  `lumo_map_get(map, key, hash) -> {found:i1, value:i64}` (or get + has split),
  `lumo_map_del(map, key, hash)`, `lumo_map_len(map) -> i64`, and
  `lumo_map_keys(map) -> array` (builds a `[string]` using the v0.22 array
  runtime — nice reuse).
- **Resize:** when `count / nbuckets` exceeds a load factor (e.g. 0.75), allocate
  a larger bucket array and rehash. **v1 may ship with a fixed bucket count**
  (e.g. 64) and defer resize to a fast follow — O(n/64) worst-case lookups are
  acceptable for a first cut and keep the initial IR small. Resize is the single
  most error-prone piece of IR, so isolating it is deliberate.

The "value is 8 bytes" trick (already used by arrays) means **one runtime serves
every `V`**: codegen bit-casts the typed value to `i64` on `put` and back to `V`
on `get`. Adding int keys later (Axis 1b) means the key path takes an `i64` key
and an identity hash instead of `strcmp`/FNV — a localized change.

### Axis 7 — Iteration

Lumo has no `for-in` loop yet, so v1 exposes iteration via **`keys(m) -> [string]`**
(a fresh growable array, in unspecified order) and lets the user index back in:

```lumo
let ks = keys(counts);
for (let i = 0; i < len(ks); i = i + 1) {
    print ks[i] + " = " + str(counts[ks[i]]);
}
```

**Iteration order is unspecified** (hash order), and documented as such (R5).
A future `for k in m { … }` (and ordered maps, if wanted) is a clean additive
extension and is called out as out of scope here.

### At-a-glance (recommended choices)

| Axis | Options | Recommended |
|---|---|---|
| 1. Keys | string / +int / arbitrary | **string only (v1)**, int-ready runtime |
| 2. Values | any Lumo type (8-byte slot) | **any type** |
| 3. Type syntax | `{string:V}` / `map<V>` / `[string:V]` | **`{string: V}`** |
| 4. Literals | `{}` & `{k:v,…}`; empty needs annotation | **adopt, with brace rules** |
| 5. Missing key | panic / zero-value / `get_or` only | **panic + `has`** |
| 6. Runtime | chained hash table in IR (8-byte values) | **adopt; fixed buckets in v1** |
| 7. Iteration | `keys()` array / `for-in` | **`keys()` now**, `for-in` later |

---

## Recommendation

**Ship a string-keyed, hash-backed `map` built into the language**, with:

- type `{string: V}`; literals `{}` (annotation-required when empty) and
  `{"a": 1, …}`;
- `m[k]` / `m[k] = v` via the existing `Index` node; **panic on missing key**;
- built-ins `has(m, k)`, `len(m)`, `delete(m, k)`, `keys(m)` (the last returns a
  `[string]`);
- a separately-chained hash table in runtime IR using the v0.22 alias-safe
  header pattern and the 8-byte-slot value trick, **fixed bucket count in v1**,
  resize as a fast follow.

This maximizes reuse (R1, R2), keeps `typeck` a type checker (R3), adds a small
built-in surface (R4), defines every edge case (R5), and looks like the eventual
generic `Map<K, V>` (R6) while keeping `Type` `Copy` (R7).

**What would change this recommendation:**

- **If the brace-literal parser rules (Axis 4) prove contentious or fragile in
  review,** switch to Swift-style `[string: V]` types with `[:]` empty literals
  (Axis 3c) — it removes the block/struct ambiguity entirely at the cost of
  looking like an array.
- **If early users need ordered or insertion-ordered iteration,** the header gains
  a parallel insertion-order list (small) before we commit to "order unspecified"
  in users' minds.
- **If Phase 7 generics arrive sooner than expected,** we might implement the map
  *in Lumo* over a generic hash table rather than as a built-in — in which case
  this RFC's surface becomes the standard-library API and the built-in is dropped.

---

## Drawbacks

- **New surface syntax (`{…}` expressions) with subtle parser rules.** Even with
  clean disambiguation, braces now mean three things (block, struct literal, map
  literal). That cognitive load is the main cost.
- **A non-trivial chunk of hand-written runtime IR.** The hash table (hashing
  loop, chain walk, resize) is the largest single piece of runtime IR to date and
  the most error-prone. Mitigated by deferring resize and by golden tests, but
  it's real.
- **Memory is never reclaimed (yet).** Entries and bucket arrays come from
  `lumo_alloc` (malloc-backed) and, like all Lumo heap data, are freed only at
  program exit until RFC 0001's reclamation lands. `delete` unlinks an entry but
  does not free it. Honest, but a limitation for long-running programs.
- **Unspecified iteration order** can surprise users who expect insertion or
  sorted order; must be documented loudly.
- **String-keys-only** means int-keyed lookups (a common case) wait for a follow-up.

## Memory & interaction with RFC 0001

Maps allocate three things: the header, the bucket array, and one entry per key.
All go through `lumo_alloc` and inherit RFC 0001's status quo — **no per-object
free; reclaimed at program exit**. Two specific notes:

- **Resize reallocs the bucket array** (via `realloc`, like `push`). Because the
  *header* pointer is stable, this is alias-safe; but it assumes a malloc-backed
  allocator. When RFC 0001's arena lands, `realloc`-based growth (here and in
  `push`) must be revisited together — a shared concern worth tracking jointly.
- **`delete` does not free.** It unlinks the entry from its chain and decrements
  `count`; the entry's memory is reclaimed only at exit. Consistent with the rest
  of the language today.

---

## Alternatives considered

- **Library-level assoc-list over two arrays.** No new syntax or runtime; O(n)
  per access. Rejected as the *primitive*: it makes word-count O(n²) and is
  exactly the boilerplate a built-in should remove. (It remains a fine teaching
  example.)
- **Open addressing (linear/Robin Hood probing) instead of chaining.** Better
  cache behavior and no per-entry `next` pointer. Rejected for v1 only because
  chaining's insert/delete is simpler to express correctly in hand-written IR
  (deletion under open addressing needs tombstones). A reasonable later swap — the
  surface doesn't change.
- **Swift-style `[K: V]` syntax.** The leading alternative for Axis 3/4; see
  "what would change this recommendation."
- **Defer maps until Phase 7 generics and implement `Map<K, V>` in Lumo.**
  Cleaner long-term layering, but leaves users without a dictionary for several
  phases. Rejected on R1/timing; the built-in can later be re-expressed as the
  std-lib generic type.
- **Tagged dynamic values as map values (one map type, any value).** Against
  Lumo's static-typing identity (mirrors RFC 0001's rejection of NaN-boxing).

---

## Phased rollout plan

Each step ships with golden-file tests, runs under JIT + native + `-O2`, and
keeps CI green — the established per-feature workflow.

- **Step 1 — Types & front end.** Add `Type::Map` (value descriptor, `Copy`,
  interned) in `types.rs`; parse `{string: V}` in `parse_type`; parse `{}` and
  `{k: v, …}` literals in the expression parser with the Axis-4 disambiguation
  rules; `typeck` validates key type = `string`, infers/checks `V`, and handles
  the empty-literal-needs-annotation rule (mirror of `[]`). No codegen yet —
  front end can be tested for accept/reject with diagnostics.
- **Step 2 — Runtime + literal codegen.** Emit `lumo_map_new`, `lumo_map_put`,
  `lumo_map_get`, `lumo_map_len` as runtime IR (fixed bucket count); lower map
  literals to `new` + a sequence of `put`s. First runnable maps.
- **Step 3 — Indexed access.** Wire `m[k]` (get, panic-on-missing) and `m[k] = v`
  (put) through the `Index` node in typeck + codegen. The headline ergonomics.
- **Step 4 — Built-ins.** `has`, `delete`, and `keys` (the last reusing the
  growable-array runtime to return `[string]`). Reserve all new names (and update
  examples/docs that might collide — a known gotcha: examples aren't in CI).
- **Step 5 — Resize.** Add load-factor-driven rehashing; remove the fixed-bucket
  limitation. Purely an internal change, guarded by tests that insert enough keys
  to force several resizes.
- **Step 6 (later, separate RFC/PR) — extensions.** Int keys (Axis 1b), `for k in
  m` iteration, `get_or`, ordered iteration — each additive.

---

## Unresolved questions

1. **Type/literal syntax: `{string: V}` + `{…}` vs. Swift `[string: V]` + `[:]`?**
   The single most important call for reviewers — it trades brace ambiguity
   against array look-alikeness.
2. **`m[missing]`: panic (recommended) or zero-value or `get_or`-only?** Affects
   the counter idiom's ergonomics.
3. **Fixed bucket count in v1, or resize from day one?** Smaller initial IR vs.
   no worst-case cliff.
4. **`keys(m)` now vs. waiting for `for k in m`.** Is an array of keys an
   acceptable iteration story for v1, or should `for-in` be co-designed?
5. **Should `delete` return whether the key was present (`bool`)?** Costs nothing;
   adds a small convenience.
6. **Int keys in v1 (Axis 1b)?** They're cheap and common; is string-only too
   limiting to ship?
7. **Interaction with RFC 0001 reclamation.** `delete` and resize both touch the
   "we don't free yet" reality and the future arena; how tightly should the map
   and the arena work be sequenced?
8. **Does `Type` grow, or do we add a `Copy` "value descriptor" alongside it?**
   Same question RFC 0001 raised for heap types; maps add `{key, value}` shape
   that must stay register-sized.

---

*Comments welcome — especially on the syntax (Q1) and missing-key semantics
(Q2). If you'd prefer Swift-style `[K: V]`, or think maps should wait for Phase 7
generics, please make the case in the tracking issue with the use cases you're
optimizing for.*
