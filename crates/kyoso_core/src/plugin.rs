//! `KyosoCorePlugin`: single-call entry point that wires every
//! per-component schema plugin for the core scene node types.
//!
//! Add it once at app startup with the WS server URL and room id; the
//! plugin handles both the structural sync (`AddNode`/`Move`/etc. via
//! `GraphSyncPlugin<SceneNode, SceneEdge>` on top of the multi-model
//! `SyncTransportPlugin`) and the typed-schema plugins for each Bevy
//! component (`Frame`, `Rectangle`, `Text`, `TypeStyle`, `Size`,
//! `Transform`).
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_core::KyosoCorePlugin;
//!
//! App::new()
//!     .add_plugins(MinimalPlugins)
//!     .add_plugins(KyosoCorePlugin {
//!         server_url: "ws://localhost:7878/ws".into(),
//!         room: "demo".into(),
//!     })
//!     .run();
//! ```
//!
//! ## TypeStyle is registered as a node component
//!
//! `TypeStyle` is the inner value of `Text.style` (`#[crdt(nested)]`)
//! but the typed-schema plugin layer also registers it as a top-level
//! component plugin so its wire dispatch is wired up. In practice
//! `TypeStyle` is never spawned as a standalone entity; the `Component`
//! derive is harmless when never spawned alone, and the plugin
//! registration is required for the inbound `RemoteOpApplied` handler
//! to find the right `Document<TypeStyleSchema>` instance.

use bevy::prelude::*;
use kyoso_graph_sync::{GraphSyncPlugin, NodeTarget, SchemaSyncedComponentPlugin};
use kyoso_sync::SyncTransportPlugin;

use crate::{Frame, Rectangle, Size, Text, TypeStyle};

pub struct KyosoCorePlugin {
    pub server_url: String,
    pub room: String,
}

impl Plugin for KyosoCorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            // Multi-model transport — owns the WsClient. Add this even
            // for graph-only apps; it's the foundation that lets future
            // model plugins (comments, presence) mount onto the same
            // socket.
            SyncTransportPlugin::new(self.server_url.clone(), self.room.clone()),
            GraphSyncPlugin::default(),
            // Field-bearing components.
            SchemaSyncedComponentPlugin::<NodeTarget, Frame>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Rectangle>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Text>::default(),
            // Satellite components shared across node kinds.
            SchemaSyncedComponentPlugin::<NodeTarget, Size>::default(),
            SchemaSyncedComponentPlugin::<NodeTarget, Transform>::default(),
            // Nested-only — see module docstring.
            SchemaSyncedComponentPlugin::<NodeTarget, TypeStyle>::default(),
        ));
    }
}
