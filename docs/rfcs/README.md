# Lumo RFCs

Design proposals for changes that affect the language surface, the runtime, or
the compiler architecture. An RFC lays out the design space, makes a concrete
recommendation, and invites pushback **before** implementation begins.

| RFC | Title | Status |
| --- | --- | --- |
| [0001](0001-memory-model.md) | Memory-management strategy (arena vs GC vs ownership) | Proposed (draft) |
| [0002](0002-map-type.md) | A built-in `map` (associative array) type | Implemented (v0.24.0) |
| [0003](0003-module-system.md) | A module system (`import`) for multi-file programs | v1 implemented (v0.35.0) |

## Process

1. Copy the structure of an existing RFC (summary → motivation → constraints →
   detailed design with options → recommendation → drawbacks → alternatives →
   rollout → unresolved questions).
2. Number it sequentially (`000N-short-title.md`) and add a row above.
3. Open it as a PR for discussion; link a tracking issue for comments.
4. RFCs are living drafts until their feature ships; update Status as they move.
