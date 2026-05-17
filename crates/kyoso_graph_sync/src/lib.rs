//! Graph-specific Bevy ↔ CRDT bridge.
//!
//! Sits on top of [`kyoso_sync::SyncTransportPlugin`] and registers the
//! graph model with the multi-model transport. Owns the graph
//! [`ClientSyncEngine`], the structural detection systems, the inbound
//! projector, the per-category edge dispatch ([`SyncedEdgeCategoryPlugin`]),
//! and the typed schema sync framework
//! ([`SchemaSyncedComponentPlugin`] etc.).
//!
//! Add [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) first,
//! then [`GraphSyncPlugin`]:
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_sync::SyncTransportPlugin;
//! use kyoso_graph_sync::GraphSyncPlugin;
//!
//! #[derive(Component, Default, Debug, Clone)]
//! struct SceneNode;
//! #[derive(Component, Default, Debug, Clone, Copy)]
//! struct SceneEdge;
//!
//! App::new()
//!     .add_plugins(SyncTransportPlugin::new("ws://localhost:7878/ws", "demo"))
//!     .add_plugins(GraphSyncPlugin::<SceneNode, SceneEdge>::default())
//!     .run();
//! ```

pub mod category;
pub mod engine;
pub mod index;
pub mod plugin;
pub mod schema_sync;

pub use category::{EdgeCategoryMarker, EdgeCategoryProjectors, SyncedEdgeCategoryPlugin};
pub use engine::ClientSyncEngine;
pub use index::EntityCrdtIndex;
pub use plugin::{GraphSyncPlugin, RemoteOpApplied, Syncable};
// Graph providers for the `kyoso_sync` component-sync pipeline.
pub use schema_sync::{EdgeTarget, NodeTarget};

// The model-agnostic typed-schema trait layer (trait + derive macro),
// the built-in `SchemaSync` impls, and the whole component-sync pipeline
// (`SchemaSyncedComponentPlugin`, `SchemaDoc`, `SchemaTarget`, `SyncSet`)
// moved to `kyoso_sync`. Re-exported here so existing
// `kyoso_graph_sync::*` imports keep working.
pub use kyoso_sync::{
    SchemaDoc, SchemaField, SchemaMutations, SchemaSync, SchemaSyncedComponentPlugin,
    SchemaTarget, SyncSet, TargetKind, TransformSchema,
};
