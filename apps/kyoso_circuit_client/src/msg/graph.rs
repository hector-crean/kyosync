//! Graph-layer external command/event surface.
//!
//! `kyoso_graph::GraphCommand` uses Bevy `Entity` handles — useful
//! inside the process, meaningless outside it. These `*Ext` siblings
//! express the same intent in [`ExternalId`] (`= kyoso_crdt::CrdtId`)
//! so they survive the duplex bridge.

use bevy::prelude::*;
use kyoso_circuit::{CircuitEdgeKind, CircuitLayer, ComponentKind};
use serde::{Deserialize, Serialize};

use super::command::{ExternalId, Pos3};

/// External-facing graph topology commands. CrdtId-based mirror of the
/// curated subset of [`kyoso_graph::GraphCommand`] that's meaningful
/// across the duplex bridge.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum GraphCommandExt {
    /// Spawn a new circuit-component node at world `position` on the
    /// given board layer. The kind determines which component schema
    /// (Resistor / Capacitor / …) is attached. `layer` is replicated
    /// via the [`OnLayer`](kyoso_circuit::OnLayer) schema-synced
    /// component; `position.y` gets snapped to the layer's y-offset by
    /// the scene layer.
    SpawnComponent {
        position: Pos3,
        kind: ComponentKind,
        layer: CircuitLayer,
    },

    /// Add a typed edge `from → to` of the given kind. Spawns an edge
    /// entity carrying `(EdgeFrom, EdgeTo, CircuitEdge)` plus the
    /// per-kind marker chosen by `kind`.
    Connect {
        from: ExternalId,
        to: ExternalId,
        kind: CircuitEdgeKind,
    },

    /// Remove the directed edge `from → to`, if one exists. Matches on
    /// endpoint pair regardless of kind.
    Disconnect { from: ExternalId, to: ExternalId },

    /// Despawn the node and all incident edges.
    RemoveNode { id: ExternalId },

    /// Despawn a specific edge entity.
    RemoveEdge { id: ExternalId },
}

/// External-facing graph observations. Both local mutations and
/// remote-applied CRDT ops produce the same event shapes.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum GraphMessageExt {
    NodeAppeared {
        id: ExternalId,
        position: Pos3,
    },
    NodeMoved {
        id: ExternalId,
        position: Pos3,
    },
    NodeRemoved {
        id: ExternalId,
    },
    EdgeAppeared {
        id: ExternalId,
        from: ExternalId,
        to: ExternalId,
    },
    EdgeRemoved {
        id: ExternalId,
        from: ExternalId,
        to: ExternalId,
    },
}
