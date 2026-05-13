# CrdtBackend vs Document — The Two-Layer Architecture

## TL;DR

**`CrdtBackend<N, E>`** (low-level, untyped):
- Stores node/edge **structure** (parent, order_key, from/to, tombstones)
- Stores **properties** as `HashMap<String, Vec<u8>>` — opaque bytes, LWW only
- Used by: server mirrors, structural sync (AddNode, Move, AddRefEdge)

**`Document<S>`** (high-level, typed):
- Stores node **properties** as typed schema `S` (LWW, OR-Set, PN-Counter, nested CRDTs)
- Wraps the same op log, but dispatches property ops through typed `Crdt` traits
- Used by: client typed-schema plugins (Frame, Rectangle, Transform components)

**Why both?** Historical layering. `CrdtBackend` came first for structure; `Document<S>` was added in Phase H for typed properties. They **coexist** — you can use `CrdtBackend` alone for simple LWW properties, or layer `Document<S>` on top for rich CRDTs.

---

## 1 · The Confusion: Two Storage Models

When you look at the code, you see:

```rust
// In CrdtBackend (backend.rs:44)
struct NodeRecord {
    tombstoned: bool,
    order_key: Option<String>,
    tree_parent: Option<CrdtId>,
    properties: HashMap<String, Vec<u8>>,  // ← opaque bytes
}

// In Document (document.rs:44)
struct NodeRecord<S> {
    tombstoned: bool,
    schema: S,  // ← typed CRDT schema (LwwRegister, OrSet, etc.)
}
```

**These are two *different* `NodeRecord` types** in two different files. The naming collision is unfortunate but intentional — they serve parallel roles at different abstraction levels.

---

## 2 · CrdtBackend: The Foundation (Phase A-G)

**What it stores**:
- **Topology**: `nodes: HashMap<CrdtId, NodeRecord>`, `edges: HashMap<CrdtId, EdgeRecord>`
- **Tree structure**: `order_key`, `tree_parent` (Kleppmann atomic moves)
- **Reference edges**: `from`, `to`, `category`, cascade tombstones
- **Properties**: `HashMap<String, Vec<u8>>` — key-value pairs, values are postcard-encoded

**Property semantics**: **LWW only**. When you call:

```rust
backend.set_node_property(node_id, "name", postcard::to_vec(&"Alice")?);
```

The backend stores:
```rust
rec.properties.insert("name".to_string(), vec![...postcard bytes...]);
```

No CRDT primitives. Just last-write-wins: the most recent `SetNodeProperty` op (by `GlobalSeq`) overwrites the value.

**Where it's used**:
1. **Server-side mirrors** — `GraphRoomHandler` owns a `Mutex<CrdtBackend<(), ()>>` to validate ops and maintain canonical state. Properties are opaque; the server doesn't interpret them.
2. **Structural sync** — `GraphSyncPlugin` uses `ClientSyncEngine` (which wraps `CrdtBackend`) for `AddNode`, `Move`, `AddRefEdge`, `Remove*` ops. Tree shape and edge topology live here.
3. **Tests** — many graph-crdt tests use bare `CrdtBackend` for simple property checks without needing typed schemas.

**What it *doesn't* do**:
- No OR-Set (can't have "tags: Vec<String>" with add-wins semantics)
- No PN-Counter (can't have "like_count" that merges concurrent +1s)
- No nested CRDTs (can't have "style.fill" where `style` is itself a CRDT Map)

For those, you need `Document<S>`.

---

## 3 · Document: Typed Schema Layer (Phase H)

**What it stores**:
- **Per-node schema**: `nodes: HashMap<CrdtId, NodeRecord<S>>` where `S: Crdt`
- **No tree structure** — `Document` doesn't track `order_key` or `tree_parent`. Those live in `CrdtBackend`.

Wait, what? **`Document` only stores properties**, not topology. It's a **property-focused** layer on top of the structural foundation.

**Property semantics**: **Any CRDT primitive**. When you define:

```rust
#[derive(Crdt)]
pub struct FrameSchema {
    pub name: LwwRegister<String>,
    pub tags: OrSet<String>,
    pub edit_count: PnCounter,
}
```

...and call:

```rust
doc.mutate_node(node_id, FrameSchemaMut::Tags(OrSetMut::Add("urgent".into())));
```

The document:
1. Looks up `nodes[node_id].schema` (a `FrameSchema` instance)
2. Calls `schema.mutate(FrameSchemaMut::Tags(...), ctx)` → produces `FrameSchemaDelta::Tags(OrSetDelta::Add { value })`
3. Converts to `(Path, WireDelta)` → `(["tags"], WireDelta::OrSetAdd { value: postcard::to_vec("urgent") })`
4. Queues `OpKind::SetNodeProperty { target: node_id, path: ["tags"], delta: ... }`

On apply, the reverse:
1. Inbound `SetNodeProperty { path: ["tags"], delta: WireDelta::OrSetAdd {...} }`
2. Route to `schema.apply_wire(path, delta, ctx)`
3. `FrameSchema` dispatches to `self.tags.apply(OrSetDelta::Add {...}, ctx)`
4. OR-Set applies: adds `("urgent", SubDot { op: ctx.op_id, sub: 0 })` to its internal map

**Where it's used**:
1. **Client typed-schema plugins** — `SchemaSyncedNodeComponentPlugin<N, E, C>` owns a `SchemaDoc<C::Schema>` resource ([schema_sync.rs:137](../../../crates/kyoso_graph_sync/src/schema_sync.rs#L137)). Bevy components like `Frame`, `Rectangle`, `Transform` sync through `Document<FrameSchema>`, `Document<RectangleSchema>`, etc.
2. **Advanced property types** — anywhere you need OR-Set tags, PN-Counter likes, nested maps, sequences (future).

**What it *doesn't* do**:
- No `Move` ops — those target the tree structure, which `Document` doesn't manage
- No `AddNode` / `RemoveNode` — those are structural, not property-level
- No server-side use yet — servers use `CrdtBackend` mirrors. `Document` is client-side only (for now).

---

## 4 · How They Relate: Parallel or Layered?

**Originally (Phase A-G)**: They were **parallel**.
- `CrdtBackend` handled structure + LWW properties
- No `Document` existed yet

**Phase H (current)**: They **coexist** with partial overlap.
- `CrdtBackend` still handles structure + LWW properties (server mirrors, structural sync)
- `Document<S>` handles typed CRDT properties (client schema sync)

**The overlap**: Both can store node properties, but in different formats:
- `CrdtBackend`: `properties: HashMap<String, Vec<u8>>` (LWW string keys)
- `Document<S>`: `schema: S` (typed CRDT fields)

**In practice (how `kyoso_graph_sync` wires them)**:

```
┌─────────────────────────────────────────────────┐
│ Bevy ECS (Frame, Rectangle, Transform)         │
└───────────┬─────────────────────────────────────┘
            │
            ▼
┌─────────────────────────────────────────────────┐
│ SchemaSyncedNodeComponentPlugin<N, E, Frame>   │
│  owns: SchemaDoc<FrameSchema>                   │
│        (a Document<FrameSchema>)                │
└───────────┬─────────────────────────────────────┘
            │ SetNodeProperty ops (typed)
            │
            ▼
┌─────────────────────────────────────────────────┐
│ ClientSyncEngine (wraps CrdtBackend)            │
│  handles: AddNode, Move, AddRefEdge, Remove*    │
│  ALSO handles: SetNodeProperty (but only logs)  │
└───────────┬─────────────────────────────────────┘
            │ All ops
            ▼
         WsClient → Server
```

**The split**:
1. **Structural ops** (AddNode, Move, AddRefEdge, Remove*) go through `ClientSyncEngine` → `CrdtBackend`
2. **Property ops** (SetNodeProperty with typed deltas) are:
   - Generated by `Document<FrameSchema>::mutate_node`
   - Applied by `Document<FrameSchema>::apply_remote`
   - Also passed to `CrdtBackend::apply_remote`, which stores them as opaque bytes (for logging / debugging, not semantic interpretation)

**Why the CrdtBackend also sees property ops**: The backend's `apply_remote` is still the canonical sequencer (checks `seq == applied_seq + 1`, bumps `applied_seq`). Property ops flow through it to maintain the global op sequence, even though the typed deltas are interpreted by `Document`.

---

## 5 · The "Legacy" Naming Confusion

You saw `path_to_legacy_key` in `CrdtBackend` and wondered if the whole backend is legacy. **It's not.** The function name is misleading.

**What `path_to_legacy_key` does** ([backend.rs:691](../../../crates/kyoso_graph_crdt/src/backend.rs#L691)):

Before `Document<S>`, properties were single-segment keys: `"name"`, `"visible"`, `"width"`. After `Document<S>`, paths became multi-segment: `["Frame", "name"]`, `["Transform", "translation", "x"]`.

`CrdtBackend` predates multi-segment paths, so its `properties: HashMap<String, Vec<u8>>` storage is keyed by **string**, not `Path`. When a multi-segment path arrives (e.g., `["Frame", "name"]`), `path_to_legacy_key` flattens it to `"Frame/name"` for logging.

**Why "legacy"?** The backend's string-keyed storage is a **legacy design choice** from before paths existed. But the backend itself is **current and actively used** — it's the foundation for structure sync and server mirrors.

**Better name**: `path_to_flat_key` or `flatten_path_for_storage`. The "legacy" label overstates the problem.

---

## 6 · When to Use Which?

| Use Case | Use This | Why |
|---|---|---|
| Server-side mirror | `CrdtBackend<(), ()>` | Server doesn't interpret properties; opaque bytes are fine |
| Structural sync (AddNode, Move, edges) | `CrdtBackend` via `ClientSyncEngine` | Tree topology and edge graph live here |
| Simple LWW properties | `CrdtBackend` | If you just need key-value LWW, no need for schemas |
| OR-Set tags / PN-Counter / nested maps | `Document<S>` with typed schema | Need CRDT primitives beyond LWW |
| Bevy component sync | `Document<S>` via `SchemaSyncedNodeComponentPlugin` | Client-side typed property sync |
| Tests (quick structural checks) | `CrdtBackend` | Lighter weight, no schema boilerplate |
| Tests (CRDT semantics) | `Document<S>` | Verify OR-Set add-wins, PN-Counter merge, etc. |

---

## 7 · Migration Path (Future)

**Eventual goal** (not started): Unify the two layers.

**Option A** (replace CrdtBackend storage with Document):
- Make `CrdtBackend` store `nodes: HashMap<CrdtId, DocumentNodeRecord<S>>` internally
- `properties: HashMap<String, Vec<u8>>` becomes `schema: S` under the hood
- Server mirrors become `CrdtBackend<EmptySchema, EmptySchema>` where `EmptySchema` is an empty CRDT struct (no fields = no properties)

**Option B** (keep both, clarify roles):
- `CrdtBackend` → rename to `StructuralBackend` (topology + tree + edges only, no properties)
- `Document<S>` → rename to `CrdtDocument<S>` (properties only, no topology)
- Apps compose: `StructuralBackend + CrdtDocument<S>` for full graph + typed properties

**Current status**: No migration planned. The two-layer system works. "Legacy" naming is the only real issue.

---

## 8 · Quick Reference

```rust
// Structural + LWW properties (server, tests, simple cases)
let mut backend = CrdtBackend::<MyNode, MyEdge>::with_peer(peer);
backend.add_node();
backend.move_node(id, parent, pos);
backend.set_node_property(id, "name", postcard::to_vec(&"Alice")?);

// Typed CRDT properties (client schema sync)
let mut doc = Document::<FrameSchema>::with_peer(peer);
doc.mutate_node(id, FrameSchemaMut::Name(LwwMut::Set("Alice".into())));
doc.mutate_node(id, FrameSchemaMut::Tags(OrSetMut::Add("urgent".into())));
doc.mutate_node(id, FrameSchemaMut::EditCount(PnCounterMut::Inc(1)));

// In practice (client), you use BOTH via plugins:
// - ClientSyncEngine (CrdtBackend) for structure
// - SchemaDoc<FrameSchema> (Document) for typed Frame properties
```

---

## 9 · Summary: Not Legacy, Just Layered

- **`CrdtBackend`**: Foundation, handles structure + simple LWW properties. **Current and actively used.**
- **`Document<S>`**: Typed schema layer for rich CRDT properties (OR-Set, PN-Counter, nested). **Current, additive.**
- **"Legacy" naming**: Misleading. `path_to_legacy_key` refers to pre-Path string keys, not a deprecated backend.
- **Migration**: Not urgent. Renaming `path_to_legacy_key` → `flatten_path_for_storage` would help clarity, but the architecture is sound.

The confusion stems from:
1. Two `NodeRecord` types in different files
2. Overlapping responsibilities (both can store properties, in different formats)
3. Misleading "legacy" labels in function names

The system works. It's just **architecturally layered** rather than unified, which is intentional (incremental Phase H addition without breaking existing code).
