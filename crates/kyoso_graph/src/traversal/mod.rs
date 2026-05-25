//! Graph traversal — agent-facing read-only walks over the live Bevy
//! world, with granular search parameters.
//!
//! Three "views" depending on what you want to walk:
//!
//! - [`WorldTreeView<F>`] — hierarchy ([`crate::tree::TreeQuery`]) +
//!   `&World`. Walks `Children`/`ChildOf`/`OrderKey`.
//! - [`WorldGraphView<N, E, NF, EF>`] — entity-edge graph
//!   ([`crate::queries::GraphQuery`]) + `&World`. Walks
//!   `EdgeFrom`/`EdgeTo`/`OutgoingEdges`/`IncomingEdges`.
//! - [`WorldSceneView<N, E, NF, EF>`] — combined
//!   ([`crate::scene::Scene`]) + `&World`. Explicit
//!   `traverse_tree` / `traverse_graph` methods.
//!
//! All three carry `&World` so [`TraversalQuery`] can evaluate
//! runtime-typed component filters (`require::<T>` / `exclude::<T>`)
//! and resolve [`NodeRef::Replicated`] through any [`NodeIdResolver`].
//!
//! ## NodeIdResolver — dependency inversion for id resolution
//!
//! `kyoso_graph` doesn't depend on any sync/CRDT layer; the
//! [`NodeIdResolver`] trait abstracts the `Entity → Id` lookup so
//! downstream crates (e.g. `kyoso_graph_sync::EntityCrdtIndex →
//! CrdtId`) provide their own impl.

pub mod query;
pub mod resolver;
pub mod runner;
pub mod view;

pub use query::{Order, TraversalQuery, WorldEntityRef};
pub use resolver::{resolve_node_ref, NodeIdResolver, NodeRef};
pub use view::{WorldGraphView, WorldSceneView, WorldTreeView};

// Re-export `Step` and `TraversalNode` from the underlying walk
// abstraction so callers using `step_with` don't need a separate
// import.
pub use crate::traverse::{Step, TraversalNode};
