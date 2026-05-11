//! `AppEvent` — outgoing notifications from Bevy to external observers.
//!
//! Mirrors [`AppCommand`](super::command::AppCommand) on the way back
//! out. External consumers read these to know what happened in the Bevy
//! world.

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

    /// Something we couldn't act on. Not necessarily fatal — the caller
    /// may retry or surface to the user.
    CommandError { message: String },
}
