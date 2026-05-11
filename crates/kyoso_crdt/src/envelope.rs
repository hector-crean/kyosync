//! Multi-model wire envelope.
//!
//! One WebSocket carries traffic for multiple CRDT models (graph,
//! comments, presence, …). Each on-the-wire frame is an
//! [`EnvelopeClientMsg`] / [`EnvelopeServerMsg`] tagged with a
//! [`ModelId`]; the per-model payload is the postcard-encoded
//! `Op<K_M>` / `Diff<K_M>` for that model.
//!
//! The framework decodes only the envelope; routing the inner payload
//! to the right model handler is the responsibility of the transport
//! layer (a `ModelHost` registry on both client and server).
//!
//! Why a string-slug `ModelId` for v1: it is debuggable, third-party
//! models can register without coordinating numeric ids, and the wire
//! cost is small (postcard length-prefixes the slug). A `u16` registry
//! is a future tightening if message sizes start to matter.

use serde::{Deserialize, Serialize};

use crate::id::{GlobalSeq, PeerId};
use crate::protocol::RoomId;

/// Identifies which CRDT model a wire frame targets.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(pub String);

impl ModelId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModelId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ModelId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Bandwidth/permission tier a connection requested at `Hello` time.
///
/// The tier is a coarse capacity classifier — it does NOT carry
/// fine-grained authz; per-op permission lives on the model handler
/// (`ModelHandler::allows_submit`). Servers may grant a *lower* tier
/// than requested (e.g., observer-only when the requester has no
/// write permission); the granted tier is echoed in
/// [`EnvelopeServerMsg::Welcome::tier_granted`].
///
/// Capacity implications (see HARNESS.md Layer 4c):
/// - `ReadWrite` peers go on the live broadcast path: per-op
///   `Apply` frames, no buffering. Latency budget ≤ 50 ms p99.
/// - `Read` peers go on the coalesced broadcast path: per-tick
///   `ApplyBatch` frames flushed at a fixed cadence. Latency
///   budget ~250 ms but the room can hold ~10× more peers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// Full live participation. Every locally-issued op is allowed
    /// (subject to per-model handler policy) and every remote op
    /// is delivered uncoalesced.
    ReadWrite,
    /// Observer with eventual-consistency reads. May still submit
    /// model-specific ops the handler permits at this tier
    /// (default: comments). Remote ops are delivered as coalesced
    /// `ApplyBatch` frames on the reader fanout cadence.
    Read,
}

/// Per-model welcome data — snapshot (optional) and diff since the
/// client's `since` cursor for that model.
///
/// `snapshot_payload` is `Some(encoded(S_M))` when the server has a
/// snapshot newer than the client's cursor; otherwise `None` and the
/// client applies `diff_payload` on top of its current state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelGreeting {
    pub model: ModelId,
    pub snapshot_payload: Option<Vec<u8>>,
    pub diff_payload: Vec<u8>,
}

/// Client → server envelope.
///
/// Per-model variants (`Submit`, `Catchup`, `Ping`) carry encoded
/// payloads; the recipient decodes them according to the registered
/// model handler. `Hello`, `Presence`, and `LeavePresence` are global
/// (model-agnostic) — they exist once per connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EnvelopeClientMsg {
    /// Join a room. `models` lists each model the client wants to
    /// subscribe to plus its current `since` (highest applied seq for
    /// that model). Fresh joiners pass `0`. `tier` is the requested
    /// bandwidth/permission tier — server may grant a lower tier
    /// (echoed in [`EnvelopeServerMsg::Welcome::tier_granted`]).
    Hello {
        room: RoomId,
        tier: Tier,
        models: Vec<(ModelId, GlobalSeq)>,
    },
    /// Submit a locally-generated op for one model. `payload` is the
    /// postcard-encoded `kyoso_crdt::Op<K_M>` for that model.
    Submit { model: ModelId, payload: Vec<u8> },
    /// Request every op the client hasn't seen yet for this model.
    Catchup { model: ModelId, since: GlobalSeq },
    /// Liveness probe + ack piggyback for one model. `applied_seq` is
    /// this client's highest applied seq for that model.
    Ping {
        model: ModelId,
        applied_seq: GlobalSeq,
    },
    /// Replace this peer's presence state. Opaque bytes — consumers
    /// postcard-encode their own struct (cursor + selection + …).
    Presence(Vec<u8>),
    /// Explicitly clear this peer's presence (also implicit on
    /// disconnect).
    LeavePresence,
}

/// Server → client envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EnvelopeServerMsg {
    /// Reply to `Hello`. Assigns the client a `PeerId` and includes a
    /// per-model greeting (snapshot + diff) for every registered model
    /// the client subscribed to. `tier_granted` is the tier the server
    /// actually assigned (≤ requested).
    Welcome {
        peer: PeerId,
        tier_granted: Tier,
        models: Vec<ModelGreeting>,
        presence: Vec<(PeerId, Vec<u8>)>,
    },
    /// Broadcast: a new op (for one model) has been confirmed. `payload`
    /// is the postcard-encoded `kyoso_crdt::Op<K_M>` with the assigned
    /// `seq` populated. Sent on the live (uncoalesced) fanout path —
    /// `ReadWrite`-tier peers receive every op as one of these frames.
    Apply { model: ModelId, payload: Vec<u8> },
    /// Broadcast (coalesced reader path): a batch of ops for one model,
    /// stamped in `GlobalSeq` order. `Read`-tier peers receive these
    /// flushed at a fixed cadence (default 250 ms). Apply each
    /// `payload` in order, exactly as you would `Apply::payload`.
    ApplyBatch {
        model: ModelId,
        payloads: Vec<Vec<u8>>,
    },
    /// Reply to `Catchup`. `payload` is the postcard-encoded
    /// `kyoso_crdt::Diff<K_M>`.
    Catchup { model: ModelId, payload: Vec<u8> },
    Pong,
    Error {
        message: String,
    },
    PresenceUpdate {
        peer: PeerId,
        state: Vec<u8>,
    },
    PresenceLeft {
        peer: PeerId,
    },
}

impl EnvelopeClientMsg {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

impl EnvelopeServerMsg {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
