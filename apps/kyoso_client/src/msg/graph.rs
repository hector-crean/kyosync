//! Graph-layer external command/event surface.
//!
//! `kyoso_graph::GraphCommand` and `kyoso_graph::GraphMessage` use Bevy
//! `Entity` handles — useful inside the process, meaningless outside it.
//! These `*Ext` siblings express the same intent in [`ExternalId`]
//! (`= kyoso_crdt::CrdtId`) so they survive the duplex bridge.
//!
//! The umbrella dispatcher in [`crate::handlers`] is responsible for
//! the `ExternalId → Entity` translation when forwarding to internal
//! consumers, and for the inverse when emitting events.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::command::{ExternalId, Pos2};

/// External-facing graph topology commands. CrdtId-based mirror of the
/// curated subset of [`kyoso_graph::GraphCommand`] that's meaningful
/// across the duplex bridge.
///
/// Tree commands (`InsertChild`/`Reparent`/`MoveSibling`) are not yet
/// mirrored — their semantics need a tree-shaped use case in the client
/// before the external API solidifies.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum GraphCommandExt {
    /// Add a typed cross-frame edge `from → to`. Spawns an edge entity
    /// carrying `EdgeFrom(from)` / `EdgeTo(to)` / `FigmaEdge` plus the
    /// per-kind marker component (`ReferenceMarker`, `DependencyMarker`,
    /// etc.) chosen by `kind`.
    Connect {
        from: ExternalId,
        to: ExternalId,
        kind: crate::weave::WeaveEdgeKind,
    },

    /// Remove the directed edge `from → to`, if it exists. Matches on
    /// endpoint pair regardless of kind.
    Disconnect { from: ExternalId, to: ExternalId },

    /// Despawn the node and all incident edges.
    RemoveNode { id: ExternalId },

    /// Despawn a specific edge entity.
    RemoveEdge { id: ExternalId },
}

/// External-facing graph observations. CrdtId-based mirror of the
/// curated subset of [`kyoso_graph::GraphMessage`] that's meaningful
/// across the duplex bridge.
///
/// These are **semantic** projections of internal state changes — both
/// local mutations and remote-applied CRDT ops produce the same event
/// shapes. External consumers should not need to know which side caused
/// which change.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum GraphMessageExt {
    NodeAppeared { id: ExternalId, position: Pos2 },
    NodeMoved { id: ExternalId, position: Pos2 },
    NodeRemoved { id: ExternalId },
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
