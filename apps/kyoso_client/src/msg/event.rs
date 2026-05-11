//! `AppEvent` — outgoing notifications from Bevy to external observers.
//!
//! Mirrors [`AppCommand`](super::command::AppCommand) on the way back
//! out. External consumers (a JS UI rebuilding a sidebar, an MCP tool
//! reporting state, telemetry/log sinks) read these to know what
//! happened in the Bevy world.
//!
//! `AppEvent` describes **observed state**, not the cause of it — both
//! local mutations (a user dragged something) and remote-applied CRDT
//! ops (a peer dragged something) emit the same kinds of events.
//!
//! ## Umbrella shape
//!
//! Each in-process plugin contributes one variant carrying its leaf
//! event type — symmetric to [`AppCommand`].
//!
//! - `Tool(ToolEvent)` — tool transitions and per-tool sub-events.
//! - `Graph(GraphMessageExt)` — semantic projections of replicated
//!   topology changes.
//! - `Sync(SyncEvent)` — connection lifecycle.
//! - `CommandError { message }` — generic failure surface.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::graph::GraphMessageExt;
use super::sync::SyncEvent;
use crate::tool::ToolEvent;

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum AppEvent {
    /// Per-tool sub-event or tool-transition observation.
    Tool(ToolEvent),

    /// Semantic projection of a replicated topology change.
    Graph(GraphMessageExt),

    /// Sync-layer state transition.
    Sync(SyncEvent),

    /// Something we couldn't act on. Not necessarily fatal — the
    /// caller may retry or surface to the user.
    CommandError { message: String },
}
