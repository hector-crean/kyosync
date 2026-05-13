# Refactoring Plan: Unified Backend Architecture

## Goal

Eliminate the confusing CrdtBackend/Document split by creating **one cohesive storage model** that handles both structure and typed properties.

## Current Problems

1. **Two `NodeRecord` types** — `CrdtBackend::NodeRecord` vs `Document::NodeRecord<S>`
2. **Duplicate storage** — properties as `HashMap<String, Vec<u8>>` (opaque) vs typed `S: Crdt`
3. **Misleading "legacy" naming** — `path_to_legacy_key` implies CrdtBackend is deprecated
4. **Unclear separation** — when to use which? Both handle properties differently
5. **Server uses opaque storage** — no benefit from typed schemas on the server side

## Proposed Architecture: Unified `GraphBackend<S>`

### Core Idea

Make **`Document<S>` the foundation**, add structure (tree, edges) to it, deprecate `CrdtBackend`.

```rust
/// Unified graph storage: structure + typed properties
pub struct GraphBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    ids: IdGen,
    /// Nodes with typed schema
    nodes: HashMap<CrdtId, NodeRecord<S>>,
    /// Edges (no schema — structure only)
    edges: HashMap<CrdtId, EdgeRecord>,
    pending: Vec<Op<OpKind>>,
    pending_moves: HashMap<CrdtId, CrdtId>,
    applied_seq: GlobalSeq,
    causal: CausalState,
}

struct NodeRecord<S> {
    tombstoned: bool,
    /// Tree structure
    order_key: Option<String>,
    tree_parent: Option<CrdtId>,
    /// Typed CRDT properties
    schema: S,
}

struct EdgeRecord {
    from: CrdtId,
    to: CrdtId,
    category: EdgeCategory,
    tombstoned: bool,
    // Edges have no properties in current design
}
```

**Key changes:**
- Merge structure (tree_parent, order_key) into `Document`'s NodeRecord
- Remove `properties: HashMap<String, Vec<u8>>` — everything goes through typed `S`
- Server uses `GraphBackend<EmptySchema>` where `EmptySchema` has zero fields

### EmptySchema for Server Mirrors

```rust
#[derive(Crdt, Default, Clone, Debug, PartialEq)]
pub struct EmptySchema;

// Generates:
// - EmptySchemaMut (zero variants, uninhabited)
// - EmptySchemaDelta (zero variants, uninhabited)
// - Lattice impl (trivial)
// - Crdt impl (apply/mutate unreachable, schema has no fields)
```

Server mirrors become `GraphBackend<EmptySchema>` — structure-only, no property interpretation.

## Migration Steps

### Phase 1: Introduce `GraphBackend<S>` (New Type)

**File**: `crates/kyoso_graph_crdt/src/unified_backend.rs`

1. Copy `Document<S>` structure
2. Add structure fields (`edges`, `pending_moves`, tree_parent/order_key)
3. Implement all `CrdtBackend` topology methods (`add_node`, `move_node`, `add_ref_edge`, etc.)
4. Implement `CrdtModel` trait
5. Keep `mutate_node` / `apply_property_op` from `Document`

**Result**: `GraphBackend<S>` does everything CrdtBackend + Document do, in one type.

### Phase 2: Migrate Client to `GraphBackend<FrameSchema>`

**File**: `crates/kyoso_graph_sync/src/engine.rs`

Replace:
```rust
pub(crate) struct ClientSyncEngine {
    backend: CrdtBackend<(), ()>,  // old
    just_projected: HashSet<CrdtId>,
}
```

With:
```rust
pub(crate) struct ClientSyncEngine<S> 
where S: Crdt + SchemaApply + Default, S::Delta: IntoWireOp
{
    backend: GraphBackend<S>,  // new
    just_projected: HashSet<CrdtId>,
}
```

**File**: `crates/kyoso_graph_sync/src/plugin.rs`

```rust
pub struct GraphSyncPlugin<N, E, S> {  // add S type param
    transport: Option<(String, String)>,
    _phantom: PhantomData<fn() -> (N, E, S)>,
}
```

**File**: `crates/kyoso_graph_sync/src/schema_sync.rs`

- Remove `SchemaDoc<C::Schema>` resource (no longer needed)
- `ClientSyncEngine<C::Schema>` now owns the typed schema state
- Detection and projection systems work directly with `backend.nodes[id].schema`

**Domain plugins update**:
```rust
// Old
App::new()
    .add_plugins(GraphSyncPlugin::<FigmaNode, FigmaEdge>::default())
    .add_plugins(SchemaSyncedNodeComponentPlugin::<FigmaNode, FigmaEdge, Frame>::default())

// New
App::new()
    .add_plugins(GraphSyncPlugin::<FigmaNode, FigmaEdge, FrameSchema>::default())
    // Frame component auto-syncs via FrameSchema, no separate plugin needed
```

### Phase 3: Migrate Server to `GraphBackend<EmptySchema>`

**File**: `apps/kyoso_server/src/services/handlers/graph.rs`

Replace:
```rust
type ServerMirror = CrdtBackend<(), ()>;  // old
```

With:
```rust
type ServerMirror = GraphBackend<EmptySchema>;  // new
```

**Define EmptySchema**:
```rust
// In kyoso_graph_crdt/src/lib.rs
#[derive(Crdt, Default, Clone, Debug, PartialEq)]
pub struct EmptySchema;
```

Server mirrors don't interpret properties (they stay opaque bytes in snapshots), but the storage is unified.

### Phase 4: Remove Old Types

**Delete**:
- `crates/kyoso_graph_crdt/src/backend.rs` → delete entire file (old `CrdtBackend`)
- `crates/kyoso_graph_crdt/src/document.rs` → delete (merged into `unified_backend.rs`)
- `path_to_legacy_key` function → gone
- `properties: HashMap<String, Vec<u8>>` storage → gone

**Rename**:
- `unified_backend.rs` → `backend.rs`
- `GraphBackend<S>` → maybe keep this name (clearer than `CrdtBackend`)

### Phase 5: Update Snapshots

**Problem**: Current snapshots serialize `properties: HashMap<String, Vec<u8>>`.

**New format**:
```rust
pub struct Snapshot<S> {
    pub at_seq: GlobalSeq,
    pub nodes: Vec<NodeSnap<S>>,
    pub edges: Vec<EdgeSnap>,
}

pub struct NodeSnap<S> {
    pub id: CrdtId,
    pub order_key: Option<String>,
    pub tree_parent: Option<CrdtId>,
    pub schema: S,  // typed, postcard-serialized
}
```

**Migration**: Snapshots are per-room. Old rooms keep old format; new rooms use new format. Or: run a migration script on the Postgres `snapshots` table (decode old, re-encode new).

### Phase 6: Update Tests

**Every test file** that uses `CrdtBackend::<(), ()>::with_peer(...)`:

Replace with:
```rust
GraphBackend::<EmptySchema>::with_peer(...)
```

Or for tests that need typed properties:
```rust
#[derive(Crdt, Default, Clone, Debug, PartialEq)]
struct TestSchema {
    name: LwwRegister<String>,
    tags: OrSet<String>,
}

let mut backend = GraphBackend::<TestSchema>::with_peer(1);
backend.add_node();
backend.mutate_node(id, TestSchemaMut::Name(LwwMut::Set("Alice".into())));
```

### Phase 7: Documentation Cleanup

**Update**:
- `crdt-overview.md` — remove Document vs CrdtBackend section, explain unified `GraphBackend<S>`
- `backend-vs-document.md` — mark as historical, explain the migration
- `architecture-evolution.md` — add "Phase I: Unified Backend" entry

**Docstring cleanup**:
- `CrdtSyncPlugin` → `GraphSyncPlugin` (6 occurrences)
- Remove all "legacy" mentions

## Benefits

✅ **One storage model** — no confusion about which type to use  
✅ **Type safety everywhere** — server uses `EmptySchema`, clients use typed schemas  
✅ **Simpler plugin API** — `GraphSyncPlugin<N, E, S>` bundles everything  
✅ **No "legacy" naming** — `path_to_legacy_key` gone, `properties: HashMap` gone  
✅ **Cleaner tests** — one backend type for all test scenarios  
✅ **Future-proof** — adding properties to edges? Add `EdgeSchema` type param  

## Risks & Mitigations

### Risk 1: Breaking Existing Snapshots

**Mitigation**: 
- Keep old snapshot deserialization code for migration
- Add a `SnapshotVersion` discriminant to detect format
- Server can auto-migrate old snapshots on load

### Risk 2: Type Param Complexity

**Before**: `GraphSyncPlugin<FigmaNode, FigmaEdge>`  
**After**: `GraphSyncPlugin<FigmaNode, FigmaEdge, FrameSchema>`

**Mitigation**:
- Provide type alias: `type FigmaGraphSync = GraphSyncPlugin<FigmaNode, FigmaEdge, FrameSchema>`
- Or use default: `GraphSyncPlugin<N, E, S = EmptySchema>` for structural-only use

### Risk 3: Test Churn

**Every test** that creates a backend needs to specify schema.

**Mitigation**:
- Make `EmptySchema` the default: `GraphBackend<S = EmptySchema>`
- Tests that don't care about properties continue to work with minimal changes

### Risk 4: Performance

**Question**: Does typed schema add overhead vs opaque bytes?

**Answer**: Negligible. Postcard serialization is equally efficient. Schema dispatch is compile-time resolved (monomorphization).

## Timeline Estimate

| Phase | Effort | Blocking |
|---|---|---|
| 1. Introduce GraphBackend<S> | 2-3 days | No (parallel with existing code) |
| 2. Migrate client | 1 day | Phase 1 complete |
| 3. Migrate server | 0.5 day | Phase 1 complete |
| 4. Remove old types | 0.5 day | Phases 2+3 complete |
| 5. Update snapshots | 1 day | Migration strategy decision |
| 6. Update tests | 1-2 days | Can be incremental |
| 7. Documentation | 0.5 day | Any time |

**Total**: ~1 week (assuming no major blockers)

## Alternative: Keep Both, Rename Clearly

If unification is too invasive, we could instead:

1. **Rename**: `CrdtBackend` → `StructuralBackend` (topology only, no properties)
2. **Rename**: `Document<S>` → `PropertyBackend<S>` (properties only, no structure)
3. **Compose**: Apps use both explicitly: `StructuralBackend + PropertyBackend<S>`

**Pros**: Less risky, smaller diff  
**Cons**: Keeps the dual-storage confusion, just with clearer names

## Recommendation

**Go with full unification** (`GraphBackend<S>`). The architecture will be:
- Clearer for new contributors
- More type-safe (EmptySchema vs opaque bytes)
- Easier to extend (add edge properties later as `GraphBackend<NodeSchema, EdgeSchema>`)
- Removes all "legacy" confusion permanently

The 1-week investment pays off in long-term maintainability.

## Next Steps

1. ✅ User approval of this plan
2. Create `GraphBackend<S>` in `unified_backend.rs`
3. Add `EmptySchema` derive
4. Migrate one test as proof-of-concept
5. Migrate client sync layer
6. Migrate server
7. Delete old code
8. Update docs

---

**Decision point**: Should we proceed with full unification, or the lighter "rename for clarity" approach?
