//! Sync-layer commands and events — the leaf message types contributed
//! by the CRDT sync subsystem to the umbrella [`AppCommand`]/[`AppEvent`].
//!
//! These are the **external-facing** sync surface: connection lifecycle
//! and (optionally) a debug-only raw-op feed. The high-volume internal
//! WebSocket traffic stays inside [`kyoso_sync`] — these events are
//! semantic projections of it.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// External commands targeting the sync layer.
///
/// All variants are stubs in v1 — the underlying transport doesn't yet
/// support reconnect or forced snapshot recovery. The variants are wired
/// into the umbrella so the API surface is stable; the implementations
/// will land alongside the transport work tracked in §7 of the plan.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum SyncCommand {
    /// Force a reconnect attempt: re-issue `Hello { since: last_acked }`
    /// after a `Disconnected` status.
    Reconnect,

    /// Force a full snapshot recovery: re-issue `Hello { since: 0 }`,
    /// discard local state, restore from the server's snapshot + diff.
    RestoreFromSnapshot,
}

/// External observation of sync-layer state.
///
/// `Connected` and `Disconnected` are connection-lifecycle events that
/// fire once per transition. `OpApplied` is an optional debug feed that
/// surfaces every confirmed CRDT op — off by default; consumers opt in
/// via a sync-plugin flag (not yet wired).
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SyncEvent {
    /// CRDT handshake completed; assigned a peer id.
    Connected { peer: kyoso_crdt::PeerId },

    /// Connection dropped. No reconnect attempt yet (see [`SyncCommand::Reconnect`]).
    Disconnected,

    /// A CRDT op was confirmed (locally appended or remotely applied).
    /// Debug/audit feed; opt-in.
    OpApplied {
        peer: kyoso_crdt::PeerId,
        seq: kyoso_crdt::GlobalSeq,
    },
}
