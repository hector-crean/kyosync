//! [`CommentsClient`] — Bevy [`Resource`] wrapping the comments
//! [`CommentsBackend`].
//!
//! Equivalent to [`kyoso_graph_sync::ClientSyncEngine`] for graph
//! traffic: shares the peer-level [`IdGen`] from
//! [`kyoso_sync::PeerIdGen`] so cross-model `CrdtId` references stay
//! collision-free, and exposes the backend's mutating API so app
//! systems can `add_comment` / `edit_body` / `delete_comment` as plain
//! resource calls.

use bevy::prelude::*;
use kyoso_comments_crdt::{CommentOpKind, CommentsBackend, CommentsSnapshot};
use kyoso_crdt::{ApplyError, CrdtId, CrdtModel, GlobalSeq, IdGen, Op, PeerId};

type CommentOp = Op<CommentOpKind>;

/// Client-side comments sync resource.
#[derive(Resource)]
pub struct CommentsClient {
    inner: CommentsBackend,
}

impl Default for CommentsClient {
    fn default() -> Self {
        Self {
            inner: CommentsBackend::with_peer(0),
        }
    }
}

impl CommentsClient {
    /// Construct sharing `ids` with every other CRDT model on this
    /// peer. The plugin layer clones from
    /// [`kyoso_sync::PeerIdGen::handle`] and passes that here.
    #[must_use]
    pub fn with_shared_ids(ids: IdGen) -> Self {
        Self {
            inner: CommentsBackend::with_shared_ids(ids),
        }
    }

    pub fn set_peer(&mut self, peer: PeerId) {
        <CommentsBackend as CrdtModel>::set_peer(&mut self.inner, peer);
    }

    pub fn peer(&self) -> PeerId {
        self.inner.peer()
    }

    pub fn applied_seq(&self) -> GlobalSeq {
        self.inner.applied_seq()
    }

    /// Cloneable handle to the comments backend's id source. Identical
    /// to [`kyoso_sync::PeerIdGen::handle`] when constructed with
    /// `with_shared_ids`.
    pub fn ids(&self) -> &IdGen {
        self.inner.ids()
    }

    // -----------------------------------------------------------------
    // Mutating API — surfaces the backend's op-generating methods.
    // -----------------------------------------------------------------

    /// Mint a new comment anchored to `anchor` (typically a graph node
    /// `CrdtId`), optionally as a reply under `parent`. Returns the new
    /// comment's `CrdtId`. The originator pre-applies the create so
    /// `body(id)` returns `Some` immediately; the server echo is
    /// idempotent.
    pub fn add_comment(
        &mut self,
        anchor: CrdtId,
        parent: Option<CrdtId>,
        body: String,
    ) -> CrdtId {
        self.inner.add_comment(anchor, parent, body)
    }

    /// Queue an edit op (LWW by `(GlobalSeq, PeerId)`).
    pub fn edit_body(&mut self, target: CrdtId, body: String) {
        self.inner.edit_body(target, body);
    }

    /// Queue a soft-delete op.
    pub fn delete_comment(&mut self, target: CrdtId) {
        self.inner.delete_comment(target);
    }

    // -----------------------------------------------------------------
    // Read-side accessors.
    // -----------------------------------------------------------------

    pub fn comment_count(&self) -> usize {
        self.inner.comment_count()
    }

    pub fn body(&self, id: CrdtId) -> Option<&str> {
        self.inner.body(id)
    }

    pub fn anchor(&self, id: CrdtId) -> Option<CrdtId> {
        self.inner.anchor(id)
    }

    pub fn parent(&self, id: CrdtId) -> Option<Option<CrdtId>> {
        self.inner.parent(id)
    }

    pub fn is_deleted(&self, id: CrdtId) -> bool {
        self.inner.is_deleted(id)
    }

    // -----------------------------------------------------------------
    // Sync bookkeeping (called by the plugin's outbound/inbound systems).
    // -----------------------------------------------------------------

    pub(crate) fn apply_remote(&mut self, op: &CommentOp) -> Result<(), ApplyError> {
        <CommentsBackend as CrdtModel>::apply_remote(&mut self.inner, op)
    }

    pub(crate) fn drain_pending(&mut self) -> Vec<CommentOp> {
        <CommentsBackend as CrdtModel>::drain_pending(&mut self.inner)
    }

    pub(crate) fn restore(&mut self, snap: CommentsSnapshot) {
        <CommentsBackend as CrdtModel>::restore(&mut self.inner, snap);
    }
}
