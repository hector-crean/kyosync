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
pub mod op;
pub mod snapshot;
pub mod topology;

pub use edge_category::{DanglePolicy, EdgeCategory, RefEdgeCrdt, RefEdgePolicy};
pub use graph_backend::GraphBackend;
pub use op::OpKind;
pub use snapshot::{EdgeSnap, NodeSnap, Snapshot};
pub use topology::GraphTopology;

/// String-slug identifying the graph model on the multi-model wire
/// envelope. Stable; clients and servers must agree on the slug.
pub const GRAPH_MODEL_ID: &str = "graph";

/// Convenience constructor for the graph model id.
#[must_use]
pub fn graph_model() -> kyoso_crdt::ModelId {
    kyoso_crdt::ModelId::new(GRAPH_MODEL_ID)
}
