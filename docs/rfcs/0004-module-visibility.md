# RFC 0004 — Module system v2: `pub` visibility and qualified imports

- **Status:** Proposed (draft).
- **Author:** Lumo contributors
- **Created:** 2026-05-24
- **Targets roadmap phase:** Phase 5 — Modules & packages (`v0.6`), the second
  half of the "module system: `import`/`pub`, visibility rules" task.
- **Tracking issue:** _TBD_
- **Depends on:** [RFC 0003](0003-module-system.md) (the `import` v1 that shipped
  in `v0.35.0`) and the `FileId`/`SourceMap` from `v0.34.1`.

> **This is a request for comments, not a settled decision.** Module v1 made
> multi-file programs *work*, but with a flat, fully-public namespace: every
> top-level `fn`/`struct` in any imported file is visible everywhere, and a name
> defined twice anywhere is an error. That does not scale past a handful of
> files. This RFC designs the encapsulation half — **`pub` visibility** — and the
> collision-avoidance half — **qualified imports** — that RFC 0003 explicitly
> deferred to "v2." Both change name resolution and one introduces a pre-1.0
> breaking change, so settling the design first matters.

---

## Summary

Two additions on top of `import` v1:

1. **`pub` visibility.** A top-level `fn`/`struct` is **private to its file** by
   default; prefix it with `pub` to make it importable. Within its own file
   everything stays visible. This is a **breaking change** (v1 was public by
   default) — proposed as a one-time pre-1.0 break.
2. **Qualified imports.** `import "geo.lum" as geo;` binds the file to a name, so
   its public items are referenced as `geo.area(...)`. Unqualified
   `import "geo.lum";` keeps the v1 "dump public names into my scope" behavior.

The headline questions are **(1) whether v2 mangles names** (which decides
whether two files may each have a *private* item of the same name) and **(2) the
`pub`-by-default → private-by-default migration**.

---

## Motivation

With `import` v1, a 10-file program shares one global namespace: every helper is
exported, so a `fn parse` in one file collides with `fn parse` in another, and
nothing can be hidden as an implementation detail. Encapsulation is what lets a
file present a small surface and refactor its internals freely — the whole point
of "code in the large." Qualified names additionally let two modules both offer
a `parse` without colliding, and make a call's origin obvious at the call site.

```lumo
# strings.lum
pub fn slugify(s: string) -> string { ... }
fn normalize(s: string) -> string { ... }   # private helper

# main.lum
import "strings.lum" as str;
fn main() {
    print str.slugify("Hello World");   # ok: slugify is pub
    # print str.normalize("x");         # error: normalize is private
    return 0;
}
```

---

## Guiding constraints / requirements

- **R1 — Same-file code is unrestricted.** `pub` only gates *cross-file* access;
  a file always sees all of its own items.
- **R2 — Single-file programs are unaffected.** Everything is one file, so all
  items are same-file-visible; `pub` is a no-op there and existing programs and
  tests keep working unchanged.
- **R3 — Honest, located errors.** Referencing a private item from another file
  is a clear diagnostic (`E0106`) pointing at the use site (the source map makes
  this possible across files).
- **R4 — Additive where possible.** Qualified imports must not break unqualified
  ones. The `pub` default flip is the one accepted exception (R6).
- **R5 — No premature machinery.** Avoid a full namespace/type-identity redesign;
  reuse the flat-merge-then-typecheck pipeline, layering accessibility on top.
- **R6 — Pre-1.0 break is allowed once.** Flipping to private-by-default breaks
  v1 multi-file programs; we take that break now, while pre-1.0, rather than
  carrying public-by-default forever.

---

## Detailed design

### Axis 1 — `pub` visibility

Add an optional `pub` modifier before a top-level `fn` or `struct`:

```lumo
pub struct Point { x: int, y: int }
pub fn area(p: Point) -> int { ... }
fn helper() -> int { ... }            # file-private
```

**Default is private** (file-local). An item is accessible from a *use site* iff
it is defined in the **same file** as the use, **or** it is `pub`.

#### The mangling question (the real decision)

To resolve `helper()` differently per file, two files could each define a private
`helper`. Today codegen emits an LLVM function literally named `helper`, so two
of them would collide at the IR level. Options:

| Option | Same-name privates across files? | Cost |
| --- | --- | --- |
| **A1a — no mangling (v2)** *(recommend)* | **No** — every `fn`/`struct` name stays globally unique (today's duplicate-name error, `E0103`/`E0304`, still fires across files). `pub` only controls *whether a use may reference* a (uniquely-named) item. | Tiny: an accessibility check layered on the existing global tables. |
| A1b — mangling (v3) | Yes — private items are renamed per file (`helper$3`) in codegen so duplicates don't clash. | A codegen renaming pass + per-file symbol tables; larger. |

**Recommend A1a for v2.** It delivers real encapsulation (you can't *reach* a
private item from another file) without a codegen renaming pass. Its limitation —
two files can't both have a private `helper` — is a minor annoyance at this scale
and a clean future extension (A1b) when it bites.

#### typeck mechanics (A1a)

The pipeline already merges all files into one `Program` and builds global
`sigs`/`structs` maps (RFC 0003). Extend each entry with `{ file: FileId, pub:
bool }` (both available from the item's `span.file` and its new `pub` flag).
Then:

- The body checker knows its **current file** (the enclosing function's
  `span.file`). All expressions in a body share that file.
- At each name resolution — a **function call**, a **struct type** in an
  annotation (`validate_type`), a **struct literal**, a **field access's struct
  type** — after finding the entry by name, require `entry.file == current_file
  || entry.pub`; otherwise emit **`E0106` "X is private to its file"** at the use
  site.

No scoping-stack change, no mangling: a thin accessibility filter over the
existing lookups.

### Axis 2 — Qualified imports and aliases

| Option | Example | Notes |
| --- | --- | --- |
| **A2a — optional `as` alias** *(recommend)* | `import "geo.lum" as geo;` then `geo.area(p)` | Unqualified `import "geo.lum";` keeps v1 behavior (dump `pub` names). The alias introduces a *module value* namespace. |
| A2b — always qualified | every import must be aliased and used qualified | Cleanest at scale but breaks v1's unqualified form (violates R4). |

**Recommend A2a.** The parsing wrinkle: `geo.area` looks like field access
(`expr.field`). Disambiguation: if the leading identifier is a **bound module
alias** (tracked from `import ... as`), `alias.name` resolves to that module's
item; otherwise `expr.field` is ordinary field access. Because aliases live in a
separate namespace from variables, this is decidable at parse/resolve time. (A
first cut may implement `pub` only, and land aliases in a follow-up — see
rollout.)

### Axis 3 — The `pub`-default migration

Flipping to private-by-default breaks any v1 multi-file program that relied on
exporting everything (including this repo's own `import_main` test, whose helper
files would need `pub` added). Per R6 we take the break at this pre-1.0 minor
(`v0.40.0`), call it out loudly in the CHANGELOG, and update the bundled
tests/examples to add `pub`. Single-file programs are unaffected (R2).

---

## Recommendation (v2, at a glance)

| Decision | Choice |
| --- | --- |
| Visibility default | **private to the file**; `pub` to export |
| Accessibility rule | same-file **or** `pub` |
| Names | stay globally unique (no mangling); cross-file duplicates remain errors |
| Diagnostic | new `E0106` at the use site |
| Qualified imports | optional `import "x" as x;` → `x.item` (may land after `pub`) |
| Migration | private-by-default is a pre-1.0 break (`v0.40.0`); update bundled tests |
| codegen | unchanged (no renaming) |

---

## Drawbacks

- **Breaking change.** v1 multi-file programs need `pub` added. Mitigated by
  being pre-1.0 and single-file-safe.
- **No same-name privates (A1a).** Two files can't each have a private `helper`.
  A future mangling pass (A1b) lifts this.
- **Alias parsing nuance.** `alias.item` vs `value.field` needs the resolver to
  know module aliases; manageable but a new rule.

## Alternatives considered

- **Keep public-by-default, add `priv`.** Avoids the break but makes the unsafe
  thing (exporting everything) the default forever; rejected.
- **Full per-file symbol tables + mangling now (A1b).** The "right" long-term
  model, but more machinery than v2 needs; deferred.
- **Path-qualified access without aliases** (`"geo.lum".area`): ugly and couples
  call sites to file paths; rejected in favor of `as` aliases.

## Phased rollout plan

1. **`pub` visibility** (own PR): the `pub` modifier (parser + AST flag), the
   accessibility check in typeck (`E0106`), the default flip, and updated bundled
   tests/examples. *Breaking; `v0.40.0`.*
2. **Qualified imports** (own PR): `import "x" as x;` + `x.item` resolution.
3. **(Later) Mangling (A1b)** if same-name privates across files become a real
   need.

## Unresolved questions

1. **`pub struct` field visibility.** Does `pub struct` export its fields too, or
   should fields have their own visibility? v2 proposes: a `pub` struct exports
   its fields (no field-level `pub` yet).
2. **Re-export.** Should `import` of a module that itself imports others re-export
   the transitive `pub` items, or only direct ones? v2 proposes: **no
   re-export** — you import what you directly name.
3. **Alias vs. variable collision.** What if a module alias `str` shadows (or is
   shadowed by) a variable `str`? Likely: aliases are a separate, file-scoped
   namespace resolved first for the `alias.item` form.
4. **`main` visibility.** `main` is found by the driver regardless of `pub`;
   should defining a `pub main` be an error or a no-op? Proposed: allowed, `pub`
   ignored on `main`.
