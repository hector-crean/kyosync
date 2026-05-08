//! Stable global identifiers for CRDT-replicated graph elements.
//!
//! Identifiers are independent of any local handle (Bevy `Entity`, petgraph
//! `NodeIndex`, etc.) so peers can refer to the same node/edge across
//! replicas without coordinating on local IDs.

use serde::{Deserialize, Serialize};

/// Unique identifier for a peer (a single client process). Assigned by the
/// server on session start.
pub type PeerId = u32;

/// Per-peer monotonic counter. Combined with [`PeerId`] in [`CrdtId`], this
/// is what makes ops globally unique without coordination.
pub type LocalSeq = u64;

/// Server-assigned global sequence number. The op log is totally ordered
/// by this; replaying ops in `GlobalSeq` order on any replica yields the
/// same converged state.
pub type GlobalSeq = u64;

/// Stable global identifier for a graph element (node or edge) and for
/// individual operations.
///
/// Two-tuple of `(peer, seq)` where `seq` is the peer's local counter.
/// Once minted on a peer, this ID is permanent and shared across all
/// replicas. The encoding is varint-friendly (small peer IDs and dense
/// counters compress well under postcard).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

/// Per-peer counter for issuing fresh [`CrdtId`]s. One of these lives per
/// `CrdtBackend` instance; bump and read for each new op.
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
    /// from a [`Snapshot`](crate::snapshot::Snapshot) so the next minted
    /// id doesn't collide with one already present in the snapshot.
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
