//! `AppCommand` — the external API surface for the circuit client.
//!
//! Every variant is safe to send from outside the Bevy world (FFI, MCP,
//! an agent framework, the kyoso_server's broadcast channel, a CLI). All
//! commands flow through [`super::duplex::DuplexPlugin`] into a single
//! Bevy `Message<AppCommand>` stream that
//! [`crate::handlers::dispatch_app_commands`] fans out into the
//! contributing plugin's internal stream.
//!
//! Mirrors `kyoso_client::msg::AppCommand`, with `WeaveEdgeKind`
//! replaced by [`kyoso_circuit::CircuitEdgeKind`] and a new
//! `SetComponentKind` variant for the place tool's palette.

use bevy::prelude::*;
use kyoso_circuit::{CircuitEdgeKind, CircuitLayer, ComponentKind};
use serde::{Deserialize, Serialize};

use super::graph::GraphCommandExt;
use super::sync::SyncCommand;
use crate::tool::{Tool, ToolCommand};

/// Document-space 3D coordinates. The circuit canvas is 3D — components
/// live on layered planes stacked along the world Y axis. Use [`Pos3`]
/// in commands and events; [`Vec2`] is reserved for the cursor's
/// viewport coordinate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pos3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl From<Vec3> for Pos3 {
    fn from(v: Vec3) -> Self {
        Self {
            x: v.x,
            y: v.y,
            z: v.z,
        }
    }
}

impl From<Pos3> for Vec3 {
    fn from(p: Pos3) -> Self {
        Self::new(p.x, p.y, p.z)
    }
}

/// RGB triple, sRGB-space, 0..=1 floats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

/// Logical entity identifier as observed from outside Bevy. Maps to a
/// CRDT `CrdtId`, which the client resolves to a local `Entity` via
/// the sync layer's index.
pub type ExternalId = kyoso_crdt::CrdtId;

/// Top-level command bus. Every external producer (FFI, MCP, agent,
/// internal UI hotkey) writes one of these.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum AppCommand {
    /// Switch the active interaction mode. App-wide.
    SetTool(Tool),

    /// Set which kind of edge the connect tool will spawn on its next
    /// click-pair completion. Independent of `SetTool` so callers can
    /// issue them in either order.
    SetWireKind(CircuitEdgeKind),

    /// Set which component the place tool will spawn on its next
    /// canvas click. Independent of `SetTool`.
    SetComponentKind(ComponentKind),

    /// Set which board layer the place tool will assign to newly
    /// spawned components. Replicated as the [`OnLayer`](kyoso_circuit::OnLayer)
    /// schema-synced component on each spawn.
    SetActiveLayer(CircuitLayer),

    /// Per-tool sub-command. Forwarded to the right tool's internal
    /// stream by the dispatcher.
    Tool(ToolCommand),

    /// Topology mutation against the replicated graph.
    Graph(GraphCommandExt),

    /// Sync-layer control: reconnect, snapshot recovery.
    Sync(SyncCommand),
}
