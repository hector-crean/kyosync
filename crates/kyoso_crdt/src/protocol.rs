//! Wire protocol shared between clients and the [`kyoso_server`] coordinator.
//!
//! Frames are postcard-encoded and exchanged over a binary WebSocket. Two
//! one-way streams in opposite directions: [`ClientMsg`] carries client →
//! server intents, [`ServerMsg`] carries confirmations and broadcasts.
//!
//! Both message enums are generic over the model's `K` (op-kind enum) and
//! `S` (snapshot type). Apps using a single model bind concrete types
//! at the call site (e.g. `ClientMsg<kyoso_graph_crdt::OpKind>`); the
//! multi-model envelope on top of this is added in a later phase.
//!
//! ## Lifecycle
//!
//! ```text
//!   Client                               Server
//!   ──────                               ──────
//!   Hello { room, since }   ─────────►
//!                           ◄─────────   Welcome { peer, diff }
//!
//!   Submit(local_op)        ─────────►
//!                                        (assign seq, persist, broadcast)
//!                           ◄─────────   Apply(stamped_op)         ┐ to all
//!                                        Apply(stamped_op)         ┘ peers
//!
//!   Catchup { since }       ─────────►
//!                           ◄─────────   Catchup(diff)
//!
//!   Ping                    ─────────►
//!                           ◄─────────   Pong
//! ```
//!
//! `Welcome` always responds to `Hello`, `Catchup` only on demand, `Apply`
//! is server-initiated whenever any peer's op is confirmed.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::id::{GlobalSeq, PeerId};
use crate::op::{Diff, Op};

/// Unique room identifier. Documents are isolated by room.
pub type RoomId = String;

/// Client → server frames.
///
/// CRDT ops travel via [`ClientMsg::Submit`] and live forever in the
/// log. Awareness data (cursors, selections, "user X is editing") is
/// fundamentally different — ephemeral, no ordering, lost on disconnect
/// — so it gets its own variants ([`ClientMsg::Presence`] /
/// [`ClientMsg::LeavePresence`]) that the server handles on a separate
/// hot path: no log, no seq, just broadcast.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg<K> {
    /// Join a room. `since` is the highest [`GlobalSeq`] the client
    /// already has applied; on a fresh load it is `0`.
    Hello { room: RoomId, since: GlobalSeq },
    /// Submit a locally-generated op. The op's [`Op::seq`] field is `None`;
    /// the server assigns one on append.
    Submit(Op<K>),
    /// Request every op since `since`. Used to recover after a temporary
    /// disconnect or a missed broadcast.
    Catchup { since: GlobalSeq },
    /// Liveness probe + ack piggyback. `applied_seq` is the highest
    /// [`GlobalSeq`] the client has currently applied; the server uses
    /// this to compute the safe-to-compact threshold across all peers.
    Ping { applied_seq: GlobalSeq },
    /// Replace this peer's presence state with `state`. Server stores
    /// the latest value in memory only and broadcasts to every other
    /// peer as [`ServerMsg::PresenceUpdate`]. The contents are opaque —
    /// consumers postcard-encode their own struct (cursor + selection
    /// + colour + display name, etc.) into the bytes.
    Presence(Vec<u8>),
    /// Explicitly clear this peer's presence (also happens implicitly
    /// on disconnect). Server broadcasts [`ServerMsg::PresenceLeft`].
    LeavePresence,
}

/// Server → client frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg<K, S> {
    /// Reply to [`ClientMsg::Hello`]. Assigns the client a [`PeerId`] and
    /// gives them everything they need to reach `head`:
    ///
    /// - If `snapshot` is `Some`, the client must `restore` it first,
    ///   then apply every op in `diff` (which begins at `snapshot.at_seq`).
    /// - If `snapshot` is `None`, the client is already past the latest
    ///   snapshot — apply `diff` directly on top of their current state.
    /// - `presence` is the current room-wide presence snapshot — every
    ///   other peer's most recent [`ClientMsg::Presence`] state. The
    ///   joiner can hydrate any UI bound to peer awareness without a
    ///   round-trip.
    Welcome {
        peer: PeerId,
        snapshot: Option<S>,
        diff: Diff<K>,
        presence: Vec<(PeerId, Vec<u8>)>,
    },
    /// Broadcast: a new op (from any peer in the room) has been confirmed
    /// and stamped with a global sequence. Apply locally.
    Apply(Op<K>),
    /// Reply to [`ClientMsg::Catchup`]. Same compaction-safety
    /// considerations as the diff inside `Welcome` — if the client is
    /// behind `compacted_below` they have to re-handshake to receive a
    /// snapshot.
    Catchup(Diff<K>),
    /// Reply to [`ClientMsg::Ping`].
    Pong,
    /// Server-side rejection or transport-level problem. The connection
    /// is not necessarily closed — the client may retry.
    Error { message: String },
    /// Broadcast: peer `peer` updated their presence. Replaces any
    /// previous state for that peer.
    PresenceUpdate { peer: PeerId, state: Vec<u8> },
    /// Broadcast: peer `peer` either explicitly cleared their presence
    /// or disconnected.
    PresenceLeft { peer: PeerId },
}

impl<K: Serialize + DeserializeOwned> ClientMsg<K> {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

impl<K: Serialize + DeserializeOwned, S: Serialize + DeserializeOwned> ServerMsg<K, S> {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
