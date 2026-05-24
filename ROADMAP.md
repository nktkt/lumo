# Lumo Roadmap

> **Vision.** Lumo grows from a toy LLVM front end into a small, fast, statically
> typed systems language with first-class tooling — a product that scales along
> two axes: **compiling large codebases quickly** and **supporting a real
> community of users and contributors**.

This is a long-range plan. It is intentionally ambitious and will be revised as
we learn. Versions are targets, not promises. Everything before `v1.0` may break.

---

## Progress so far

What is implemented today — all merged to `main`, CI green, across PRs #1–#11:

- [x] **Foundations:** byte-offset source spans, rich diagnostics (error codes + caret), an end-to-end test harness, and GitHub Actions CI (`fmt` + `clippy` + `build` + `test` on Ubuntu/macOS with LLVM 22).
- [x] **Types:** `int`, `bool`, `float`, `string`, and arrays `[T]`.
- [x] **Type annotations** on functions (`fn f(x: int) -> bool`) and a dedicated type-checking pass (separate from codegen).
- [x] **Numeric conversions:** the `int()` / `float()` built-ins.
- [x] **Control flow:** `if`/`else`, `while`, `for`, `break`, `continue`.
- [x] **Block scoping** with shadowing (lexical).
- [x] **A minimal heap runtime** (`lumo_alloc`): string concatenation (`+`) and equality (`==` / `!=`); arrays (literals, bounds-checked indexing read/write, `len`).
- [x] **Optimization:** LLVM optimization passes via `-O0`..`-O3`.
- [x] **Backends:** JIT (`run`), native executable (`build`, links libc via clang), and `emit-ir`.
- [x] **Docs:** a tutorial, a language reference, a compiler-internals guide, RFCs 0001 (memory model) and 0002 (map type), an examples catalog, and a CHANGELOG.

Still open: memory reclamation, `struct`s, and modules. See
[Immediate next steps](#immediate-next-steps-next-few-prs).

---

## Design principles

These constraints shape every decision below.

1. **LLVM does the heavy lifting.** We own the front end and a mid-level IR; LLVM
   owns optimization and code generation. We do not reinvent a backend.
2. **Errors are a feature.** Diagnostics with source spans, clear messages, and
   suggestions are part of the language, not an afterthought.
3. **Fast feedback.** Compilation must stay fast as programs grow — incremental
   and parallel by design, not bolted on later.
4. **Boring, testable architecture.** Every phase ships with tests and CI. No
   feature is "done" without a regression test.
5. **Stability is earned at 1.0.** Before 1.0 we move fast; after 1.0 we keep
   promises (semver + a deprecation policy).

---

## The v0.1 milestone (historical)

The proof of concept that started it all. The full pipeline worked end to end:

`lexer → parser → AST → LLVM IR codegen → JIT / native executable / IR dump`

- `i64`-only values; `+ - * / %`, comparisons, `if/while`, recursive functions, `print`.
- Three CLI modes: `run` (JIT), `build` (native), `emit-ir`.

Every limit of v0.1 — no source locations, one numeric type, no heap types, no
type checker, no test suite or CI — has since been removed; see
[Progress so far](#progress-so-far) for the current state.

---

## The path at a glance

Status: ✅ done · 🟡 partial · ⬜ not started.

| Phase | Version | Theme | Status | Outcome |
|------:|---------|-------|:------:|---------|
| 0 | `v0.1` | Proof of concept | ✅ | Compiles & runs basic programs through LLVM |
| 1 | `v0.2` | **Foundations** | ✅ | Source spans, rich diagnostics, test harness, CI |
| 2 | `v0.3` | **A useful language** | ✅ | `bool`/`float`/`string`/arrays, richer control flow |
| 3 | `v0.4` | **Type system** | 🟡 | Annotations + a type-checking pass done; generics/traits/`struct`s not done |
| 4 | `v0.5` | **Memory & runtime** | 🟡 | Minimal malloc-backed allocator done; reclamation + heap-model decision open |
| — | — | **Optimization** | 🟡 | `-O0`..`-O3` done; incremental/parallel compilation not done |
| 5 | `v0.6` | **Modules & packages** | ⬜ | Module system, FFI, package manager + registry (alpha) |
| 6 | `v0.7` | **Tooling & DX** | ⬜ | LSP, formatter, REPL, debug info (DWARF) |
| 7 | `v0.8` | **Abstraction** | ⬜ | Generics, traits/interfaces, ADTs + pattern matching |
| 8 | `v0.9` | **Compiler scale** | ⬜ | Query-based incremental + parallel compilation, opt passes |
| 9 | `v1.0` | **Production-ready** | ⬜ | Stability guarantees, multi-target (incl. WASM), docs, governance |
| 10 | `1.x+` | **Scale-out** | ⬜ | Concurrency, self-hosting, ecosystem, performance leadership |

> Note: optimization (`-O` flags) was originally scoped under Phase 8 (Compiler
> scale) but landed early; the rest of that phase (incremental + parallel
> compilation) is still outstanding.

---

## Phases in detail

### Phase 1 — Foundations (`v0.2`) ✅
*Make the project safe to build on.*

- Track byte/line/column **spans** in the lexer and parser.
- **Rich diagnostics**: `error[E0001]`-style codes, source snippets with carets, hints.
- **Test harness**: golden-file tests (`tests/cases/*.lum` + `*.expected`) for output and for emitted IR (snapshot).
- **CI** (GitHub Actions): `cargo build`, `cargo test`, `cargo fmt --check`, `cargo clippy`, on macOS + Linux.
- `CONTRIBUTING.md`, issue/PR templates.

**Exit:** every error points at a source location; a green CI gate runs on every PR.

### Phase 2 — A useful language (`v0.3`) ✅
*More than integers.*

- Types at the value level: `bool`, `float` (f64), `string`, with literals.
- Heap-allocated `string` and fixed/dynamic `array`.
- `for` loops, `break`/`continue`, logical `&&` / `||` (short-circuit), `!`.
- `print` becomes a small set of built-ins (`print`, `println`, `len`).

**Exit:** can write small real programs (string manipulation, simple algorithms).

### Phase 3 — Type system (`v0.4`) 🟡
*Catch errors before runtime.*

- [x] Function **type annotations** (`fn f(x: int) -> bool`) and a dedicated **type-checking pass**, separate from codegen.
- [x] Compile-time errors for type mismatches, arity, undefined names.
- [ ] A typed AST/HIR with local **type inference**.
- [ ] User-defined `struct`s; field access and construction.
- [ ] `Result`-style error values or first-class `Option`/`Result` (decision spike).

**Exit:** ill-typed programs are rejected with good messages; structs work end to end.

### Phase 4 — Memory & runtime (`v0.5`) 🟡
*A model that lasts.*

- [x] A minimal heap runtime (`lumo_alloc`, malloc-backed) powering strings and arrays.
- [~] **Research spike + RFC**: ownership/borrowing vs tracing GC vs ARC — RFC 0001 drafted; decision still open.
- [ ] Implement the chosen model; memory **reclamation** (arena/regions) with deterministic cleanup or GC. *(Allocations currently leak.)*
- [~] Begin the **standard library**: collections, strings, math, basic I/O. *(Arrays + `push`/`pop`/slicing (`a[i:j]`)/`sorted`/`reversed` (v0.32–0.33), math built-ins, an associative `map` type — [RFC 0002](docs/rfcs/0002-map-type.md), shipped in v0.24 — **nested collections** (`[[T]]`, `{string: [V]}`, v0.29), the string toolkit (`substr`/`split`/`join`, `to_upper`/`to_lower`/`trim`/`find`/`contains`/`replace`/`repeat`, slicing, + `int`/`float`/`is_int`/`is_float` parsing), and file I/O (`read_file`/`write_file`, v0.28) are done; more stdlib ongoing.)*
- [ ] Formalize the runtime (replace ad-hoc `printf` with a real runtime/stdlib boundary).

**Exit:** programs that allocate and free memory run without leaks under the chosen model.

### Phase 5 — Modules & packages (`v0.6`)
*Code in the large.*

- [~] **Module system**: files/namespaces, `import`/`pub`, visibility rules. *([RFC 0003](docs/rfcs/0003-module-system.md): path-based `import` with flat-namespace v1 shipped in v0.35.0 — multi-file programs work. Remaining: qualified imports and `pub` visibility (v2).)*
- **FFI**: `extern` declarations for C interop, formalized.
- **Package manager** (`lumo add/build/test`) with lockfiles + a registry **alpha**.
- Multi-file compilation.

**Exit:** a multi-package project builds reproducibly from a manifest + lockfile.

### Phase 6 — Tooling & DX (`v0.7`)
*Where adoption is won or lost.*

- **Language Server (LSP)**: diagnostics, go-to-def, hover, completion.
- **Formatter** (`lumo fmt`) with a canonical style.
- **REPL** (`lumo repl`) backed by the JIT.
- **Debug info** (DWARF) so `lldb`/`gdb` can step through Lumo source.
- Editor extensions (VS Code first).

**Exit:** an editor gives real-time errors, formatting, and breakpoints work.

### Phase 7 — Abstraction (`v0.8`)
*Reuse that scales.*

- **Generics** (monomorphized through LLVM).
- **Traits / interfaces** for shared behavior.
- **Algebraic data types + pattern matching** (`match`, exhaustiveness checks).
- Closures / first-class functions.

**Exit:** a generic, trait-based collection library is expressible in Lumo itself.

### Phase 8 — Compiler scale (`v0.9`) 🟡
*Stay fast as codebases grow.*

- [x] **Optimization**: drive LLVM pass pipelines via `-O0`..`-O3`. *(Landed early.)*
- [ ] **Query-based, incremental** compilation (salsa-style): recompile only what changed.
- [ ] **Parallel** front end across modules; on-disk caching of artifacts.
- [ ] **Benchmarks** with regression tracking (compile time + runtime).

**Exit:** incremental rebuilds of a large project are sub-second; benchmarks gate PRs.

### Phase 9 — Production-ready (`v1.0`)
*Make promises we can keep.*

- **Stability**: semver, a language spec, a documented deprecation policy.
- **Targets**: x86_64 + aarch64 native, **WebAssembly**, cross-compilation.
- **Docs**: "The Lumo Book" (tutorial), full language reference, an in-browser **playground** (WASM).
- **Governance**: RFC process, code of conduct, maintainer model, release cadence.
- Prebuilt binaries + installer; nightly and stable channels.

**Exit:** someone unaffiliated with the project can install Lumo, learn it from the docs, ship a program, and trust it not to break under `1.x`.

### Phase 10 — Scale-out (`1.x` and beyond)
*From language to ecosystem.*

- **Concurrency**: async/await or lightweight tasks (RFC-driven).
- **Self-hosting**: rewrite the Lumo compiler in Lumo (the classic credibility milestone).
- **Ecosystem**: a healthy package registry, third-party libraries, conferences/community.
- **Performance leadership**: competitive with established systems languages on benchmarks.

---

## Cross-cutting tracks

These run continuously, not as one-off phases.

- **Quality & CI** — tests required for every feature; clippy + fmt gates; fuzz the parser; conformance test suite grows with the spec.
- **Performance** — a benchmark suite (compile time and runtime) from Phase 1; track regressions per commit.
- **Security & correctness** — UB-free codegen, sanitizer runs in CI, a fuzzing harness.
- **Documentation** — every user-facing feature lands with docs in the same PR.
- **Community** — issue triage, RFCs for anything that affects the language surface, transparent roadmap board.

---

## Scalability strategy (explicit)

"Scalable product" means three concrete things, each owned by phases above:

1. **Scales to large codebases** — incremental + parallel + cached compilation
   (Phase 8), and a module/package system (Phase 5) so projects don't become
   monoliths.
2. **Scales to many users** — installers, prebuilt binaries, WASM playground,
   docs, and stability guarantees (Phase 9) lower the cost of adoption.
3. **Scales to many contributors** — clear architecture, tests/CI as a contract
   (Phase 1), an RFC process and governance (Phase 9), and self-hosting (Phase 10)
   so the community can evolve the language in the language.

---

## Versioning & release policy

- `0.x`: anything may change. Frequent minor releases.
- `1.0`: language and stdlib are stable; breaking changes require a new major.
- Deprecations carry warnings for at least one minor cycle before removal.
- Two channels post-1.0: **nightly** (latest) and **stable** (vetted).

---

## Immediate next steps (next few PRs)

Phases 1–2 and the typed front end are done (see [Progress so far](#progress-so-far)).
The highest-leverage work right now, in order:

1. **Memory reclamation** — `lumo_alloc` allocations currently leak; design and
   implement an arena/region scheme (or land the heap-model decision in RFC 0001).
2. **`struct`s** — user-defined types, field access, and construction (Phase 3).
3. **Local type inference** — reduce the annotation burden the checker requires today.

Track progress on the GitHub issues/milestones for each phase.
