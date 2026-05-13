# Architecture Evolution — Legacy vs Current

This document clarifies what's **legacy** vs **current** in the kyoso workspace, based on the recent refactoring commits and phase markers in the code.

## TL;DR: What Changed Recently

**Before (~6 commits ago):** Monolithic `CrdtSyncPlugin<N, E>` handled both transport and graph sync.  
**After (current):** Clean separation into transport + per-model plugins for multi-model composition.

---

## 1 · The Major Refactorings (Git History)

Traced from `git log --oneline`:

### Commit 0acc1e8: "refactor(sync): split into transport-only kyoso_sync"

**What changed**: Split the monolithic sync layer into:
- **`kyoso_sync`** — model-agnostic WebSocket transport (`WsClient`, `SyncTransportPlugin`)
- **`kyoso_graph_sync`** — graph-specific plugin (`GraphSyncPlugin<N, E>`)
- **`kyoso_comments_sync`** — comments model plugin (separate)

**Before**:
```rust
App::new()
    .add_plugins(CrdtSyncPlugin::<MyNode, MyEdge>::new("ws://...", "room"))
    .run();
```

**After (current)**:
```rust
// Multi-model composition
App::new()
    .add_plugins((
        SyncTransportPlugin::new("ws://...", "room"),
        GraphSyncPlugin::<MyNode, MyEdge>::default(),
        CommentsSyncPlugin::default(),
    ))
    .run();

// Legacy convenience (single-model)
App::new()
    .add_plugins(GraphSyncPlugin::<MyNode, MyEdge>::new("ws://...", "room"))
    .run();
```

The "legacy convenience" constructor still works (bundles `SyncTransportPlugin` internally) for backward compatibility.

### Commit ab9ec7b: "refactor(server): multi-model room handler"

**What changed**: Server-side `Room` became model-agnostic. Before: one room = one model (graph). After: one room can host multiple models (graph + comments + future models).

**Architecture shift**:
- `Room` → thin router over `HashMap<ModelId, Arc<dyn RoomModelHandler>>`
- Each model handler (`GraphRoomHandler`, `CommentsRoomHandler`) owns its own:
  - Op log (`OpStore` or in-memory)
  - Server-side mirror (`CrdtBackend`)
  - `append_lock` (per-model, independent)

This is the foundation for comments, future canvas-sync, etc., all over one WebSocket.

### Commit c683d87: "refactor(figma): adapt to kyoso_graph_sync plugin layer"

**What changed**: Client apps (`kyoso_figma`, `kyoso_circuit`) migrated from the old `CrdtSyncPlugin` to `GraphSyncPlugin` + separate transport.

### Commit ead93ad: "feat(graph-crdt): backend/document refactor"

**What changed**: Split typed schema support from the raw backend:
- `CrdtBackend<N, E>` — properties as `HashMap<String, Vec<u8>>` (LWW only)
- `Document<S>` — properties as typed `S: Crdt` schema (LWW, OR-Set, PN-Counter, nested CRDTs)

This is the "Phase H" mentioned in [document.rs:9](../../../crates/kyoso_graph_crdt/src/document.rs#L9).

---

## 2 · The Phase Markers (A-H)

The derive macros and CRDT primitives evolved incrementally. These phase markers in comments track the evolution:

| Phase | What Landed | Where |
|---|---|---|
| **Phase A** | Scaffold + LWW happy path, `#[schema(name)]` | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs#L5) |
| **Phase B** | `#[crdt(skip)]`, `#[crdt(rename)]`, `#[crdt(default)]` | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs#L6) |
| **Phase C** | `#[crdt(or_set)]`, `#[crdt(counter)]` | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs#L8) |
| **Phase D** | `#[crdt(map)]`, `#[crdt(nested)]` | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs#L9) |
| **Phase E** | `#[crdt(with = "Type")]` escape hatch, per-edge-category hooks | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs#L10) |
| **Phase F** | `#[crdt(sequence)]` stub (current — see §10 Known Gaps) | [kyoso_sync_derive](../../../crates/kyoso_sync_derive/src/lib.rs#L11) |
| **Phase G** | Base CRDT primitives (`LwwRegister`, `OrSet`, `PnCounter`, `CausalMap`) | [kyoso_crdt::types](../../../crates/kyoso_crdt/src/types/mod.rs#L9) |
| **Phase H** | Typed schema layer (`Document<S>`) on top of `CrdtBackend` | [document.rs:9](../../../crates/kyoso_graph_crdt/src/document.rs#L9) |

**Current state**: Phases A–H are landed. Phase F's `Sequence<T>` is a stub (see Known Gaps in `crdt-overview.md §10`).

These phases are **not legacy** — they're historical markers showing the incremental build-up of the current architecture. Code referencing "Phase X" is explaining *when* a feature landed, not that it's old.

---

## 3 · Legacy Artifacts Still in the Code

### 3.1 `path_to_legacy_key` ([backend.rs:691](../../../crates/kyoso_graph_crdt/src/backend.rs#L691))

**What it is**: `CrdtBackend` predates multi-segment paths (`Path = Vec<PathSegment>`). Property storage was simple `HashMap<String, Vec<u8>>`. When the schema layer added nested properties (e.g., `["Transform", "translation", "x"]`), the backend's LWW dispatcher needed a way to flatten paths to keys for logging.

**Legacy status**: The `CrdtBackend` property bag is still LWW-only and keyed by string. The typed schema layer (`Document<S>`) routes multi-segment paths correctly, but the backend's internal storage hasn't migrated to a path-addressed tree.

**Impact**: None for consumers. Typed schema plugins use `Document<S>`, which handles paths correctly. The "legacy key" is an internal detail for `CrdtBackend`'s LWW dispatch.

**Migration path (if ever needed)**: Replace `properties: HashMap<String, Vec<u8>>` with `properties: PathTree<Vec<u8>>` where `PathTree` is a prefix-tree keyed by `Path`. Not urgent — the current abstraction works.

### 3.2 References to `CrdtSyncPlugin` in comments

**Where**: [kyoso_figma/src/lib.rs](../../../crates/kyoso_figma/src/lib.rs#L13), [kyoso_client/src/scene.rs](../../../apps/kyoso_client/src/scene.rs#L8).

**Legacy status**: Docstring references that haven't been updated to say `GraphSyncPlugin<N, E>`.

**Impact**: Confusing for new readers. The code itself is current; just the docs lag.

**Fix**: Global search-replace `CrdtSyncPlugin` → `GraphSyncPlugin` in comments.

### 3.3 "Legacy convenience" constructor ([plugin.rs:71](../../../crates/kyoso_graph_sync/src/plugin.rs#L71))

**What it is**: `GraphSyncPlugin::new(url, room)` bundles `SyncTransportPlugin` for single-model apps that don't need multi-model composition.

**Legacy status**: Not legacy — it's a **convenience API** for simple cases. Multi-model apps use the explicit `SyncTransportPlugin + GraphSyncPlugin::default()` composition; single-model apps use `GraphSyncPlugin::new(...)`.

**Impact**: None. Both approaches are current and correct. The "legacy" label in the comment is historical (it mirrors the old `CrdtSyncPlugin::new` API for migration ease), but the implementation is clean.

---

## 4 · What's Current (As of Latest Commits)

### Current architecture (post-refactor)

```
┌─────────────────────────────────────┐
│ Apps (kyoso_client, kyoso_circuit)  │
└───────────┬─────────────────────────┘
            │
            ▼
┌─────────────────────────────────────┐
│ Domain plugins                      │
│  - kyoso_figma (bundles graph sync) │
│  - kyoso_circuit (bundles graph)    │
└───────────┬─────────────────────────┘
            │
            ▼
┌──────────────────────────────────────────────────┐
│ Per-model sync plugins (composable)              │
│  ├─ GraphSyncPlugin<N, E>    (kyoso_graph_sync)  │
│  └─ CommentsSyncPlugin       (kyoso_comments_sync)│
└───────────┬──────────────────────────────────────┘
            │
            ▼
┌─────────────────────────────────────┐
│ SyncTransportPlugin (kyoso_sync)    │
│  - WsClient (model-agnostic)        │
│  - ModelRegistry                    │
│  - Envelope protocol                │
└───────────┬─────────────────────────┘
            │ WebSocket
            ▼
┌─────────────────────────────────────┐
│ kyoso_server                        │
│  ├─ Room (model-agnostic router)    │
│  ├─ GraphRoomHandler                │
│  └─ CommentsRoomHandler             │
└─────────────────────────────────────┘
```

**Clean separation**: Transport, per-model logic, and domain-specific wiring are independent layers.

### Current CRDT substrate

- **Identity**: Shared `IdGen` across models (§2.1, §2.1.1 in `crdt-overview.md`)
- **Causality**: `CausalContext` with SubDot derivation (§2.4, §2.4.1)
- **Primitives**: `LwwRegister`, `OrSet`, `PnCounter`, `CausalMap` (Phase G)
- **Schema derive**: `#[derive(Crdt)]` for schemas, `#[derive(SchemaSync)]` for Bevy components (Phases A-E)
- **Composition**: `Document<S>` for typed properties on top of `CrdtBackend` (Phase H)

### Current server

- **Multi-model rooms**: One `Room` hosts N models, each with its own `RoomModelHandler`
- **Independent append locks**: Graph and comments don't block each other
- **Envelope protocol**: `EnvelopeClientMsg` / `EnvelopeServerMsg` with `ModelId` tags

---

## 5 · Migration Checklist (If You're Updating Old Code)

If you have code written before commit `0acc1e8`, migrate like this:

### Replace old monolithic plugin

**Old**:
```rust
use kyoso_sync::CrdtSyncPlugin;

App::new()
    .add_plugins(CrdtSyncPlugin::<MyNode, MyEdge>::new("ws://...", "room"))
```

**New (single-model convenience)**:
```rust
use kyoso_graph_sync::GraphSyncPlugin;

App::new()
    .add_plugins(GraphSyncPlugin::<MyNode, MyEdge>::new("ws://...", "room"))
```

**New (multi-model explicit)**:
```rust
use kyoso_sync::SyncTransportPlugin;
use kyoso_graph_sync::GraphSyncPlugin;
use kyoso_comments_sync::CommentsSyncPlugin;

App::new()
    .add_plugins((
        SyncTransportPlugin::new("ws://...", "room"),
        GraphSyncPlugin::<MyNode, MyEdge>::default(),
        CommentsSyncPlugin::default(),
    ))
```

### Update docstring references

Global search-replace in comments:
- `CrdtSyncPlugin` → `GraphSyncPlugin`
- "the sync plugin" → "the graph sync plugin" (when graph-specific)

---

## 6 · What to Ignore (Phase Markers Aren't Legacy)

When you see comments like:

```rust
// Phase H ships this as an additional layer over CrdtBackend
```

...that's **not legacy**. It's documenting *when* the feature landed in the incremental build-up. All phases A–H are current and in production use.

The only truly legacy artifact is `path_to_legacy_key`, which is an internal backend detail that doesn't affect consumers.

---

## 7 · Summary Table: Legacy vs Current

| Artifact | Status | Migration Needed? |
|---|---|---|
| `CrdtSyncPlugin` type | **Legacy** (renamed to `GraphSyncPlugin`) | Import from `kyoso_graph_sync`, not `kyoso_sync` |
| `CrdtSyncPlugin` references in docs | **Legacy** | Update comments to say `GraphSyncPlugin` |
| `path_to_legacy_key` in backend | **Legacy** (internal detail) | No (consumers use typed schema) |
| Monolithic sync plugin | **Legacy** (pre-refactor) | Use `SyncTransportPlugin + GraphSyncPlugin` |
| Phase A-H markers in comments | **Current** (historical docs) | No (these are just timestamps, not old code) |
| `GraphSyncPlugin::new(url, room)` "legacy convenience" | **Current** (misnomer) | No (it's a convenience API, not legacy) |
| Multi-model `Room` architecture | **Current** | No |
| `Document<S>` typed schema layer | **Current** (Phase H) | No |
| Shared `IdGen` across models | **Current** | No |
| Envelope protocol with `ModelId` | **Current** | No |

---

## 8 · For New Contributors

**Don't worry about phases** — they're just historical markers showing the order features landed. All of them are current.

**Do use the multi-model APIs** — if you're adding a new replicated model (e.g., canvas strokes, comments, presence), follow the pattern:
1. Implement `CrdtModel` in `kyoso_your_model_crdt`
2. Write a Bevy plugin in `kyoso_your_model_sync`
3. Register the model in `ModelRegistry` via `SyncTransportPlugin`
4. Write a `YourModelRoomHandler` for the server

See [`kyoso_comments_crdt`](../../../crates/kyoso_comments_crdt/) + [`kyoso_comments_sync`](../../../crates/kyoso_comments_sync/) + [`handlers/comments.rs`](../../../apps/kyoso_server/src/services/handlers/comments.rs) as the reference.

---

## 9 · Questions This Doc Answers

- **"Why do I see both `CrdtSyncPlugin` and `GraphSyncPlugin`?"** → Old name vs new name. Use `GraphSyncPlugin`.
- **"What's 'Phase H'?"** → Historical marker for when typed schema (`Document<S>`) landed. It's current, not legacy.
- **"Is the monolithic sync plugin still supported?"** → No, it was split in commit `0acc1e8`. Use `SyncTransportPlugin + GraphSyncPlugin`.
- **"What's `path_to_legacy_key`?"** → Internal backend detail from before multi-segment paths. Doesn't affect typed schema consumers.
- **"Should I use `GraphSyncPlugin::new(...)` or `SyncTransportPlugin + GraphSyncPlugin::default()`?"** → Both are current. Use `::new(...)` for single-model apps, explicit composition for multi-model.
