//! `AppCommand` — the **external API surface** for the client.
//!
//! Every variant is safe to send from outside the Bevy world (JS,
//! MCP, an agent framework, the kyoso_server's broadcast channel, a
//! CLI). All commands flow through the
//! [`DuplexPlugin`](super::duplex::DuplexPlugin) into a single Bevy
//! `Message<AppCommand>` stream that
//! [`crate::handlers::dispatch_app_commands`] fans out into the
//! contributing plugin's internal command stream.
//!
//! ## Umbrella shape
//!
//! Each in-process plugin contributes one variant carrying its leaf
//! command type. The umbrella's only job is fan-out:
//!
//! - `SetTool(Tool)` — app-wide state change. Valid in any tool / mode.
//! - `Tool(ToolCommand)` — sub-tagged into `Select(...)` / `Create(...)`,
//!   forwarded to the corresponding tool plugin's internal stream.
//!   Gated by `run_if(in_state(Tool::X))` on the consumer side; sending
//!   a tool subcommand without the tool active is a no-op.
//! - `Graph(GraphCommandExt)` — topology mutations
//!   ([`super::graph::GraphCommandExt`]). The dispatcher resolves
//!   `ExternalId → Entity` via the sync layer's `GraphEntityIndex`
//!   before applying.
//! - `Sync(SyncCommand)` — sync-layer control
//!   ([`super::sync::SyncCommand`]): reconnect, snapshot recovery.
//!
//! Why one variant per plugin, not one giant enum: each plugin's
//! types live with its plugin so adding a plugin doesn't touch a
//! central handler. The umbrella just aggregates.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::graph::GraphCommandExt;
use super::sync::SyncCommand;
use crate::tool::{Tool, ToolCommand};

/// Document-space 2D coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pos2 {
    pub x: f32,
    pub y: f32,
}

impl From<Vec2> for Pos2 {
    fn from(v: Vec2) -> Self {
        Self { x: v.x, y: v.y }
    }
}

impl From<Pos2> for Vec2 {
    fn from(p: Pos2) -> Self {
        Self::new(p.x, p.y)
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

/// Top-level command bus. Every external producer (JS, MCP, agent,
/// internal UI hotkey) writes one of these.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum AppCommand {
    /// Switch the active interaction mode. App-wide.
    SetTool(Tool),

    /// Set which kind of weave edge the Connect tool will spawn on its
    /// next click-pair completion. Independent of `SetTool` so callers
    /// can issue them in either order; the toolbar UI typically writes
    /// both in the same frame when the user clicks a Connect-X button.
    SetConnectKind(crate::weave::WeaveEdgeKind),

    /// Per-tool sub-command. Forwarded to `MessageWriter<SelectCommand>`
    /// / `MessageWriter<CreateCommand>` etc. by the dispatcher.
    Tool(ToolCommand),

    /// Topology mutation against the replicated graph. Resolved through
    /// `GraphEntityIndex` by the dispatcher.
    Graph(GraphCommandExt),

    /// Sync-layer control: reconnect, snapshot recovery.
    Sync(SyncCommand),
}
