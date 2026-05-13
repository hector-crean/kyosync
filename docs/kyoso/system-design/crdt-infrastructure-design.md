# CRDT Infrastructure Design — Reusable Patterns

## Goal

Make it **easy to create new CRDT structures** (canvas, whiteboard, rich text doc, spreadsheet) by following a clean pattern. The graph CRDT should be **one instance** of this pattern, not a special case.

## Current Infrastructure (Already Reusable)

These layers are **domain-agnostic** and work for any CRDT:

### Layer 1: Core Primitives (`kyoso_crdt`)

**Already perfect** — no changes needed:

```rust
// Identity
pub struct CrdtId { peer: u32, seq: u64 }
pub struct IdGen(Arc<Mutex<IdGenerator>>);

// CRDT primitives (work for any domain)
pub struct LwwRegister<T>;
pub struct OrSet<T>;
pub struct PnCounter;
pub struct CausalMap<K, V: Crdt>;
pub struct Sequence<T>;  // (stub, will be Fugue)

// Composition
pub trait Lattice { fn join(&mut self, other: Self); }
pub trait Crdt: Lattice {
    type Mutation;
    type Delta;
    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext);
    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta;
}

// Wire format (domain-agnostic)
pub enum WireDelta {
    LwwReplace { value: Vec<u8> },
    OrSetAdd { value: Vec<u8> },
    OrSetRemove { observed: Vec<SubDot> },
    PnCounterDelta { by: i64 },
    SequenceInsert { predecessor: Option<SubDot>, value: Vec<u8> },
    SequenceDelete { targets: Vec<SubDot> },
    MapPut { key: PathSegment, inner: Box<WireDelta> },
    MapRemove { key: PathSegment, observed: Vec<SubDot> },
}
```

**Pattern**: Any domain can use these primitives. Graph uses them for node properties; comments use them for comment bodies; canvas will use them for stroke properties, etc.

### Layer 2: Model Abstraction (`kyoso_crdt::CrdtModel`)

**Already reusable** — every CRDT structure implements this:

```rust
pub trait CrdtModel {
    type OpKind;
    type State;
    
    fn set_peer(&mut self, peer: PeerId);
    fn applied_seq(&self) -> GlobalSeq;
    fn apply_remote(&mut self, op: &Op<Self::OpKind>) -> Result<(), ApplyError>;
    fn snapshot(&self) -> Self::State;
    fn restore(&mut self, snap: Self::State);
    fn drain_pending(&mut self) -> Vec<Op<Self::OpKind>>;
    fn op_kind_label(op: &Self::OpKind) -> &'static str;
}
```

**Pattern**: 
- **Graph** implements `CrdtModel` with `OpKind = GraphOpKind`, `State = GraphSnapshot`
- **Comments** implements `CrdtModel` with `OpKind = CommentOpKind`, `State = CommentsSnapshot`
- **Future canvas** implements `CrdtModel` with `OpKind = CanvasOpKind`, `State = CanvasSnapshot`

### Layer 3: Transport (`kyoso_sync`)

**Already multi-model** — routes by `ModelId`:

```rust
// Model-agnostic envelope
pub enum EnvelopeClientMsg {
    Hello { room: RoomId, models: Vec<(ModelId, GlobalSeq)> },
    Submit { model: ModelId, payload: Vec<u8> },
    // ...
}

// Model registry
pub struct ModelRegistry {
    models: Vec<ModelId>,
}
```

**Pattern**: Each CRDT registers a `ModelId` ("graph", "comments", "canvas"). Transport multiplexes over one WebSocket.

### Layer 4: Server (`apps/kyoso_server`)

**Already extensible** — per-model handlers:

```rust
pub trait RoomModelHandler {
    fn model_id(&self) -> ModelId;
    fn submit(&self, payload: Vec<u8>) -> Result<Vec<u8>>;
    fn welcome_for(&self, since: GlobalSeq) -> Result<(Option<Vec<u8>>, Vec<u8>)>;
    // ...
}

// Graph handler
pub struct GraphRoomHandler { /* ... */ }
impl RoomModelHandler for GraphRoomHandler { /* ... */ }

// Comments handler
pub struct CommentsRoomHandler { /* ... */ }
impl RoomModelHandler for CommentsRoomHandler { /* ... */ }
```

**Pattern**: Add a new CRDT → implement `RoomModelHandler` for it.

---

## Problem: Graph-Specific Coupling

The **graph CRDT** is currently tightly coupled to specific concepts:

```rust
// Graph-specific ops
pub enum OpKind {
    AddNode,
    RemoveNode { target: CrdtId },
    Move { target: CrdtId, new_parent: Option<CrdtId>, position: String },
    AddRefEdge { category: EdgeCategory, from: CrdtId, to: CrdtId },
    RemoveRefEdge { target: CrdtId },
    SetNodeProperty { target: CrdtId, path: Path, delta: WireDelta },
    SetRefEdgeProperty { target: CrdtId, path: Path, delta: WireDelta },
}
```

This works great for **hierarchical graph structures** (Figma, circuit designer), but what if you want:

- **Canvas** (flat list of strokes, no tree)
- **Rich text document** (Fugue sequence, no nodes/edges)
- **Spreadsheet** (grid of cells, no tree)
- **Whiteboard** (mix of shapes, connectors, sticky notes)

---

## Solution: Generic Backend Pattern

Make `GraphBackend<S>` an instance of a **reusable backend pattern**.

### Pattern: Backend = Topology + Schema

```rust
/// Generic CRDT backend template
pub struct Backend<T, S> {
    ids: IdGen,
    
    /// Domain-specific topology (nodes/edges, strokes, cells, etc.)
    topology: T,
    
    /// Schema state per entity (CRDT properties)
    schemas: HashMap<CrdtId, S>,
    
    pending: Vec<Op<T::OpKind>>,
    applied_seq: GlobalSeq,
    causal: CausalState,
}

/// Topology abstraction
pub trait Topology {
    type OpKind;
    type SnapshotState;
    
    fn apply_structural_op(&mut self, op: &Self::OpKind, ctx: &CausalContext);
    fn snapshot(&self) -> Self::SnapshotState;
    fn restore(&mut self, snap: Self::SnapshotState);
}
```

### Graph as an Instance

```rust
/// Graph-specific topology (tree + reference edges)
pub struct GraphTopology {
    nodes: HashMap<CrdtId, NodeStructure>,
    edges: HashMap<CrdtId, EdgeStructure>,
    pending_moves: HashMap<CrdtId, CrdtId>,
}

struct NodeStructure {
    tombstoned: bool,
    order_key: Option<String>,
    tree_parent: Option<CrdtId>,
}

struct EdgeStructure {
    from: CrdtId,
    to: CrdtId,
    category: EdgeCategory,
    tombstoned: bool,
}

impl Topology for GraphTopology {
    type OpKind = GraphOpKind;
    type SnapshotState = GraphSnapshot;
    
    fn apply_structural_op(&mut self, op: &GraphOpKind, ctx: &CausalContext) {
        match op {
            GraphOpKind::AddNode => { /* ... */ },
            GraphOpKind::Move { .. } => { /* ... */ },
            GraphOpKind::AddRefEdge { .. } => { /* ... */ },
            // SetNodeProperty handled by Backend<GraphTopology, S>
        }
    }
}

/// Concrete graph backend
pub type GraphBackend<S> = Backend<GraphTopology, S>;
```

### Canvas as Another Instance

```rust
/// Canvas-specific topology (flat list of strokes)
pub struct CanvasTopology {
    strokes: HashMap<CrdtId, StrokeStructure>,
}

struct StrokeStructure {
    tombstoned: bool,
    z_index: u32,
}

pub enum CanvasOpKind {
    AddStroke,
    RemoveStroke { target: CrdtId },
    SetStrokeProperty { target: CrdtId, path: Path, delta: WireDelta },
    ReorderStroke { target: CrdtId, new_z: u32 },
}

impl Topology for CanvasTopology {
    type OpKind = CanvasOpKind;
    type SnapshotState = CanvasSnapshot;
    
    fn apply_structural_op(&mut self, op: &CanvasOpKind, ctx: &CausalContext) {
        match op {
            CanvasOpKind::AddStroke => { /* ... */ },
            CanvasOpKind::ReorderStroke { .. } => { /* ... */ },
            // ...
        }
    }
}

/// Concrete canvas backend
pub type CanvasBackend<S> = Backend<CanvasTopology, S>;
```

### Rich Text Document as Yet Another Instance

```rust
/// Document topology (single Fugue sequence)
pub struct DocumentTopology {
    text: FugueString,
}

pub enum DocumentOpKind {
    InsertText { position: FuguePosition, chars: Vec<char> },
    DeleteText { range: FugueRange },
    SetDocProperty { path: Path, delta: WireDelta },  // title, author, etc.
}

impl Topology for DocumentTopology {
    type OpKind = DocumentOpKind;
    type SnapshotState = DocumentSnapshot;
    
    fn apply_structural_op(&mut self, op: &DocumentOpKind, ctx: &CausalContext) {
        match op {
            DocumentOpKind::InsertText { position, chars } => {
                self.text.insert(*position, chars);
            }
            DocumentOpKind::DeleteText { range } => {
                self.text.delete(*range);
            }
            // ...
        }
    }
}

pub type DocumentBackend<S> = Backend<DocumentTopology, S>;
```

---

## Refactored Architecture

```
kyoso_crdt/
├── primitives (LwwRegister, OrSet, PnCounter, CausalMap, Sequence)
├── traits (Lattice, Crdt, CrdtModel, Topology)
├── backend.rs (generic Backend<T, S>)
├── id.rs
├── context.rs
├── delta.rs
└── envelope.rs

kyoso_graph_crdt/
├── topology.rs (GraphTopology impl)
├── op.rs (GraphOpKind enum)
├── backend.rs (type GraphBackend<S> = Backend<GraphTopology, S>)
└── snapshot.rs

kyoso_canvas_crdt/  (future)
├── topology.rs (CanvasTopology impl)
├── op.rs (CanvasOpKind enum)
├── backend.rs (type CanvasBackend<S> = Backend<CanvasTopology, S>)
└── snapshot.rs

kyoso_document_crdt/  (future)
├── topology.rs (DocumentTopology impl, wraps FugueString)
├── op.rs (DocumentOpKind enum)
├── backend.rs (type DocumentBackend<S> = Backend<DocumentTopology, S>)
└── snapshot.rs
```

---

## How to Create a New CRDT Structure

**Example**: Add a **whiteboard** CRDT (mix of sticky notes, shapes, connectors)

### Step 1: Define Topology

```rust
// crates/kyoso_whiteboard_crdt/src/topology.rs

pub struct WhiteboardTopology {
    elements: HashMap<CrdtId, ElementStructure>,
    connectors: HashMap<CrdtId, ConnectorStructure>,
}

struct ElementStructure {
    tombstoned: bool,
    kind: ElementKind,  // StickyNote, Rectangle, Circle
    z_index: u32,
}

struct ConnectorStructure {
    from: CrdtId,
    to: CrdtId,
    tombstoned: bool,
}
```

### Step 2: Define OpKind

```rust
// crates/kyoso_whiteboard_crdt/src/op.rs

pub enum WhiteboardOpKind {
    AddElement { kind: ElementKind },
    RemoveElement { target: CrdtId },
    AddConnector { from: CrdtId, to: CrdtId },
    RemoveConnector { target: CrdtId },
    SetElementProperty { target: CrdtId, path: Path, delta: WireDelta },
    Reorder { target: CrdtId, new_z: u32 },
}
```

### Step 3: Implement Topology

```rust
impl Topology for WhiteboardTopology {
    type OpKind = WhiteboardOpKind;
    type SnapshotState = WhiteboardSnapshot;
    
    fn apply_structural_op(&mut self, op: &WhiteboardOpKind, ctx: &CausalContext) {
        match op {
            WhiteboardOpKind::AddElement { kind } => {
                self.elements.insert(ctx.op_id, ElementStructure {
                    tombstoned: false,
                    kind: *kind,
                    z_index: self.elements.len() as u32,
                });
            }
            WhiteboardOpKind::AddConnector { from, to } => {
                self.connectors.insert(ctx.op_id, ConnectorStructure {
                    from: *from,
                    to: *to,
                    tombstoned: false,
                });
            }
            // ...
        }
    }
    
    fn snapshot(&self) -> WhiteboardSnapshot {
        WhiteboardSnapshot {
            elements: self.elements.iter()
                .filter(|(_, e)| !e.tombstoned)
                .map(|(id, e)| (*id, e.clone()))
                .collect(),
            connectors: self.connectors.iter()
                .filter(|(_, c)| !c.tombstoned)
                .map(|(id, c)| (*id, c.clone()))
                .collect(),
        }
    }
    
    fn restore(&mut self, snap: WhiteboardSnapshot) {
        self.elements = snap.elements.into_iter().collect();
        self.connectors = snap.connectors.into_iter().collect();
    }
}
```

### Step 4: Type Alias

```rust
// crates/kyoso_whiteboard_crdt/src/backend.rs

pub type WhiteboardBackend<S> = Backend<WhiteboardTopology, S>;
```

### Step 5: Implement CrdtModel

```rust
impl<S> CrdtModel for WhiteboardBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    type OpKind = WhiteboardOpKind;
    type State = WhiteboardSnapshot;
    
    // ... delegate to Backend<T, S> generic impl
}
```

### Step 6: Add Sync Plugin

```rust
// crates/kyoso_whiteboard_sync/src/plugin.rs

pub struct WhiteboardSyncPlugin<S> {
    transport: Option<(String, String)>,
    _phantom: PhantomData<S>,
}

impl<S> Plugin for WhiteboardSyncPlugin<S>
where S: Crdt + SchemaApply + Default + Send + Sync + 'static,
      S::Delta: IntoWireOp,
{
    fn build(&self, app: &mut App) {
        app.init_resource::<WhiteboardEngine<S>>();
        app.add_systems(Update, (
            whiteboard_inbound_system::<S>,
            detect_added_elements::<S>,
            detect_removed_elements::<S>,
            outbound_system::<S>,
        ).chain());
    }
}
```

### Step 7: Add Server Handler

```rust
// apps/kyoso_server/src/services/handlers/whiteboard.rs

pub struct WhiteboardRoomHandler {
    room_id: RoomId,
    store: OpStore,
    mirror: Mutex<WhiteboardBackend<EmptySchema>>,
    append_lock: Mutex<()>,
}

impl RoomModelHandler for WhiteboardRoomHandler {
    fn model_id(&self) -> ModelId { "whiteboard".into() }
    
    async fn submit(&self, payload: Vec<u8>) -> Result<Vec<u8>> {
        let op: Op<WhiteboardOpKind> = postcard::from_bytes(&payload)?;
        let _guard = self.append_lock.lock().await;
        let stamped = self.store.append(&self.room_id, op).await?;
        self.mirror.lock().await.apply_remote(&stamped)?;
        Ok(postcard::to_allocvec(&stamped)?)
    }
    
    // ... welcome_for, snapshot, etc.
}
```

### Step 8: Done!

You now have a **fully functional whiteboard CRDT** following the same pattern as graph and comments.

---

## Benefits of This Design

✅ **Reusable primitives** — LWW, OR-Set, PN-Counter work across all domains  
✅ **Clear separation** — Topology (structure) vs Schema (properties)  
✅ **Easy to add models** — follow 8-step pattern above  
✅ **Type-safe** — `Backend<GraphTopology, FrameSchema>` vs `Backend<CanvasTopology, StrokeSchema>`  
✅ **Multi-model rooms** — graph + canvas + comments over one WebSocket  
✅ **Testable** — each topology can be tested independently  
✅ **Future-proof** — adding edge properties → `Backend<T, NodeSchema, EdgeSchema>`  

---

## Implementation Plan

### Phase 1: Extract Generic Backend<T, S>

**File**: `crates/kyoso_crdt/src/backend.rs`

```rust
pub struct Backend<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    pub(crate) ids: IdGen,
    pub(crate) topology: T,
    pub(crate) schemas: HashMap<CrdtId, S>,
    pub(crate) pending: Vec<Op<T::OpKind>>,
    pub(crate) applied_seq: GlobalSeq,
    pub(crate) causal: CausalState,
}

impl<T, S> Backend<T, S> {
    pub fn add_entity(&mut self, structural_op: impl FnOnce(CrdtId) -> T::OpKind) {
        let id = self.ids.next();
        let op_kind = structural_op(id);
        self.pending.push(Op { id, seq: None, kind: op_kind });
        // Don't pre-apply (echo-wait pattern)
    }
    
    pub fn mutate_schema(&mut self, target: CrdtId, mutation: S::Mutation) {
        if let Some(schema) = self.schemas.get_mut(&target) {
            let mut ctx = CausalContext::new(self.ids.next(), None, &mut self.causal);
            let delta = schema.mutate(mutation, &mut ctx);
            let (path, wire) = delta.into_wire_op();
            // Queue SetProperty op (topology-agnostic)
        }
    }
    
    pub fn apply_remote(&mut self, op: &Op<T::OpKind>) -> Result<(), ApplyError> {
        // Check seq ordering
        let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
        
        // Dispatch to topology or schema
        match try_extract_property_op(&op.kind) {
            Some((target, path, delta)) => {
                let schema = self.schemas.entry(target).or_insert_with(S::default);
                schema.apply_wire(path, delta, &ctx)?;
            }
            None => {
                self.topology.apply_structural_op(&op.kind, &ctx);
                // If AddEntity op, insert default schema
                if let Some(new_id) = extract_add_entity_id(&op.kind) {
                    self.schemas.insert(new_id, S::default());
                }
            }
        }
        
        self.applied_seq = op.seq.unwrap();
        Ok(())
    }
}
```

### Phase 2: Define Topology Trait

**File**: `crates/kyoso_crdt/src/topology.rs`

```rust
pub trait Topology: Send + Sync + 'static {
    type OpKind: Clone + Debug + Serialize + DeserializeOwned;
    type SnapshotState: Clone + Serialize + DeserializeOwned;
    
    /// Apply a structural op (AddNode, Move, AddEdge, etc.)
    fn apply_structural_op(&mut self, op: &Self::OpKind, ctx: &CausalContext);
    
    /// Snapshot structural state (tree, edges, z-order, etc.)
    fn snapshot(&self) -> Self::SnapshotState;
    
    /// Restore from snapshot
    fn restore(&mut self, snap: Self::SnapshotState);
    
    /// Extract the target ID from property ops (if this op is SetProperty)
    fn extract_property_target(op: &Self::OpKind) -> Option<(CrdtId, Path, WireDelta)>;
    
    /// Extract newly-created entity ID from Add ops (if this op creates an entity)
    fn extract_new_entity_id(op: &Self::OpKind) -> Option<CrdtId>;
}
```

### Phase 3: Implement GraphTopology

**File**: `crates/kyoso_graph_crdt/src/topology.rs`

```rust
pub struct GraphTopology {
    nodes: HashMap<CrdtId, NodeStructure>,
    edges: HashMap<CrdtId, EdgeStructure>,
    pending_moves: HashMap<CrdtId, CrdtId>,
}

impl Topology for GraphTopology {
    type OpKind = GraphOpKind;
    type SnapshotState = GraphSnapshot;
    
    fn apply_structural_op(&mut self, op: &GraphOpKind, ctx: &CausalContext) {
        match op {
            GraphOpKind::AddNode => {
                self.nodes.insert(ctx.op_id, NodeStructure {
                    tombstoned: false,
                    order_key: None,
                    tree_parent: None,
                });
            }
            GraphOpKind::Move { target, new_parent, position } => {
                if let Some(node) = self.nodes.get_mut(target) {
                    // Cycle check
                    node.tree_parent = *new_parent;
                    node.order_key = Some(position.clone());
                }
            }
            // ... AddRefEdge, RemoveNode, etc.
        }
    }
    
    fn extract_property_target(op: &GraphOpKind) -> Option<(CrdtId, Path, WireDelta)> {
        match op {
            GraphOpKind::SetNodeProperty { target, path, delta } => {
                Some((*target, path.clone(), delta.clone()))
            }
            GraphOpKind::SetRefEdgeProperty { target, path, delta } => {
                Some((*target, path.clone(), delta.clone()))
            }
            _ => None,
        }
    }
    
    fn extract_new_entity_id(op: &GraphOpKind) -> Option<CrdtId> {
        match op {
            GraphOpKind::AddNode => Some(/* op.id from context */),
            GraphOpKind::AddRefEdge { .. } => Some(/* op.id */),
            _ => None,
        }
    }
}
```

### Phase 4: Type Aliases

**File**: `crates/kyoso_graph_crdt/src/backend.rs`

```rust
pub type GraphBackend<S> = Backend<GraphTopology, S>;

// Convenience for structure-only
pub type StructuralGraphBackend = GraphBackend<EmptySchema>;
```

---

## Updated "How to Create a CRDT Structure" Guide

**File**: `docs/kyoso/system-design/how-to-create-crdt-structures.md`

1. **Define your topology** (what structural operations exist?)
2. **Impl `Topology` trait** (how to apply/snapshot/restore structure)
3. **Define `OpKind` enum** (what ops can happen?)
4. **Type alias**: `type YourBackend<S> = Backend<YourTopology, S>`
5. **Impl `CrdtModel`** (delegate to generic `Backend<T, S>`)
6. **Add sync plugin** (Bevy systems for your structure)
7. **Add server handler** (implements `RoomModelHandler`)
8. **Register in app** (add to `ModelRegistry`)

**Done!** You have a new CRDT structure following the same pattern as graph/comments.

---

## Summary

**Before**: Graph CRDT is a monolith, hard to adapt  
**After**: Graph is **one instance** of `Backend<Topology, Schema>` pattern

**Reusable infrastructure**:
- ✅ `kyoso_crdt` primitives (LWW, OR-Set, PN-Counter, etc.)
- ✅ `Topology` trait (structure operations)
- ✅ `Backend<T, S>` (generic storage + ops)
- ✅ `CrdtModel` trait (wire integration)
- ✅ Transport + server (multi-model)

**Easy to extend**:
- Add canvas → impl `CanvasTopology`, done
- Add rich text → impl `DocumentTopology` wrapping Fugue, done
- Add spreadsheet → impl `GridTopology`, done

The graph CRDT becomes **clean infrastructure you can clone for new domains**.
