//! Topology abstraction for CRDT backends.
//!
//! A [`Topology`] defines the *structural operations* of a CRDT model
//! (add/remove entities, move in hierarchy, create relationships) as
//! distinct from *property operations* (set name, add tag, increment
//! counter) which are handled uniformly by the schema layer.
//!
//! Each domain (graph, canvas, document, whiteboard) implements this
//! trait to define its structural semantics. The generic [`Backend<T, S>`]
//! in [`crate::backend`] composes topology + schema into a complete
//! CRDT model.
//!
//! # Example
//!
//! ```ignore
//! // Graph topology (nodes in tree + reference edges)
//! pub struct GraphTopology {
//!     nodes: HashMap<CrdtId, NodeStructure>,
//!     edges: HashMap<CrdtId, EdgeStructure>,
//! }
//!
//! impl Topology for GraphTopology {
//!     type OpKind = GraphOpKind;
//!     type SnapshotState = GraphTopologySnapshot;
//!
//!     fn apply_structural_op(&mut self, op: &GraphOpKind, ctx: &CausalContext) {
//!         match op {
//!             GraphOpKind::AddNode => { /* insert node */ },
//!             GraphOpKind::Move { .. } => { /* update tree parent */ },
//!             // ...
//!         }
//!     }
//!
//!     // ...
//! }
//! ```

use std::fmt::Debug;

use serde::{Serialize, de::DeserializeOwned};

use crate::context::CausalContext;
use crate::delta::{Path, WireDelta};
use crate::id::CrdtId;

/// Structural operations for a CRDT domain.
///
/// Separates domain-specific structure (tree hierarchy, edge topology,
/// z-order, grid layout) from domain-agnostic properties (LWW, OR-Set,
/// PN-Counter fields on entities).
///
/// The trait has two responsibilities:
/// 1. Apply structural ops (AddNode, Move, AddEdge, etc.)
/// 2. Snapshot/restore structural state (tree shape, edge list, etc.)
///
/// Property ops (SetNodeProperty, etc.) are handled uniformly by the
/// generic backend and schema layer — topologies don't see them.
pub trait Topology: Send + Sync + 'static + Default {
    /// Domain-specific operation kind enum.
    ///
    /// Must include both structural ops (AddNode, Move, AddEdge) and
    /// property ops (SetNodeProperty). The trait's `apply_structural_op`
    /// only sees structural variants; property variants are extracted
    /// via `extract_property_op` and routed to the schema layer.
    type OpKind: Clone + Debug + Serialize + DeserializeOwned + Send + Sync;

    /// Snapshot format for structural state.
    ///
    /// Excludes properties (those live in per-entity schemas) and
    /// excludes tombstones (compaction removes them). A snapshot is
    /// the minimal state needed to reconstruct the topology.
    type SnapshotState: Clone + Debug + Serialize + DeserializeOwned + Send + Sync;

    /// Apply a structural operation.
    ///
    /// Called for ops like AddNode, Move, AddRefEdge, RemoveNode.
    /// Property ops (SetNodeProperty) are routed to the schema layer
    /// instead and never reach this method.
    ///
    /// The implementation should:
    /// - Validate the op is well-formed (e.g., Move doesn't create cycle)
    /// - Update internal topology state (nodes map, edges map, etc.)
    /// - Use `ctx.op_id` as the new entity ID for Add ops
    /// - Be idempotent (applying the same op twice is a no-op or updates to same state)
    fn apply_structural_op(&mut self, op: &Self::OpKind, ctx: &CausalContext);

    /// Snapshot the current structural state.
    ///
    /// Returns only live (non-tombstoned) entities and their structural
    /// metadata (tree parent, order key, edge endpoints, z-index, etc.).
    /// Properties are snapshotted separately by the schema layer.
    fn snapshot(&self) -> Self::SnapshotState;

    /// Restore from a snapshot.
    ///
    /// Clears current state and replaces with the snapshot. The backend
    /// handles restoring property schemas; this method only restores
    /// structural topology.
    fn restore(&mut self, snap: Self::SnapshotState);

    /// Extract property-mutation target from an op.
    ///
    /// If `op` is a property-setting variant (SetNodeProperty,
    /// SetEdgeProperty, etc.), return `Some((target_id, path, delta))`.
    /// Otherwise return `None` (structural op, will be passed to
    /// `apply_structural_op`).
    ///
    /// This is how the backend routes ops: property ops go to schema
    /// layer, structural ops go to topology.
    fn extract_property_op(op: &Self::OpKind) -> Option<PropertyOp>;

    /// Extract newly-created entity ID from an Add op.
    ///
    /// If `op` creates a new entity (AddNode, AddEdge, AddStroke, etc.),
    /// return `Some(new_entity_id)`. The backend uses this to insert a
    /// default schema for the new entity.
    ///
    /// For ops that don't create entities (Move, Remove, SetProperty),
    /// return `None`.
    ///
    /// # Note
    ///
    /// The ID should be the *op's own CrdtId* (from `ctx.op_id` when
    /// applying) for Add ops. The convention is that Add ops reuse their
    /// op ID as the entity ID — no separate "target" field needed.
    fn extract_new_entity_id(op: &Self::OpKind, ctx: &CausalContext) -> Option<CrdtId>;

    /// Optional: label for telemetry/logging.
    ///
    /// Returns a short human-readable label for this op kind, e.g.
    /// "AddNode", "Move", "SetNodeProperty". Used by `CrdtModel::op_kind_label`.
    fn op_kind_label(op: &Self::OpKind) -> &'static str;
}

/// Extracted property operation fields.
///
/// When a topology's `OpKind` includes property-setting variants
/// (SetNodeProperty, SetEdgeProperty, etc.), the topology's
/// `extract_property_op` method should parse them into this uniform
/// shape so the backend can route to the schema layer.
#[derive(Clone, Debug)]
pub struct PropertyOp {
    /// Entity being mutated (node ID, edge ID, etc.)
    pub target: CrdtId,
    /// Path to the property (e.g., `["Frame", "name"]`)
    pub path: Path,
    /// CRDT delta (LwwReplace, OrSetAdd, etc.)
    pub delta: WireDelta,
}
