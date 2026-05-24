# RFC 0001 — Memory-Management Strategy for Lumo

- **Status:** **Implemented (Boehm GC, `v0.44.0`).** Decided in the 2026-05-24
  revision and shipped: `lumo_alloc`→`GC_malloc`, grow paths→`GC_realloc`,
  `GC_init` at startup; heap is reclaimed automatically. The original draft
  recommended
  "arena-first, reclamation-deferred," which shipped as a never-free
  `lumo_alloc` (a malloc wrapper). With heap strings, growable arrays, maps, and
  structs all in the language now — and a *scalable* product as the goal — the
  deferred question is due. **Decision: adopt the Boehm–Demers–Weiser
  conservative garbage collector for v1 reclamation** (see
  [Decision](#decision-2026-05-24-revision)). The original option analysis below
  is preserved for context; the [Recommendation](#recommendation) it reached is
  **superseded** by the Decision.
- **Author:** Lumo contributors
- **Created:** 2026-05-21 (revised 2026-05-24)
- **Targets roadmap phase:** Phase 4 — Memory & runtime (`v0.5`)
- **Tracking issue:** _TBD_

> **The decision below is settled; the rest is the analysis that led to it.** The
> original draft laid out five strategies and ranked an arena first. This revision
> records the actual call now that the language has matured past the point where
> "reclaim at exit" is acceptable.

---

## Summary

Lumo needs heap memory. Today every Lumo value fits in a machine register —
`int` (`i64`), `bool` (`i1`), `float` (`f64`) — and `string` exists only as an
**immutable literal** lowered to a global constant: a string value is just a
pointer to a NUL-terminated global (`build_global_string_ptr` in `codegen.rs`).
There is **no heap allocation anywhere in the language or compiler**, and there
is **no runtime library** beyond an external declaration of C's `printf`.

To deliver the next tier of features the roadmap promises — string
concatenation/building, dynamic arrays/lists, and (later) user-defined aggregate
types — Lumo must allocate, track, and reclaim dynamically-sized memory.

This RFC analyzes five strategies (manual `alloc`/`free`, arena/region
allocation, automatic reference counting, tracing GC, and Rust-style
ownership/borrowing). The original draft **recommended a pragmatic, incremental
path**: ship a *minimal runtime with a bump/arena allocator* to unblock heap
strings and arrays **now**, deferring reclamation. That shipped (as a never-free
`lumo_alloc`). The **2026-05-24 Decision below** now resolves the deferred
reclamation question.

---

## Decision (2026-05-24 revision)

**Adopt the Boehm–Demers–Weiser conservative garbage collector (`libgc`) as
Lumo's v1 memory-reclamation strategy.** Concretely: `lumo_alloc` calls
`GC_malloc` instead of `malloc`, the growth paths (`push`, map resize) call
`GC_realloc`, and the collector reclaims unreachable objects automatically by
conservatively scanning the stack and heap. This is **Option D1** from the
analysis below, chosen now over the originally-ranked arena (Option B).

### Why this, why now

The original RFC explicitly said the arena-first call would flip *"if Lumo's
first real users run long-lived servers — program-lifetime arenas are
unacceptable and the calculus shifts toward Boehm GC now."* We are there:

- **The leak is real and total.** `lumo_alloc` is a `malloc` wrapper that never
  frees. Every concatenated string, `push`, `substr`/`split`/`replace`, `sorted`,
  `map` insert, `read_line`, and `read_file` leaks for the life of the process. A
  `while (line != null)` loop over a large input grows without bound. For a
  *scalable* product this is the single most important gap.
- **The other options don't fit or are too large, here and now:**
  - *Arena/regions (B):* a process-lifetime arena reclaims no better than the
    current never-free malloc. Real reclamation needs **scoped regions + escape
    analysis**, and the growth model (`push`/map resize via `realloc`, returning a
    stable header) does not map cleanly onto bump arenas. Significant front-end
    work for partial reclamation.
  - *ARC (C):* pervasive retain/release insertion across all control flow, a
    per-type drop story, and **cycles leak** without an added cycle collector.
  - *Custom precise GC (D2):* Lumo's heap is **many small allocations** with
    indirection — an array is a `{len,cap,data}` header *plus* a separate data
    block; a map is a header *plus* a bucket array *plus* per-entry nodes. A
    precise collector needs per-kind layout descriptors and a root map
    (stack maps / shadow stack) for all of these. Large and correctness-critical.
  - *Ownership/borrowing (E):* a borrow checker is a research track, not a step.
- **Boehm is the only option that is both *real reclamation* (including cycles)
  and *tractable*.** It needs **no object headers, no shadow stack, and no layout
  descriptors** — it scans conservatively. Integration is essentially "swap the
  allocator," which fits the existing malloc-based runtime with near-zero codegen
  change. It handles Lumo's layouts correctly: every value reference points at a
  block start (the array/map/string/struct base), the header→data and
  bucket→entry pointers are ordinary words the collector traces, and Boehm's
  default interior-pointer support covers the transient `data + 8*i` pointers that
  indexing forms on the stack.

### Trade-offs accepted

- **A dependency.** `libgc` (bdw-gc) is not currently installed; it must be added
  locally and in CI (`apt-get install libgc-dev` on Ubuntu, `brew install bdw-gc`
  on macOS). The project already depends on LLVM 22, so one more well-established
  system library is in keeping.
- **Conservative & non-moving.** May occasionally retain garbage that looks like a
  pointer, and cannot compact. Acceptable for Lumo's scale; precise/moving GC
  stays a future option (D2) and the door to ownership (E) is not *architecturally*
  closed (the value model is unchanged).
- **GC pauses / nondeterminism.** Fine for the CLI/batch/scripting workloads Lumo
  targets today. `lumo_alloc` remains the single allocation chokepoint, so the
  strategy can be swapped again later behind that boundary.

### How it integrates

- **JIT (`lumo run`):** the JIT resolves runtime symbols (`printf`, `malloc`, …)
  from the host process. Link the `lumo` compiler binary against `libgc` (a
  `build.rs` emitting `cargo:rustc-link-lib=gc` + the search path) so `GC_malloc`
  / `GC_realloc` / `GC_init` are present in the process and resolvable by the JIT,
  exactly as libc symbols are today.
- **Native (`lumo build`):** the `clang` link step adds `-lgc` (and the library
  search path) so produced executables carry the collector.
- **Startup:** call `GC_init` once at the top of generated `main` (Boehm also
  lazily self-initializes on first `GC_malloc`, but an explicit init is clearer
  and pins the stack base).
- **Allocator:** `declare_runtime` declares `GC_malloc`/`GC_realloc` and defines
  `lumo_alloc` as a thin `GC_malloc` wrapper; the two `realloc` call sites (`push`
  grow, `lumo_map_resize`) switch to `GC_realloc`. No other codegen changes.

The [Phased rollout plan](#phased-rollout-plan) below is updated to these steps.

---

## Motivation

The roadmap's north star is "a small, fast, statically-typed systems language
that scales." Two near-term features are blocked on a heap:

1. **Strings that do more than print.** Concatenation and string building require
   producing a string whose size is not known at compile time. Our current
   string is an immovable global constant pointer — there is nowhere to put the
   result of `"a" + b`.
2. **Dynamic arrays / lists.** A growable sequence needs a backing buffer that
   can be resized at runtime.

Both of these, and the user-defined aggregate types that follow, force three
decisions we have so far avoided:

- **Where does memory come from?** (an allocator / runtime)
- **Who owns a heap value, and for how long?** (the ownership/lifetime model)
- **When and how is it reclaimed?** (the reclamation strategy)

This is also a *strategic* decision, not just a tactical one. The memory model
leaks into the syntax, the type system, the calling convention, the FFI story
(Phase 5), and even concurrency (Phase 10). Phase 4 of the roadmap explicitly
calls for "a research spike + RFC: ownership/borrowing vs tracing GC vs ARC.
Pick one and document why." **This document is that RFC.** Choosing badly early
is expensive to undo; choosing *nothing* blocks Phase 2/3 features that users
will want long before Phase 4 lands.

---

## Guiding constraints / requirements

These are the lenses every option below is judged against. They follow directly
from the roadmap's design principles ("LLVM does the heavy lifting", "boring,
testable architecture", "fast feedback").

- **R1 — Unblock Phase 2/3 soon.** Strings and arrays should not have to wait for
  a perfect lifetime theory. We need *something* that works this quarter.
- **R2 — Small surface area.** A new strategy should be implementable and
  testable by a small team. Complexity must buy proportional value.
- **R3 — Don't foreclose the systems-language endgame.** Whatever ships first
  must not paint us into a corner that makes a future predictable / GC-free model
  impossible without a language-breaking rewrite.
- **R4 — Fits the existing compiler shape.** It must compose with:
  - the `typeck` pass that runs before codegen and that codegen *trusts*;
  - the codegen value model, where every expression lowers to
    `(BasicValueEnum<'ctx>, Type)` (see `gen_expr`);
  - the fact that `Type` (`src/types.rs`) is currently `Copy` and register-sized
    (`Int`, `Bool`, `Float`, `Str`);
  - the absence of any runtime — today the only external symbol is `printf`.
- **R5 — Deterministic and debuggable.** UB-free codegen and sanitizer-friendly
  output are cross-cutting roadmap commitments. Whatever we pick should be
  inspectable with `lldb`/ASan and shouldn't fight DWARF debug info (Phase 6).
- **R6 — Predictable performance.** "Fast systems language" implies we care about
  pause times, throughput, and memory overhead — at least enough to not box
  ourselves out of those goals later.
- **R7 — FFI-friendly.** Phase 5 adds C FFI. The heap representation should be
  expressible across an FFI boundary without a heavy translation layer.

No single option maxes out all seven; the recommendation is about which
constraints matter *most, first*.

---

## Detailed design (the options)

Each option below covers: **how it works**, **pros/cons for a small LLVM-based
language**, **implementation complexity**, **runtime cost**, and **impact on
ergonomics/syntax**.

A note that applies to all five: **every option needs a minimal runtime first.**
Right now there is no place for `malloc`, no `lumo_alloc`, no string-builder
helpers, nothing. So step zero in *all* cases is "formalize a runtime/stdlib
boundary" (which the roadmap already lists as a Phase 4 task). The options differ
in what that runtime *does*.

### Option A — Manual allocation (explicit `alloc` / `free`)

**How it works.** Expose allocation and deallocation primitives (likely thin
wrappers over C `malloc`/`free`, declared the same way `printf` is declared
today). Heap strings/arrays become `{ ptr, len, cap }`-style structs; the
programmer is responsible for freeing them. `typeck` need only know the types;
it makes no lifetime guarantees.

**Pros for a small LLVM language.**
- Trivial to lower: a `call` to an external `malloc`/`free`. Reuses the exact
  mechanism we already have for `printf`.
- Zero hidden runtime cost; fully predictable; great for FFI (R7).
- Smallest possible runtime.

**Cons.**
- Pushes correctness entirely onto the user. Use-after-free, double-free, and
  leaks are *the language's behavior*, not bugs in the compiler — a terrible
  default for a language that markets "errors are a feature."
- Doesn't compose with value-style ergonomics: `"a" + b` would have to return
  something the caller must remember to free. That is hostile syntax for the
  small programs Phase 2 wants to enable.
- Contradicts the "scalable to many users" goal: manual memory is a notorious
  adoption tax.

**Implementation complexity.** Lowest. Days, not weeks.

**Runtime cost.** Whatever `malloc`/`free` cost; no overhead beyond that.

**Ergonomics/syntax impact.** High and negative. Either we add explicit
`free(x)` to the surface language (ugly, error-prone) or we never free (leaks).
Acceptable only as a *temporary* internal primitive, not as the user-facing
model.

### Option B — Arena / region allocation (bump allocator, free in bulk)

**How it works.** Maintain one or more arenas. Allocation is a pointer bump
(`current += size`); individual values are never freed. An entire arena is
reclaimed in **bulk** at a well-defined point — at minimum, at program exit; more
usefully, at the end of a scope/region (e.g. a function or an explicit
`region { ... }`). The runtime is tiny: a growable list of large blocks plus a
bump pointer.

**Pros for a small LLVM language.**
- **Dead simple to implement and to lower.** Allocation is a couple of IR
  instructions or a call to `lumo_alloc(size)`; there is no per-object
  bookkeeping, no headers, no destructors, no graph walking.
- **Fast allocation, zero per-object free cost.** Excellent throughput.
- No changes to the type system are *required* — `typeck` stays a type checker,
  not a lifetime checker (R4). `Type` can stay `Copy`; heap values are just
  pointers into an arena.
- Plays perfectly with "LLVM does the heavy lifting" and "boring, testable
  architecture": the whole allocator is a few hundred lines of Rust/C with
  obvious tests.
- Naturally compatible with a future ownership model: arenas are *also* a
  performance tool in ownership-based languages, so this is not wasted work (R3).

**Cons.**
- **Memory is only reclaimed in bulk.** A long-running program that allocates in
  one big region effectively leaks until that region ends. For short-lived,
  batch-style programs (compilers, scripts, CLI tools — exactly Lumo's early
  audience) this is *fine*; for servers it is not, on its own.
- Region inference / scoping is its own design problem if we want
  finer-than-program-lifetime reclamation. Getting it ergonomic (without
  dangling references escaping a region) eventually wants type-system support —
  which starts to look like a slice of ownership anyway.
- Sharing across regions / returning heap data from a function needs a rule
  (e.g. "promote to a longer-lived arena" or "the caller passes the arena in").

**Implementation complexity.** Low. The allocator itself is small. Bulk-free at
program exit is essentially free to implement. Scoped regions add complexity but
can be deferred.

**Runtime cost.** Allocation: near-optimal (pointer bump). Reclamation:
amortized to ~zero (one bulk free). Memory overhead: high water mark within a
region; no compaction.

**Ergonomics/syntax impact.** Low if reclamation is program-lifetime only —
strings and arrays "just work" and are never explicitly freed, which reads like a
GC'd language to the user. Higher if we expose `region { ... }` blocks, but that
can be additive and optional.

### Option C — Automatic reference counting (ARC)

**How it works.** Every heap object carries a reference count in a header.
Codegen inserts `retain`/`release` calls when references are copied and dropped;
when a count hits zero the object (and its owned children) is freed. Cycles must
be handled separately (weak refs, or a cycle collector, or "we don't, document
it").

**Pros for a small LLVM language.**
- Deterministic, prompt reclamation; predictable destruction order — good for
  RAII-style resource cleanup later.
- No stop-the-world pauses; relatively friendly to systems programming and FFI
  (you can hand out a `+1` reference).
- The runtime is moderate: `retain`/`release` plus an allocator. No stack
  scanning, no GC threads.

**Cons.**
- **`typeck`/codegen must become ownership-aware enough to insert retain/release
  correctly at every copy, move, drop, branch, and early return.** This is
  exactly the bookkeeping that ownership models make explicit — except here it's
  implicit and easy to get subtly wrong, producing leaks or use-after-free.
- **Cycles leak** unless we add weak references (more syntax/semantics) or a
  backup cycle collector (a second memory subsystem — defeats "small").
- Per-operation runtime cost (atomic ops once we have threads; non-atomic but
  still non-trivial before that). Retain/release traffic can dominate hot loops.
- Touches the value model deeply: `(BasicValueEnum, Type)` becomes insufficient
  on its own; codegen needs to know which types are refcounted and emit cleanup
  on every scope exit, which means a real ownership/drop analysis pass — a large
  jump from where `typeck` is today.

**Implementation complexity.** Medium-high. The allocator is easy; *correct
automatic retain/release insertion across all control flow* is the hard,
bug-prone part, and cycles add a whole second mechanism.

**Runtime cost.** Steady per-reference overhead; cache-unfriendly count updates;
atomics under concurrency. No pauses.

**Ergonomics/syntax impact.** Mostly invisible to users (a plus) until cycles
bite, at which point `weak`/`unowned` annotations leak into the surface language
(see Swift). Reasonable long-term, heavy to start.

### Option D — Tracing garbage collection (mark-sweep; Boehm GC or custom)

**How it works.** The runtime periodically traces from roots (stack, globals)
through the object graph, marks reachable objects, and reclaims the rest. Two
sub-flavors:

- **Boehm-Demers-Weiser (conservative) GC as a library.** Link `libgc`, replace
  `malloc` with `GC_malloc`, and *delete* `free` from our concerns. Conservative
  scanning means **no compiler changes to track roots** — it scans the stack
  conservatively.
- **A custom precise collector.** We emit stack maps / shadow stacks so the GC
  knows exactly where pointers are. More work, more control, better precision.

**Pros for a small LLVM language.**
- **Boehm specifically is the fastest path to "allocation just works with
  automatic reclamation."** Drop-in, no ownership analysis, no retain/release,
  handles cycles for free. For unblocking strings/arrays it is genuinely
  competitive with the arena option on implementation effort.
- Frees the front end from lifetime reasoning entirely — `typeck` stays a type
  checker (R4).
- Great ergonomics: users never think about memory.

**Cons.**
- **Boehm is conservative**: it can retain garbage (false roots), it's a large
  external dependency, it doesn't move/compact, and it's somewhat at odds with a
  "we own a small, fast systems runtime" identity. It also complicates FFI and
  precise stack scanning, and interacts awkwardly with future precise features.
- **A custom precise collector is a major project**: stack maps via LLVM's GC
  intrinsics (`gc.statepoint`/`gc.root`/`stackmap`), a mark-sweep (or
  generational/compacting) core, write barriers if generational — easily the
  largest item in this RFC. That's a poor fit for "small team, small surface
  area" *right now*.
- Pause times and non-determinism cut against R6 for the systems-language
  endgame, and against deterministic resource cleanup.
- A GC'd ABI is the hardest of all options to later swap *out* for a GC-free
  model — programs and libraries start assuming GC semantics (R3 risk).

**Implementation complexity.** Boehm: low-to-medium (mostly integration +
testing). Custom precise GC: high.

**Runtime cost.** Cheap allocation, periodic pause/throughput cost; memory
overhead for headroom; conservative GC may over-retain.

**Ergonomics/syntax impact.** Lowest of all — memory becomes invisible. The cost
is philosophical/strategic, not syntactic.

### Option E — Rust-style ownership & borrowing (compile-time, no runtime GC)

**How it works.** The type system tracks **ownership** (each value has one
owner), **moves** (transferring ownership), and **borrows** (temporary
references with lifetimes checked at compile time). The compiler inserts
deterministic `drop`s at end of scope; there is **no runtime collector**. This is
the model that most directly serves "small, fast systems language."

**Pros for a small LLVM language.**
- **No runtime memory manager at all** beyond an allocator — best fit for R6 and
  the systems-language north star. Deterministic destruction, no pauses, minimal
  overhead.
- Memory-safety errors become *compile-time* diagnostics — directly on-brand with
  "errors are a feature."
- Excellent FFI and predictable performance (R7, R6).

**Cons.**
- **This is, by a wide margin, the largest front-end project in the RFC.** It
  requires: a real ownership/move analysis, a borrow checker with lifetime
  inference, a typed HIR rich enough to express references and regions, drop
  insertion across all control flow, and a substantial body of diagnostics to
  make the inevitable borrow-checker errors *teachable*. None of that exists
  today; `typeck` is presently a straightforward type checker over a small `Type`
  enum.
- It changes the **surface language** the most: references, lifetimes (even if
  mostly inferred), `mut`, move semantics, and the mental model users must learn.
  That is a big ask to bolt on right when we're trying to get basic strings and
  arrays working.
- High risk of stalling Phase 2/3 features for a long time while the model is
  designed and stabilized. Borrow checking is notoriously hard to get *both*
  sound and ergonomic.

**Implementation complexity.** Highest. A multi-phase effort in its own right;
realistically a research track, not a single PR.

**Runtime cost.** Effectively just the allocator. Best-in-class.

**Ergonomics/syntax impact.** Largest — a defining characteristic of the
language, with a real learning curve. Powerful long-term, premature now.

### At-a-glance comparison

| Option | Impl. effort (now) | Runtime cost | Reclaims memory? | Touches `typeck`? | Syntax impact | Fits systems endgame |
|---|---|---|---|---|---|---|
| A. Manual alloc/free | Lowest | None | Manual (leaky) | No | `free()` everywhere | Neutral, but bad UX |
| B. Arena / bump | **Low** | Negligible alloc, bulk free | Bulk only | No (initially) | Minimal | Yes (complementary) |
| C. ARC | Med-high | Per-ref + cycles | Yes (no cycles) | Yes (drop insertion) | Mostly hidden (`weak`) | Reasonable |
| D1. Boehm GC | Low-med | GC pauses | Yes (incl. cycles) | No | None | Weak / lock-in risk |
| D2. Custom GC | High | GC pauses | Yes | Roots/stackmaps | None | Reasonable |
| E. Ownership/borrow | **Highest** | Allocator only | Yes (compile-time) | Heavily | Largest | **Best** |

---

## Recommendation

> **⚠️ Superseded by the [Decision (2026-05-24 revision)](#decision-2026-05-24-revision).**
> The arena-first recommendation below was the original call; it shipped as a
> never-free allocator and is now replaced by adopting the Boehm conservative GC.
> The reasoning is kept for the historical record and because the option
> comparison remains useful.

**Original recommendation (for discussion): a phased "arena-first, ownership-later" path.**

1. **Now (Phase 4 first cut): build a minimal runtime around an arena / bump
   allocator.** Heap strings and dynamic arrays allocate from a process-lifetime
   arena and are *not* individually freed in v1 of the runtime. This unblocks
   string concatenation/building and dynamic arrays immediately (R1), with the
   smallest possible runtime and zero changes to the ownership semantics of the
   type system (R2, R4). For Lumo's early audience — CLI tools, algorithms,
   batch programs — "allocate freely, reclaim at exit" is correct and invisible.

2. **Soon after: add scoped regions (`region { ... }` or per-call arenas)** so
   reclamation is no longer only at program exit, addressing the obvious leak
   objection for longer-running programs without committing to a full collector.

3. **Long term (research track, not blocking): pursue compile-time ownership &
   borrowing** as the eventual primary model, because it is the only option that
   fully serves the "small, fast, GC-free systems language" north star (R6, R3).
   Arenas are *complementary* to ownership (they remain a performance tool), so
   the early work is not thrown away.

**Why arena over the alternatives, specifically:**

- **vs. Manual (A):** Same low effort, dramatically better UX. Manual `free` in
  the surface language contradicts "errors are a feature" and the adoption goals.
  We'll still build `lumo_alloc` internally — arena is "manual alloc, but the
  language frees in bulk so users never write `free`."
- **vs. ARC (C):** ARC's hard part (correct automatic retain/release across all
  control flow, plus cycles) is precisely the front-end complexity we want to
  avoid while we're still landing basic heap types. It also pre-commits the value
  model to refcounting before we've decided whether ownership is the endgame.
- **vs. Boehm GC (D1):** This is the closest competitor and a legitimate
  alternative — it reclaims cycles and "just works." We rank arena slightly ahead
  because (a) a bump allocator is *even simpler* and has no large external
  dependency, (b) it introduces no GC pauses or conservative over-retention to
  reason about, and (c) it carries **less strategic lock-in**: an arena-backed
  ABI is far easier to evolve toward ownership than a GC-backed one (R3). **This
  is the decision most worth debating** — see below.
- **vs. custom GC / ownership (D2, E):** Both are right-sized for *later*, not for
  unblocking Phase 2/3. Ownership is the likely long-term destination, but
  starting there risks stalling the language for a long time on borrow-checker
  design. We keep it as a research track.

**What would change this recommendation:**

- **If Lumo's first real users run long-lived servers**, program-lifetime arenas
  are unacceptable and the calculus shifts toward **Boehm GC now** (fast path to
  real reclamation incl. cycles) or accelerating regions/ownership.
- **If we discover the team has appetite and bandwidth for a borrow checker
  sooner**, we could skip straight to ownership (E) and use arenas only as an
  internal optimization — better long-term coherence at higher short-term risk.
- **If FFI (Phase 5) lands before reclamation matters**, GC's conservative stack
  scanning becomes a liability and the arena/ownership direction looks even
  better.
- **If benchmarks show arena high-water-mark memory is a problem in practice**
  before regions exist, that accelerates the regions milestone (step 2) or a move
  to a real collector.

In short: **start simple to unblock strings/arrays, keep the long-term door open
to ownership, and treat the arena-vs-Boehm question as the key open call for
reviewers.**

---

## Drawbacks

- **Arenas don't free per-object.** The headline objection. A program that
  allocates a lot in a single region holds that memory until the region ends. We
  accept this for the first cut and mitigate it with scoped regions (step 2). It
  is a real limitation for server-style workloads and we should say so plainly in
  the docs rather than pretend it is GC.
- **We are explicitly deferring the hard question.** Choosing arena-first means we
  have *not* yet committed to the endgame memory model. Some reviewers may
  reasonably prefer to decide ownership-vs-GC once and avoid two migrations.
- **Two migrations of risk.** Arena → regions → ownership is more total churn
  than picking the destination immediately. We're betting that shipping value
  early and learning from real programs is worth that churn.
- **The value model and `typeck` will eventually have to grow regardless.** Heap
  types break the current assumption that every `Type` is `Copy` and
  register-sized; even arenas need codegen to represent strings/arrays as
  `{ ptr, len, cap }` aggregates and to thread an allocator handle. That work is
  unavoidable under *any* option here.

---

## Alternatives considered

The five strategies in **Detailed design** are themselves the alternatives; the
recommendation rejects A (manual as a user model), C, D2, and E *as the
first step*, and ranks B over D1 for the first step. Additional alternatives:

- **"Do nothing / stay register-only."** Rejected: it permanently blocks strings,
  arrays, and structs — the whole point of Phases 2–4.
- **Boehm GC as the *first* step (rather than arena).** A serious alternative,
  not dismissed — see the recommendation's "what would change this." It wins if
  early users need real reclamation (incl. cycles) immediately and we value
  zero front-end work over avoiding a runtime dependency and GC pauses.
- **Hybrid from day one (arena + GC, or arena + ARC).** Rejected for the first
  cut on R2 grounds (two memory subsystems is the opposite of "small"), but a
  plausible *end state* — e.g. arenas for short-lived allocations plus an
  ownership model for the rest.
- **Tagged/NaN-boxed dynamic values + GC (a scripting-language runtime).**
  Rejected: it pulls toward a dynamically-typed runtime, against Lumo's
  statically-typed, systems-language identity.

---

## Phased rollout plan

> Updated for the Decision (Boehm GC). Steps 0–3 and the struct/heap work of the
> *original* plan already shipped (heap strings, growable arrays, maps, structs)
> on the never-free `lumo_alloc`. What remains is turning on reclamation.

- **Step 1 — Build integration.** Add `libgc` to the toolchain: a `build.rs`
  linking the `lumo` binary against `gc` (so the JIT can resolve `GC_*` from the
  process), the `clang` link step gaining `-lgc` for native builds, and CI
  installing bdw-gc (`libgc-dev` on Ubuntu, `bdw-gc` on macOS). De-risk: confirm
  `GC_malloc` resolves in both JIT and native before touching the allocator.
- **Step 2 — Swap the allocator.** `declare_runtime` declares `GC_malloc`,
  `GC_realloc`, `GC_init`; `lumo_alloc` becomes a `GC_malloc` wrapper; the `push`
  grow path and `lumo_map_resize` switch their `realloc` to `GC_realloc`; emit a
  `GC_init` call at the top of generated `main`. **This is the step that makes
  memory actually get reclaimed.** No layout, value-model, or `typeck` changes.
- **Step 3 — Prove it.** Tests that allocate unboundedly in a loop (e.g. millions
  of `push`/concat/`read_line` iterations) must run in **bounded** resident
  memory rather than growing without limit — the concrete "no leaks" check the
  original exit criterion deferred. Keep the existing golden tests green
  (behavior must be identical).
- **Later (optional) — precise / moving GC (D2)** if conservatism or pause
  behavior ever bites, and the **ownership research track (E)** remains open as
  the long-term "GC-free systems language" endgame; both live behind the
  unchanged `lumo_alloc` boundary, so swapping is localized.

This completes roadmap **Phase 4 (Memory & runtime)**'s reclamation goal: with the
GC, "programs that allocate and free memory run without leaks" holds at runtime,
not just at program exit.

---

## Unresolved questions

1. ~~**Arena vs. Boehm GC for the very first step.**~~ **Resolved (2026-05-24):**
   Boehm GC — see the [Decision](#decision-2026-05-24-revision). Real users /
   long-running programs make cycle-safe automatic reclamation worth the
   dependency.
2. **Is ownership truly the endgame, or is a precise GC acceptable forever?** Left
   open; the GC is behind the `lumo_alloc` boundary, so this stays revisable (R3).
3. **GC tuning.** Should generated `main` call `GC_init` explicitly (clearer,
   pins the stack base) or rely on lazy self-init? Do we ever need
   `GC_set_*` knobs, or `GC_malloc_atomic` for pointer-free blocks (string/`[int]`
   data) as an optimization? Treated as follow-ups, not blockers.
4. **JIT root scanning.** Confirm Boehm correctly finds roots on the stack of the
   JIT-invoked `main` across platforms (it does in the common single-threaded
   case; the Step 1 de-risk check verifies this before committing).
5. **String literals: keep the global-constant fast path?** Almost certainly yes,
   but we need a clean way for a `Str` value to be *either* a global literal or an
   arena-allocated buffer without two separate types in `typeck`.
6. **Concurrency interaction (Phase 10).** Arenas are simple single-threaded;
   thread-local arenas vs. shared, and atomic refcounts/GC roots, all change
   under concurrency. How much do we pre-design now vs. defer?
7. **FFI ownership across the boundary (Phase 5).** When Lumo hands a heap value
   to C (or receives one), who owns it and who frees it? Each model answers this
   differently.
8. **Does the `Type` enum stay `Copy`?** Heap types likely force it to carry more
   structure; we should decide whether `Type` grows or whether a separate
   "layout/representation" concept is introduced alongside it.

---

*Comments welcome. If you disagree with the arena-first recommendation —
especially in favor of Boehm GC or of going straight to ownership — please make
the case in the tracking issue with the workload assumptions you're optimizing
for.*
