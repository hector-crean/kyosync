//! Sync-layer commands and events — the leaf message types contributed
//! by the CRDT sync subsystem to the umbrella [`AppCommand`]/[`AppEvent`].

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SyncEvent {
    /// CRDT handshake completed; assigned a peer id.
    Connected { peer: kyoso_crdt::PeerId },

    /// Connection dropped. No reconnect attempt yet.
    Disconnected,

    /// A CRDT op was confirmed (locally appended or remotely applied).
    /// Debug/audit feed; opt-in.
    OpApplied {
        peer: kyoso_crdt::PeerId,
        seq: kyoso_crdt::GlobalSeq,
    },
}
