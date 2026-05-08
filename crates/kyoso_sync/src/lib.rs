//! Bevy ↔ kyoso_server bridge.
//!
//! Add [`CrdtSyncPlugin`] to a Bevy `App`, configured with the server URL
//! and a room id, and the app gains a `Graph<N, E, CrdtBackend<N, E>>`
//! resource that stays in sync with every other peer joined to the same
//! room.
//!
//! # Example
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_sync::CrdtSyncPlugin;
//!
//! #[derive(Component, Default, Debug, Clone)]
//! struct SceneNode;
//! #[derive(Component, Default, Debug, Clone, Copy)]
//! struct SceneEdge;
//!
//! App::new()
//!     .add_plugins(CrdtSyncPlugin::<SceneNode, SceneEdge>::new(
//!         "ws://localhost:7878/ws",
//!         "demo-room",
//!     ))
//!     .run();
//! ```

pub mod builtin_schemas;
pub mod category;
pub mod client;
pub mod engine;
pub mod index;
pub mod plugin;
pub mod schema_sync;
pub mod sequence_diff;

pub use builtin_schemas::TransformSchema;
pub use category::{
    EdgeCategoryMarker, EdgeCategoryProjectors, SyncedEdgeCategoryPlugin,
};
pub use client::{ConnectError, Inbound, WsClient};
pub use engine::ClientSyncEngine;
pub use index::EntityCrdtIndex;
pub use plugin::{
    ClearLocalPresence, CrdtSyncPlugin, RawPresence, RawPresenceEvent, RemoteOpApplied,
    SetLocalPresence, SyncStatus, Syncable,
};
pub use schema_sync::{
    SchemaDoc, SchemaField, SchemaSync, SchemaSyncedEdgeComponentPlugin,
    SchemaSyncedNodeComponentPlugin,
};
pub use sequence_diff::sequence_diff;

// Re-export the derive macro alongside the trait. Rust's namespace rules
// allow a trait and a derive macro to share a name; consumers can write
// `use kyoso_sync::SchemaSync;` to bring both into scope.
pub use kyoso_sync_derive::SchemaSync;
