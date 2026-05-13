# Creating New CRDT Structures

This guide explains how to create new CRDT data structures using the reusable `Backend<T, S>` infrastructure introduced in the unified backend architecture.

## Overview

The kyoso CRDT framework separates concerns into two orthogonal dimensions:

1. **Topology (`T`)**: Domain-specific structural operations (add/remove entities, relationships, ordering)
2. **Schema (`S`)**: CRDT properties on entities (LWW fields, OR-Sets, PN-Counters)

The generic `Backend<T, S>` type in `kyoso_crdt` composes these dimensions into a complete CRDT model.

## Architecture Pattern

```rust
// Core abstractions (kyoso_crdt)
pub trait Topology { /* structural ops */ }
pub struct Backend<T: Topology, S: Crdt> { /* identity, op log, causal context */ }

// Domain implementation (your crate)
pub struct MyTopology { /* domain-specific structure */ }
impl Topology for MyTopology { /* implement trait */ }

// Usage
type MyBackend<S> = Backend<MyTopology, S>;

// Server mirrors (structure-only)
type ServerMirror = Backend<MyTopology, EmptySchema>;

// Clients (structure + typed properties)
#[derive(Crdt)]
struct MySchema {
    name: LwwRegister<String>,
    tags: OrSet<String>,
}
type ClientBackend = Backend<MyTopology, MySchema>;
```

## Step-by-Step Guide

### 1. Define Your Operation Enum

Create an enum representing all operations in your domain:

```rust
use serde::{Serialize, Deserialize};
use kyoso_crdt::{CrdtId, delta::{Path, WireDelta}};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CanvasOpKind {
    // Structural operations (topology)
    AddStroke,
    RemoveStroke { target: CrdtId },
    SetZOrder { target: CrdtId, z: u32 },
    
    // Property operations (schema)
    SetStrokeProperty {
        target: CrdtId,
        path: Path,
        delta: WireDelta,
    },
}
```

**Key points:**
- Structural ops create/modify/remove entities and their relationships
- Property ops carry `Path` + `WireDelta` for the schema layer to handle
- Add ops use the op's own `CrdtId` as the entity ID (convention)

### 2. Define Your Topology Structure

Create a struct holding your domain's structural state:

```rust
use std::collections::HashMap;
use kyoso_crdt::CrdtId;

#[derive(Clone, Debug, Default)]
pub struct CanvasTopology {
    strokes: HashMap<CrdtId, StrokeStructure>,
    z_order: Vec<CrdtId>,
}

#[derive(Clone, Debug)]
struct StrokeStructure {
    points: Vec<Point>,
    tombstoned: bool,
}
```

**Key points:**
- Store structural metadata only (positions, relationships, ordering)
- Do NOT store properties (name, color, tags) — those live in schemas
- Include `tombstoned` flags for removed entities

### 3. Define Snapshot Format

Create the snapshot representation:

```rust
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanvasSnapshot {
    pub strokes: Vec<StrokeSnap>,
    pub z_order: Vec<CrdtId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrokeSnap {
    pub id: CrdtId,
    pub points: Vec<Point>,
}
```

**Key points:**
- Exclude tombstoned entities
- Only structural data (properties are snapshotted separately)
- Must be serializable (Serialize + Deserialize)

### 4. Implement the Topology Trait

```rust
use kyoso_crdt::topology::{Topology, PropertyOp};
use kyoso_crdt::context::CausalContext;

impl Topology for CanvasTopology {
    type OpKind = CanvasOpKind;
    type SnapshotState = CanvasSnapshot;

    fn apply_structural_op(&mut self, op: &Self::OpKind, ctx: &CausalContext) {
        match op {
            CanvasOpKind::AddStroke => {
                self.strokes.insert(ctx.op_id, StrokeStructure {
                    points: Vec::new(),
                    tombstoned: false,
                });
            }
            CanvasOpKind::RemoveStroke { target } => {
                if let Some(stroke) = self.strokes.get_mut(target) {
                    stroke.tombstoned = true;
                }
            }
            CanvasOpKind::SetZOrder { target, z } => {
                // Update z-order (structural, not a property)
                // ...
            }
            // Property ops are filtered out, never reach here
            CanvasOpKind::SetStrokeProperty { .. } => {}
        }
    }

    fn snapshot(&self) -> Self::SnapshotState {
        let mut strokes: Vec<StrokeSnap> = self
            .strokes
            .iter()
            .filter(|(_, s)| !s.tombstoned)
            .map(|(id, s)| StrokeSnap {
                id: *id,
                points: s.points.clone(),
            })
            .collect();
        strokes.sort_by_key(|s| s.id);

        CanvasSnapshot {
            strokes,
            z_order: self.z_order.clone(),
        }
    }

    fn restore(&mut self, snap: Self::SnapshotState) {
        self.strokes.clear();
        for s in snap.strokes {
            self.strokes.insert(s.id, StrokeStructure {
                points: s.points,
                tombstoned: false,
            });
        }
        self.z_order = snap.z_order;
    }

    fn extract_property_op(op: &Self::OpKind) -> Option<PropertyOp> {
        match op {
            CanvasOpKind::SetStrokeProperty { target, path, delta } => {
                Some(PropertyOp {
                    target: *target,
                    path: path.clone(),
                    delta: delta.clone(),
                })
            }
            _ => None,
        }
    }

    fn extract_new_entity_id(op: &Self::OpKind, ctx: &CausalContext) -> Option<CrdtId> {
        match op {
            CanvasOpKind::AddStroke => Some(ctx.op_id),
            _ => None,
        }
    }

    fn op_kind_label(op: &Self::OpKind) -> &'static str {
        match op {
            CanvasOpKind::AddStroke => "AddStroke",
            CanvasOpKind::RemoveStroke { .. } => "RemoveStroke",
            CanvasOpKind::SetZOrder { .. } => "SetZOrder",
            CanvasOpKind::SetStrokeProperty { .. } => "SetStrokeProperty",
        }
    }
}
```

**Key points:**
- Use `ctx.op_id` to get the operation's unique ID
- Check `tombstoned` flags when reading state
- Property ops are routed to schema layer automatically
- Snapshots should be sorted by ID for deterministic comparison

### 5. Create a Domain-Specific Wrapper (Optional)

For ergonomics, wrap `Backend<MyTopology, S>` in a newtype:

```rust
use kyoso_crdt::{Backend, id::{CrdtId, PeerId, GlobalSeq, IdGen}};
use kyoso_crdt::lattice::Crdt;
use kyoso_crdt::schema::{SchemaApply, IntoWireOp};

pub struct CanvasBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    inner: Backend<CanvasTopology, S>,
}

impl<S> CanvasBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    pub fn with_peer(peer: PeerId) -> Self {
        Self {
            inner: Backend::with_peer(peer),
        }
    }

    /// Domain-specific method: add a stroke
    pub fn add_stroke(&mut self, points: Vec<Point>) -> CrdtId {
        let id = self.inner.queue_op(CanvasOpKind::AddStroke);
        // Optimistically update local state if needed
        if let Some(stroke) = self.inner.topology_mut().strokes.get_mut(&id) {
            stroke.points = points;
        }
        id
    }

    /// Domain-specific method: remove a stroke
    pub fn remove_stroke(&mut self, target: CrdtId) -> bool {
        if self.inner.topology().strokes.get(&target)
            .map_or(true, |s| s.tombstoned)
        {
            return false;
        }
        self.inner.queue_op(CanvasOpKind::RemoveStroke { target });
        true
    }

    /// Domain-specific method: mutate stroke properties
    pub fn mutate_stroke(&mut self, target: CrdtId, mutation: S::Mutation) -> Option<()> {
        let (op_id, delta) = self.inner.mutate_schema_with_context(target, mutation)?;
        let (path, wire_delta) = delta.into_wire_op();

        self.inner.pending_mut().push(kyoso_crdt::Op::new(
            op_id,
            CanvasOpKind::SetStrokeProperty {
                target,
                path,
                delta: wire_delta,
            },
        ));

        Some(())
    }

    // Delegate other methods to inner as needed
    pub fn backend(&self) -> &Backend<CanvasTopology, S> {
        &self.inner
    }
}
```

**Key points:**
- Provides domain-specific API surface
- Hides generic backend complexity
- Can add validation and business logic
- Use `mutate_schema_with_context` helper to avoid borrow-checker issues

### 6. Define Typed Schemas for Clients

```rust
use kyoso_crdt::lattice::{LwwRegister, OrSet, PnCounter};
use kyoso_crdt_derive::Crdt;

#[derive(Crdt, Default, Clone, Debug, PartialEq)]
pub struct StrokeSchema {
    pub color: LwwRegister<String>,
    pub width: LwwRegister<f32>,
    pub tags: OrSet<String>,
}

// Usage on client
type ClientCanvas = CanvasBackend<StrokeSchema>;

let mut canvas = ClientCanvas::with_peer(my_peer_id);
let stroke_id = canvas.add_stroke(vec![Point { x: 0.0, y: 0.0 }]);

canvas.mutate_stroke(stroke_id, StrokeSchemaMut::Color(
    LwwMut::Set("red".into())
));
```

### 7. Server Mirror (Structure-Only)

```rust
use kyoso_crdt::EmptySchema;

type ServerMirror = CanvasBackend<EmptySchema>;

let mut mirror = ServerMirror::with_peer(0); // server is peer 0
// Server only tracks structure, not properties
```

**Key points:**
- `EmptySchema` has zero fields — no property storage
- Server still applies structural ops correctly
- Snapshots only contain topology, not schemas
- Lighter memory footprint

## Example: Graph Domain

The graph CRDT is a reference implementation of this pattern:

- **Topology**: [`GraphTopology`](crates/kyoso_graph_crdt/src/topology.rs) — nodes in tree + reference edges
- **OpKind**: [`GraphOpKind`](crates/kyoso_graph_crdt/src/op.rs) — AddNode, Move, AddRefEdge, etc.
- **Wrapper**: [`GraphBackend<S>`](crates/kyoso_graph_crdt/src/graph_backend.rs) — domain methods
- **Server**: `GraphBackend<EmptySchema>`
- **Client**: `GraphBackend<FrameSchema>` with typed properties

See these files for a complete working example.

## Testing Your CRDT

### Unit Tests

```rust
#[test]
fn concurrent_ops_converge() {
    let mut peer_a = CanvasBackend::<EmptySchema>::with_peer(1);
    let mut peer_b = CanvasBackend::<EmptySchema>::with_peer(2);
    let mut log = InMemoryOpLog::new();

    // Concurrent adds
    let id_a = peer_a.add_stroke(vec![Point { x: 0.0, y: 0.0 }]);
    let id_b = peer_b.add_stroke(vec![Point { x: 1.0, y: 1.0 }]);

    // Flush through log (simulates server)
    flush(&mut peer_a, &mut log, &mut [&mut peer_b]);
    flush(&mut peer_b, &mut log, &mut [&mut peer_a]);

    // Build canonical replica
    let mut canonical = CanvasBackend::<EmptySchema>::with_peer(99);
    for op in log.slice(0, log.head()) {
        canonical.backend_mut().apply_remote(&op).unwrap();
    }

    // Verify convergence
    assert_eq!(peer_a.backend().snapshot(), canonical.backend().snapshot());
    assert_eq!(peer_b.backend().snapshot(), canonical.backend().snapshot());
}
```

### Property-Based Testing

Use `proptest` to generate random operation sequences and verify CRDT invariants.

## Common Patterns

### Cycle Detection

For tree-structured topologies, validate moves don't create cycles:

```rust
fn would_create_cycle(&self, target: CrdtId, proposed_parent: CrdtId) -> bool {
    if target == proposed_parent {
        return true;
    }
    let mut cursor = Some(proposed_parent);
    while let Some(id) = cursor {
        if id == target {
            return true;
        }
        cursor = self.nodes.get(&id).and_then(|n| n.parent);
    }
    false
}
```

### Cascade Tombstoning

When removing an entity with dependents:

```rust
CanvasOpKind::RemoveLayer { target } => {
    if let Some(layer) = self.layers.get_mut(target) {
        layer.tombstoned = true;
        // Cascade to child strokes
        for stroke in self.strokes.values_mut() {
            if stroke.layer_id == *target {
                stroke.tombstoned = true;
            }
        }
    }
}
```

### Pending Op Tracking

Track in-flight ops to suppress duplicate emissions:

```rust
pub fn is_pending_move(&self, target: CrdtId) -> bool {
    self.pending_moves.values().any(|t| *t == target)
}

// In move_node:
let op_id = self.inner.queue_op(CanvasOpKind::Move { ... });
self.inner.topology_mut().track_pending_move(op_id, target);
```

## Server Integration

### Registering Your Model

```rust
// In your app
use kyoso_server::{AppState, OpStore};
use my_canvas_crdt::{CanvasBackend, CanvasOpKind};

let store = OpStore::postgres(db_url).await?;
let state = AppState::from_store(store.clone())
    .with_factory(CanvasHandlerFactory::new(store));
```

### Handler Implementation

```rust
pub struct CanvasHandlerFactory { store: OpStore }

impl HandlerFactory for CanvasHandlerFactory {
    fn model_id(&self) -> ModelId {
        ModelId::new("canvas")
    }

    async fn create(&self, room_id: RoomId) -> Result<Box<dyn RoomModelHandler>> {
        Ok(Box::new(CanvasRoomHandler::restore(room_id, self.store.clone()).await?))
    }
}

pub struct CanvasRoomHandler {
    mirror: Mutex<CanvasBackend<EmptySchema>>,
    // ... store, append_lock
}
```

## Best Practices

1. **Separate structure from properties**: Topology handles relationships, schemas handle data
2. **Use ctx.op_id for entity IDs**: Avoids extra fields, keeps ops small
3. **Implement idempotent apply**: Applying same op twice should be safe
4. **Sort snapshots by ID**: Enables deterministic comparison
5. **Test convergence**: Write tests that verify replicas converge under concurrent ops
6. **Document invariants**: Explain constraints (e.g., "tree must be acyclic")

## Migration from Old Code

If you have an existing CRDT backend, migrate incrementally:

1. Extract structural logic into `Topology` impl
2. Create wrapper type with domain methods
3. Update server to use new type with `EmptySchema`
4. Update tests to use new snapshot format
5. Migrate clients to typed schemas
6. Remove old backend code

## Further Reading

- [CRDT Overview](./crdt-overview.md) — High-level system design
- [Backend vs Document](./backend-vs-document.md) — Historical context
- [Architecture Evolution](./architecture-evolution.md) — Phase markers explained
