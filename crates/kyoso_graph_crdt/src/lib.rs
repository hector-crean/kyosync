//! Graph CRDT data model.
//!
//! Implements [`kyoso_crdt::CrdtModel`] for a node + reference-edge + tree
//! topology. This crate previously lived inside `kyoso_crdt`; it was split
//! out so `kyoso_crdt` can host other domain models (comments, presence,
//! text) alongside the graph on the same wire protocol and shared id space.
//!
//! See [`GraphBackend`] for the storage type and [`OpKind`] for the op enum.

pub mod edge_category;
pub mod graph_backend;
pub mod invariants;
pub mod op;
pub mod topology;
pub mod view;

pub use edge_category::{DanglePolicy, EdgeCategory, RefEdgeCrdt, RefEdgePolicy};
pub use graph_backend::GraphBackend;
pub use invariants::{
    check_topology, cross_check_cycle_detection, InvariantViolation, ViolationKind,
};
pub use op::OpKind;
pub use topology::{EdgeSnap, GraphSnapshot, GraphTopology, NodeSnap};
pub use view::{
    ancestors, connected_component_undirected, descendants, would_create_cycle, GraphView,
};

/// String-slug identifying the graph model on the multi-model wire
/// envelope. Stable; clients and servers must agree on the slug.
pub const GRAPH_MODEL_ID: &str = "graph";

/// Convenience constructor for the graph model id.
#[must_use]
pub fn graph_model() -> kyoso_crdt::ModelId {
    kyoso_crdt::ModelId::new(GRAPH_MODEL_ID)
}
