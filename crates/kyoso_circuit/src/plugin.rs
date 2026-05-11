//! `KyosoCircuitPlugin`: single-call entry that wires every per-component
//! schema plugin for the circuit node types and the structural transport.
//!
//! Add it once at app startup with the WS server URL and room id; the
//! plugin handles both the structural sync (`AddNode` / `Move` / etc.
//! via `GraphSyncPlugin<CircuitNode, CircuitEdge>` on top of the
//! multi-model `SyncTransportPlugin`) and the typed-schema plugins for
//! each component type (`Resistor`, `Capacitor`, `Inductor`,
//! `VoltageSource`, `Ground`) plus `Transform`.
//!
//! Per-edge-category plugins (`SyncedEdgeCategoryPlugin`) are added by
//! the consuming app, mirroring how `kyoso_client::AppPlugin` does it
//! outside `KyosoFigmaPlugin`. That keeps the domain crate independent
//! of which subset of edge kinds an app cares about.

use bevy::prelude::*;
use kyoso_graph_sync::{GraphSyncPlugin, SchemaSyncedNodeComponentPlugin};
use kyoso_sync::SyncTransportPlugin;

use crate::components::{Capacitor, Ground, Inductor, Resistor, VoltageSource};
use crate::layer::OnLayer;
use crate::{CircuitEdge, CircuitNode};

pub struct KyosoCircuitPlugin {
    pub server_url: String,
    pub room: String,
}

impl Plugin for KyosoCircuitPlugin {
    fn build(&self, app: &mut App) {
        // Bevy's `add_plugins` tuple impl tops out at seven elements;
        // split into multiple calls to fit the full schema set
        // (transport + graph sync + 5 components + Transform + OnLayer).
        app.add_plugins((
            SyncTransportPlugin::new(self.server_url.clone(), self.room.clone()),
            GraphSyncPlugin::<CircuitNode, CircuitEdge>::default(),
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, Resistor>::default(),
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, Capacitor>::default(),
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, Inductor>::default(),
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, VoltageSource>::default(),
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, Ground>::default(),
        ));
        app.add_plugins((
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, Transform>::default(),
            SchemaSyncedNodeComponentPlugin::<CircuitNode, CircuitEdge, OnLayer>::default(),
        ));
    }
}
