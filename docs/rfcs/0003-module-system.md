# RFC 0003 — A module system (`import`) for multi-file Lumo programs

- **Status:** v1 implemented (`v0.35.0`). The recommended v1 below shipped:
  path-based `import "..."`, flat namespace, public-by-default, dedup +
  cycle-safe resolution, one merged LLVM module. The source-map prerequisite
  shipped first in `v0.34.1` (step 1). Remaining (v2): qualified imports
  (`import "x" as x; x.item`) and `pub` visibility.
- **Author:** Lumo contributors
- **Created:** 2026-05-24
- **Targets roadmap phase:** Phase 5 — Modules & packages (`v0.6`), the
  "module system: files/namespaces, `import`/`pub`, multi-file compilation" task.
- **Tracking issue:** _TBD_
- **Depends on:** nothing in the runtime, but **requires a source-map change to
  the span/diagnostics layer** (see [Diagnostics](#the-source-map-problem-the-real-work)),
  which is the bulk of the real work.

> **This is a request for comments, not a settled decision.** Every Lumo program
> so far has lived in a single `.lum` file. Splitting code across files is the
> single biggest prerequisite for building anything large — but it forces
> decisions about naming, visibility, and (less obviously) about how the compiler
> tracks source positions across files. The goal here is to settle the surface
> and the architecture *before* implementing, because both are hard to change
> later.

---

## Summary

Add an **`import` statement** so a Lumo program can span multiple files. v1
proposes the smallest design that is genuinely useful and forward-compatible:

- **Syntax:** `import "relative/path.lum";` at the top of a file.
- **Resolution:** the path is resolved **relative to the importing file's own
  directory**, then canonicalized.
- **Semantics (v1):** *flat-namespace inclusion* — every imported file's
  top-level `struct`s and `fn`s become visible in the importing program, as if
  concatenated. Each file is parsed **once** (dedup by canonical path), so a
  diamond import (`a`→`b`, `a`→`c`, both→`util`) pulls `util` in once. Genuine
  name collisions across files reuse the existing duplicate-definition errors
  (`E0304` / `E0103`-style).
- **Entry point:** exactly one file in the build has `fn main`.

The headline open questions are **(1) flat namespace vs. qualified names
(`util.parse`) and visibility (`pub`)**, and **(2) the source-map refactor** the
compiler needs so diagnostics from an imported file point at the right file and
line.

---

## Motivation

The roadmap's Phase 5 is "code in the large." Today everything — the lexer demo,
a web of structs, the standard-library-in-Lumo we will eventually want — must go
in one file. That caps the realistic size of a Lumo program and prevents any
reuse between programs.

Concretely, none of these are possible today:

```lumo
# main.lum
import "geometry.lum";
import "io_util.lum";

fn main() {
    let p = Point { x: 1, y: 2 };       # struct from geometry.lum
    print describe(p);                  # fn from geometry.lum
    return 0;
}
```

```lumo
# geometry.lum
struct Point { x: int, y: int }

fn describe(p: Point) -> string {
    return "(" + str(p.x) + ", " + str(p.y) + ")";
}
```

A module system also lets us start shipping a **standard library written in
Lumo** (rather than only as codegen built-ins), and lets examples share helpers
instead of copy-pasting a `show([int]) -> string` into every file.

---

## Guiding constraints / requirements

- **R1 — Forward-compatible surface.** Whatever v1 syntax we pick must not box us
  out of qualified names and visibility later. Adding `pub` and `module.item`
  must be *additive*, not a breaking change to v1 programs.
- **R2 — Honest diagnostics.** An error in an imported file must render with that
  file's name, line, and source snippet — never the root file's. This is the
  hard requirement; getting it wrong makes multi-file debugging miserable.
- **R3 — Deterministic, cycle-safe resolution.** Importing the same file twice
  (directly or via a diamond) must be a no-op, not a duplicate-definition error
  or an infinite loop. Import cycles must terminate.
- **R4 — No new runtime.** Modules are a front-end/driver concern; codegen still
  emits one LLVM module. (Separate compilation / linking is a later, bigger
  step — see [Alternatives](#alternatives-considered).)
- **R5 — Small, reviewable v1.** Ship the minimum that makes multi-file programs
  work, with a clear path to namespacing/visibility, rather than the whole Phase
  5 design at once.

---

## Detailed design

### Axis 1 — Import syntax

| Option | Example | Notes |
| --- | --- | --- |
| **A1a — path string** *(recommend)* | `import "util/strings.lum";` | No new identifier rules; path is explicit; trivially forward-compatible with an alias clause later (`import "x.lum" as x;`). |
| A1b — bare module name | `import strings;` | Needs a search-path concept and a name→file mapping; premature without a package manager. |
| A1c — selective | `from "x.lum" import parse;` | Useful with namespacing; overkill while v1 is flat. |

A path string is the least-commitment choice and matches how a file-based v1
actually resolves things.

### Axis 2 — Path resolution

Resolve the path **relative to the directory of the file that contains the
`import`** (not the process's CWD, and not a project root we don't yet define),
then `canonicalize` to an absolute path for deduplication. So `main.lum`'s
`import "util/strings.lum";` looks for `<dir of main.lum>/util/strings.lum`.

This is intuitive ("relative to me"), needs no project manifest, and composes:
a file deep in `util/` can import its siblings with short relative paths.

### Axis 3 — Namespacing

| Option | What `describe` from `geometry.lum` is called | Notes |
| --- | --- | --- |
| **A3a — flat / inclusion** *(recommend for v1)* | `describe(...)` | Simplest; imported names join one global namespace; collisions are errors. Matches the existing single-namespace typeck. |
| A3b — qualified | `geometry.describe(...)` | Cleaner at scale, avoids collisions, but needs a module value/namespace concept the language doesn't have, plus `.`-after-module-name parsing that overlaps field access. |
| A3c — qualified + aliases | `import "geometry.lum" as geo; geo.describe(...)` | The eventual target. |

**v1 picks A3a (flat).** Crucially it is *forward-compatible*: a future `import
... as name;` (A3c) is purely additive — unqualified `import` keeps meaning "dump
into my namespace," or we later require qualification and provide a deprecation
window. We accept that flat namespaces don't scale to large dependency trees;
v1's job is to make 2–10 file programs pleasant, and collisions there are rare
and clearly reported.

### Axis 4 — Visibility

| Option | Notes |
| --- | --- |
| **A4a — everything public** *(recommend for v1)* | Every top-level `fn`/`struct` in an imported file is visible. Zero new syntax. |
| A4b — `pub` keyword | Only `pub fn`/`pub struct` are importable; the rest are file-private. The right long-term default for encapsulation. |

v1 picks A4a but **reserves `pub` semantics for v2**. Adding `pub` later is
additive *if* we make the v2 rule "un-annotated items become file-private" only
under an opt-in (edition/flag) or accept it as a one-time breaking change at a
pre-1.0 minor bump. This is the main tension with R1 and is called out as an
[unresolved question](#unresolved-questions).

### Axis 5 — The source-map problem (the real work)

Today a [`Span`](../../src/span.rs) is a pair of byte offsets into *the one*
source string, and diagnostics render via `diag.render(&src, &path)` with that
single `src`. The moment two files contribute AST nodes, a span of `42..47` is
ambiguous — offset 42 of *which* file? An error in `geometry.lum` would be
printed against `main.lum`'s text: wrong file name, wrong line, wrong snippet.
This violates R2 and is the bulk of the implementation cost.

Proposed change:

1. Introduce a `FileId` (a small integer) and a **`SourceMap`**: `Vec<SourceFile
   { path: String, src: String }>`, indexed by `FileId`.
2. Extend `Span` to `{ file: FileId, start: usize, end: usize }`. `Span::merge`
   asserts equal `file`.
3. The lexer is parameterized by the `FileId` it is tokenizing; it stamps every
   token's span with it.
4. `Diagnostic::render` takes the `SourceMap` (not a single `&str`) and looks up
   the offending span's file for the filename + snippet.

This touches every `Span::new` call site (lexer) and the diagnostic renderer,
but is mechanical. It is independently valuable — even single-file builds get a
cleaner `render(sourcemap)` signature — and could land as a **prep PR before**
the `import` PR to keep each reviewable.

### Axis 6 — Resolution algorithm

```
parse_program(entry_path):
    sources = SourceMap::new()
    program = Program { structs: [], funcs: [] }
    visited = Set<canonical_path>()
    stack   = [canonical(entry_path)]          # DFS; order is post-order merge
    visit(canonical(entry_path))

    visit(path):
        if path in visited: return             # R3: dedup + cycle-safe
        visited.add(path)
        src  = read(path)
        fid  = sources.add(path, src)
        file = parse_file(lex(src, fid))       # -> { imports, structs, funcs }
        for imp in file.imports:               # imports resolved first (deps before dependents)
            visit(canonical(dir(path) + imp))
        program.structs += file.structs
        program.funcs   += file.funcs
    return (program, sources)
```

- `import` statements are only legal at the **top of a file**, before any
  `fn`/`struct` (keeps parsing simple; matches most languages).
- Deduplication is by **canonical path**, so symlinks/`./` variants collapse.
- After merging, the **existing** `typeck` runs unchanged on the combined
  `Program` and already reports duplicate struct names (`E0304`) and would report
  duplicate function names — so cross-file collisions need no new diagnostic
  code, only good messages (ideally naming both files; possible once spans carry
  `FileId`).
- Exactly one merged file may define `fn main` (a second `main` is just a
  duplicate-function error).

### Axis 7 — Build model

Codegen is unchanged: it compiles the single merged `Program` into one LLVM
module, exactly as today (R4). There is **no separate compilation or linking**
in v1 — every build re-reads and re-parses all imported files. That is fine at
this scale and keeps the JIT/`build`/`emit-ir` paths identical. Separate
compilation and a real package/dependency story are Phase 5's later half.

---

## Recommendation (v1, at a glance)

| Decision | Choice |
| --- | --- |
| Syntax | `import "relative/path.lum";` at file top |
| Resolution | relative to the importing file's dir, then canonicalized |
| Namespacing | flat (inclusion); collisions are duplicate-definition errors |
| Visibility | everything public (`pub` deferred to v2) |
| Dedup / cycles | parse each canonical path once (R3) |
| Spans | add `FileId` + a `SourceMap`; `render` takes the map |
| Codegen | unchanged; one merged LLVM module |
| Entry point | exactly one `fn main` across the build |

This is deliberately the *floor* of a module system: enough to split a program
across files and share helpers, nothing more.

---

## Drawbacks

- **Flat namespace doesn't scale.** Ten files with overlapping helper names will
  collide. Mitigation: v1 targets small programs; v2 adds qualification.
- **No encapsulation.** Without `pub`, every helper is exported; refactors can't
  hide internals. Accepted for v1; called out below.
- **Re-parse cost.** No caching/separate compilation means large graphs re-parse
  every build. Negligible now; revisit with a package manager.
- **The span refactor is invasive.** It edits every span construction site. But
  it is mechanical and independently improves diagnostics.

## Alternatives considered

- **Textual `#include` (C-style).** Splice tokens before parsing. Rejected: no
  dedup, no cycle safety, and it makes the source-map problem *worse* (one
  conceptual file from many). The post-order AST merge gives the same "flat"
  feel with proper file tracking.
- **Separate compilation + linking.** Compile each file to its own object and
  link. The eventual goal for build times and true modularity, but far larger
  (needs symbol visibility, a linking step, cross-module type identity). Out of
  scope for v1.
- **Namespaced-from-day-one (A3b/c).** Cleaner long-term, but requires a module
  value concept and disambiguating `module.item` from `value.field` in the
  parser. Deferred to keep v1 small.

## Phased rollout plan

1. **Prep — source map.** Add `FileId` + `SourceMap`; thread it through the lexer
   and `Diagnostic::render`. No surface change; single-file builds behave
   identically. *(Own PR.)*
2. **`import` v1.** Lexer keyword + top-of-file `import "..."` parsing; the
   resolve/merge driver (Axis 6); flat namespace; relative resolution; dedup +
   cycle safety; a multi-file test and example. *(Own PR.)*
3. **Quality.** Cross-file duplicate-definition messages that name both files;
   a clear error for a missing/unreadable import path with the `import`'s span.
4. **v2 (later RFC or amendment).** Qualified imports (`import "x.lum" as x;`,
   `x.item`) and `pub` visibility.

## Unresolved questions

1. **`pub` migration.** If v2 makes un-annotated items file-private, every v1
   program that relied on exporting everything breaks. Do we (a) take the break
   at a pre-1.0 minor, (b) gate it behind an edition/flag, or (c) keep "public by
   default" forever and add `priv`? *(Leaning (a), pre-1.0.)*
2. **Qualified vs flat as the default.** Should v2 *require* qualification
   (`x.item`) for imported names, or keep flat as an option (`import ... as *`)?
3. **Standard library distribution.** Once we can write stdlib in Lumo, where do
   those files live and how are they found without a package manager — a bundled
   path, an env var, a search list?
4. **`import` placement.** Strictly top-of-file (recommended) or anywhere at top
   level? Top-of-file is simpler and conventional.
5. **Cross-file spans in one diagnostic.** A type error involving a call site in
   `a.lum` and a definition in `b.lum` ideally shows both. v1 can show the
   primary span only; multi-span diagnostics are a later enhancement.
