//! Graph-specific Bevy ↔ CRDT bridge.
//!
//! Sits on top of [`kyoso_sync::SyncTransportPlugin`] and registers
//! the graph model with the multi-model transport. The plugin itself
//! is generics-free: graph **structure** (which nodes / edges exist,
//! which endpoints each edge connects) is expressed as two normal
//! [`SchemaSync`](kyoso_sync::SchemaSync) components —
//! [`NodePresence`] and [`EdgeEndpoints`] — that ride the standard
//! [`SchemaSyncedComponentPlugin`](kyoso_sync::SchemaSyncedComponentPlugin)
//! pipeline. Per-node / per-edge custom components ([`bevy::prelude::Transform`],
//! the consumer's own [`derive(SchemaSync)`](kyoso_sync::SchemaSync)
//! components) layer on the same way.
//!
//! Cycle / dangle / cascade enforcement is the consumer's
//! responsibility at the ECS command layer — see
//! `kyoso_graph::queries::GraphQuery::would_create_cycle`.
//!
//! ## Wiring
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_sync::SyncTransportPlugin;
//! use kyoso_graph_sync::{
//!     GraphSyncPlugin, NodeTarget, NodePresence, EdgeEndpoints,
//!     SchemaSyncedComponentPlugin,
//! };
//!
//! #[derive(Component, Default, Debug, Clone)]
//! #[require(NodePresence)]
//! struct SceneNode;
//!
//! #[derive(Component, Default, Debug, Clone)]
//! #[require(EdgeEndpoints)]
//! struct SceneEdge;
//!
//! App::new()
//!     .add_plugins(SyncTransportPlugin::new("ws://...", "demo"))
//!     .add_plugins(GraphSyncPlugin)
//!     .add_plugins(SchemaSyncedComponentPlugin::<NodeTarget, Transform>::default())
//!     .run();
//! ```

pub mod engine;
pub mod index;
pub mod plugin;
pub mod schema_sync;
pub mod structural;

pub use engine::ClientSyncEngine;
pub use index::EntityCrdtIndex;
pub use plugin::{GraphOp, GraphSyncPlugin, RemoteOpApplied};
pub use schema_sync::{EdgeTarget, NodeTarget};
pub use structural::{EdgeEndpoints, EdgePending, NodePresence};

// Re-export the model-agnostic typed-schema layer + the
// component-sync pipeline so consumers only need
// `kyoso_graph_sync::*` for graph apps. Everything graph-specific
// lives here; everything model-agnostic lives in `kyoso_sync` and
// is re-exported.
pub use kyoso_sync::{
    SchemaDoc, SchemaField, SchemaMutations, SchemaSync, SchemaSyncedComponentPlugin,
    SchemaTarget, SyncSet, TargetKind, TransformSchema,
};
