//! Stable global identifiers for CRDT-replicated elements.
//!
//! Identifiers are independent of any local handle (Bevy `Entity`, petgraph
//! `NodeIndex`, etc.) so peers can refer to the same element across
//! replicas without coordinating on local IDs.
//!
//! ## Sharing across models
//!
//! All CRDT models on the same peer mint IDs from a shared
//! [`IdGen`] — a cloneable handle around an `Arc<Mutex<IdGenerator>>`.
//! That keeps `(PeerId, LocalSeq)` globally unique across the graph,
//! comments, and any other model on the same peer, which in turn lets
//! cross-model references (e.g. a comment anchored to a graph node) be
//! plain [`CrdtId`] values with no model tag.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Unique identifier for a peer (a single client process). Assigned by the
/// server on session start.
pub type PeerId = u32;

/// Per-peer monotonic counter. Combined with [`PeerId`] in [`CrdtId`], this
/// is what makes ops globally unique without coordination.
pub type LocalSeq = u64;

/// Server-assigned global sequence number. Each model's op log is totally
/// ordered by this; replaying ops in `GlobalSeq` order on any replica
/// yields the same converged state for that model.
pub type GlobalSeq = u64;

/// Stable global identifier for an element (node, edge, comment, …) or
/// for an individual operation.
///
/// Two-tuple of `(peer, seq)` where `seq` is the peer's local counter.
/// Once minted on a peer, this ID is permanent and shared across all
/// replicas. The encoding is varint-friendly (small peer IDs and dense
/// counters compress well under postcard).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
    schemars::JsonSchema,
)]
pub struct CrdtId {
    pub peer: PeerId,
    pub seq: LocalSeq,
}

impl CrdtId {
    pub const fn new(peer: PeerId, seq: LocalSeq) -> Self {
        Self { peer, seq }
    }
}

impl std::fmt::Display for CrdtId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.peer, self.seq)
    }
}

/// Per-peer counter for issuing fresh [`CrdtId`]s. The mutable state
/// behind an [`IdGen`] handle — production code holds an [`IdGen`]
/// rather than an `IdGenerator` directly so multiple model backends on
/// the same peer can share one counter.
#[derive(Debug, Default, Clone)]
pub struct IdGenerator {
    peer: PeerId,
    next: LocalSeq,
}

impl IdGenerator {
    pub const fn new(peer: PeerId) -> Self {
        Self { peer, next: 0 }
    }

    /// Restart a generator at the given counter — used after restoring
    /// from a snapshot so the next minted id doesn't collide with one
    /// already present in the snapshot.
    pub const fn resume(peer: PeerId, next: LocalSeq) -> Self {
        Self { peer, next }
    }

    pub const fn peer(&self) -> PeerId {
        self.peer
    }

    pub const fn next_seq(&self) -> LocalSeq {
        self.next
    }

    /// Mint a new globally unique ID under this peer.
    pub fn next(&mut self) -> CrdtId {
        let id = CrdtId::new(self.peer, self.next);
        self.next += 1;
        id
    }
}

/// Cloneable handle to a peer-level [`IdGenerator`].
///
/// All CRDT model backends on a single peer should mint IDs through one
/// of these so the per-peer `LocalSeq` counter is shared. Cross-model
/// references then need only a [`CrdtId`] — no model tag, no
/// disambiguation, because IDs are globally unique across all models on
/// the peer.
///
/// The handle is `Send + Sync + Clone`; cloning increments the `Arc`
/// refcount. Lock contention is negligible in practice because op
/// minting happens in foreground engine systems, not in hot async paths.
#[derive(Debug, Clone)]
pub struct IdGen(Arc<Mutex<IdGenerator>>);

impl IdGen {
    /// Construct a fresh handle for `peer` with `next = 0`.
    #[must_use]
    pub fn new(peer: PeerId) -> Self {
        Self(Arc::new(Mutex::new(IdGenerator::new(peer))))
    }

    /// Mint a new globally unique ID under this peer. Atomic across
    /// clones of the handle.
    pub fn next(&self) -> CrdtId {
        self.0.lock().unwrap().next()
    }

    /// Read the peer id this handle is currently bound to.
    pub fn peer(&self) -> PeerId {
        self.0.lock().unwrap().peer()
    }

    /// Read the next-to-be-minted local sequence (without minting).
    pub fn next_seq(&self) -> LocalSeq {
        self.0.lock().unwrap().next_seq()
    }

    /// Reset this handle to a different peer with `next = 0`.
    ///
    /// Visible to every clone of the handle. Intended for use during
    /// the server `Welcome` flow where the peer first learns its
    /// `PeerId`; calling this after IDs have been minted under the old
    /// peer would invalidate any pending ops still referencing them.
    pub fn set_peer(&self, peer: PeerId) {
        *self.0.lock().unwrap() = IdGenerator::new(peer);
    }

    /// Ensure `next_seq()` is at least `seq`. Used during snapshot
    /// restore to advance past any local IDs already present in the
    /// snapshot. No-op if `next_seq()` is already `>= seq`.
    pub fn bump_to(&self, seq: LocalSeq) {
        let mut g = self.0.lock().unwrap();
        if g.next_seq() < seq {
            *g = IdGenerator::resume(g.peer(), seq);
        }
    }
}

impl Default for IdGen {
    fn default() -> Self {
        Self::new(0)
    }
}
