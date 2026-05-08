//! Graph CRDT data model.
//!
//! Implements [`kyoso_crdt::CrdtModel`] for a node + reference-edge + tree
//! topology. This crate previously lived inside `kyoso_crdt`; it was split
//! out so `kyoso_crdt` can host other domain models (comments, presence,
//! text) alongside the graph on the same wire protocol and shared id space.
//!
//! See [`CrdtBackend`] for the storage type, [`OpKind`] for the op enum,
//! and [`Document`] for the schema-aware typed wrapper.

pub mod backend;
pub mod document;
pub mod edge_category;
pub mod op;
pub mod snapshot;

pub use backend::CrdtBackend;
pub use document::Document;
pub use edge_category::{DanglePolicy, EdgeCategory, RefEdgeCrdt, RefEdgePolicy};
pub use op::OpKind;
pub use snapshot::{EdgeSnap, NodeSnap, Snapshot};
