//! [`CrdtModel`] — the abstraction over a replicated data structure.
//!
//! Pluggable across data shapes (graph, text, list, JSON-CRDT, comments)
//! via two associated types: [`CrdtModel::OpKind`] (the per-op enum) and
//! [`CrdtModel::State`] (the persistable snapshot type). Each domain
//! crate defines its own backend type and implements this trait.
//!
//! ## What the trait abstracts
//!
//! - **Apply / snapshot / restore** — the core CRDT operations every
//!   replicated structure needs.
//! - **Pending op drain** — outbound side: the transport layer pulls
//!   locally-generated ops to ship to the server.
//! - **Applied-seq** — for liveness and compaction GC.
//!
//! [`ApplyError`] is the single error taxonomy every model returns from
//! `apply_remote`; it captures the two failure modes that exist for any
//! op-log replay (op not yet stamped, or applied out of order).

use serde::{Serialize, de::DeserializeOwned};

use crate::id::{GlobalSeq, PeerId};
use crate::op::Op;

/// Failure modes of [`CrdtModel::apply_remote`].
///
/// Both variants are recoverable: `Unconfirmed` means the caller needs
/// to wait for the server to stamp the op before re-applying;
/// `OutOfOrder` means a gap in the op stream — request a catchup.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApplyError {
    #[error("op has no global sequence assigned yet")]
    Unconfirmed,
    #[error("expected seq {expected}, got {got}")]
    OutOfOrder { expected: GlobalSeq, got: GlobalSeq },
}

/// A replicated data structure addressed by [`CrdtId`](crate::CrdtId)
/// with operations stamped by a server-assigned [`GlobalSeq`].
///
/// Implementors are typically held inside a Bevy `Resource` (the
/// graph backend) or inside the server's per-room mirror.
pub trait CrdtModel: Default + Send + Sync + 'static {
    /// The op-kind enum this model accepts. Wire frames carry
    /// [`Op<Self::OpKind>`] as their payload; the server log stores
    /// `Op<Self::OpKind>` blobs.
    type OpKind: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Persistent state captured by [`Self::snapshot`] / restored by
    /// [`Self::restore`]. The server stores this in the snapshots
    /// table; clients restore it during the `Welcome` handshake.
    type State: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Set this replica's `PeerId` once the server's `Welcome` arrives.
    /// Implementations use this to attribute newly-minted [`Op::id`]s.
    fn set_peer(&mut self, peer: PeerId);

    /// Highest server-confirmed [`GlobalSeq`] applied to this replica.
    fn applied_seq(&self) -> GlobalSeq;

    /// Apply an op (locally-generated or remotely-broadcast) to the
    /// replica state. Implementations are expected to be **idempotent**:
    /// applying the same op twice is a no-op.
    fn apply_remote(&mut self, op: &Op<Self::OpKind>) -> Result<(), ApplyError>;

    /// Take a snapshot of the current state. The server uses this for
    /// compaction; clients use it for snapshot-based recovery via the
    /// `Welcome` handshake.
    fn snapshot(&self) -> Self::State;

    /// Replace the entire replica state with a snapshot. Used during
    /// recovery (`Welcome { snapshot, … }`).
    fn restore(&mut self, snap: Self::State);

    /// Drain locally-generated ops that have accumulated since the last
    /// call. The transport layer flushes these to the server. Returns
    /// an empty `Vec` if no local mutations are pending.
    fn drain_pending(&mut self) -> Vec<Op<Self::OpKind>>;

    /// Stable string label for an op, used by server-side telemetry.
    /// Defaults to a generic "Op"; concrete impls override for
    /// finer-grained logging (matching on the variants of their
    /// `OpKind`).
    fn op_kind_label(_op: &Op<Self::OpKind>) -> &'static str {
        "Op"
    }
}
