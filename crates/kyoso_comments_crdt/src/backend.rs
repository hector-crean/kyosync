//! [`CommentsBackend`] — the comments CRDT replica.
//!
//! Local mutations (`add_comment`, `edit_body`, `delete_comment`)
//! generate [`Op`]`<CommentOpKind>`s that accumulate in
//! [`CommentsBackend::pending`]. The transport layer drains them and
//! ships them through the multi-model envelope under
//! [`crate::COMMENTS_MODEL_ID`].
//!
//! IDs are minted from a shared [`IdGen`] handle — the same handle the
//! graph model uses on the peer — so comment-to-graph anchor references
//! never collide.

use std::collections::HashMap;

use kyoso_crdt::context::{CausalContext, CausalState};
use kyoso_crdt::id::{CrdtId, GlobalSeq, IdGen, LocalSeq, PeerId};
use kyoso_crdt::lattice::Crdt;
use kyoso_crdt::model::{ApplyError, CrdtModel};
use kyoso_crdt::op::Op;
use kyoso_crdt::types::{LwwMut, LwwRegister};

use crate::op::CommentOpKind;
use crate::snapshot::{CommentSnap, CommentsSnapshot};

/// Per-comment bookkeeping inside [`CommentsBackend`].
struct CommentRecord {
    anchor: CrdtId,
    parent: Option<CrdtId>,
    body: LwwRegister<String>,
    deleted: LwwRegister<bool>,
}

/// CRDT-replicated comments storage. Implements
/// [`CrdtModel`](kyoso_crdt::CrdtModel) so it slots into the framework's
/// model registry on both client and server.
pub struct CommentsBackend {
    /// Shared id source — clone the same handle the graph backend uses
    /// so anchor references stay collision-free.
    ids: IdGen,
    comments: HashMap<CrdtId, CommentRecord>,
    pending: Vec<Op<CommentOpKind>>,
    applied_seq: GlobalSeq,
    causal: CausalState,
}

impl Default for CommentsBackend {
    fn default() -> Self {
        Self::with_peer(0)
    }
}

impl CommentsBackend {
    /// Construct with a fresh, owned [`IdGen`] handle. For multi-model
    /// peers — where the graph and comments backends must share one
    /// counter — use [`Self::with_shared_ids`].
    #[must_use]
    pub fn with_peer(peer: PeerId) -> Self {
        Self::with_shared_ids(IdGen::new(peer))
    }

    /// Construct sharing `ids` with other CRDT models on the same peer.
    /// This is the production path: clone
    /// [`ClientSyncEngine::ids`](kyoso_sync::ClientSyncEngine::ids) and
    /// pass the clone here so anchor `CrdtId`s line up across models.
    #[must_use]
    pub fn with_shared_ids(ids: IdGen) -> Self {
        Self {
            ids,
            comments: HashMap::new(),
            pending: Vec::new(),
            applied_seq: 0,
            causal: CausalState::new(),
        }
    }

    pub fn peer(&self) -> PeerId {
        self.ids.peer()
    }

    pub fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq
    }

    /// Cloneable handle to this backend's id source.
    pub fn ids(&self) -> &IdGen {
        &self.ids
    }

    /// Live comment count (excludes soft-deleted).
    #[must_use]
    pub fn comment_count(&self) -> usize {
        self.comments
            .values()
            .filter(|c| !c.deleted.get().copied().unwrap_or(false))
            .count()
    }

    /// Read a live comment's body. `None` if unknown or deleted.
    #[must_use]
    pub fn body(&self, id: CrdtId) -> Option<&str> {
        let rec = self.comments.get(&id)?;
        if rec.deleted.get().copied().unwrap_or(false) {
            return None;
        }
        rec.body.get().map(String::as_str)
    }

    /// Read a comment's anchor. Returns `Some` even for tombstoned
    /// comments — late-arriving threads may need to find their anchor.
    #[must_use]
    pub fn anchor(&self, id: CrdtId) -> Option<CrdtId> {
        Some(self.comments.get(&id)?.anchor)
    }

    /// Read a comment's parent (None for thread roots).
    #[must_use]
    pub fn parent(&self, id: CrdtId) -> Option<Option<CrdtId>> {
        Some(self.comments.get(&id)?.parent)
    }

    /// True iff this comment has been soft-deleted.
    #[must_use]
    pub fn is_deleted(&self, id: CrdtId) -> bool {
        self.comments
            .get(&id)
            .map(|c| c.deleted.get().copied().unwrap_or(false))
            .unwrap_or(false)
    }

    // ---------------------------------------------------------------
    // Op generation
    // ---------------------------------------------------------------

    /// Mint a new comment, queue an [`OpKind::AddComment`] op, and
    /// return its id. The local replica pre-applies the create so the
    /// originator sees the comment immediately; the server echo is then
    /// idempotent (`or_insert`).
    pub fn add_comment(&mut self, anchor: CrdtId, parent: Option<CrdtId>, body: String) -> CrdtId {
        let id = self.ids.next();
        let mut rec = CommentRecord {
            anchor,
            parent,
            body: LwwRegister::default(),
            deleted: LwwRegister::default(),
        };
        // Pre-stamp the body with `seq = None` so a server-confirmed
        // remote edit (Some(seq)) always wins on collision. Same trick
        // graph CrdtBackend uses for AddNode.
        let mut ctx = CausalContext::new(id, None, &mut self.causal);
        rec.body.mutate(LwwMut::Set(body.clone()), &mut ctx);
        self.comments.insert(id, rec);
        self.pending.push(Op::new(
            id,
            CommentOpKind::AddComment {
                anchor,
                parent,
                body,
            },
        ));
        id
    }

    /// Queue an [`OpKind::EditBody`] op. Local visibility waits for the
    /// server echo (LWW stamp ordering would be wrong with pre-apply,
    /// same as graph's `Document<S>::mutate_node`).
    pub fn edit_body(&mut self, target: CrdtId, body: String) {
        let op_id = self.ids.next();
        self.pending.push(Op::new(
            op_id,
            CommentOpKind::EditBody { target, body },
        ));
    }

    /// Queue a [`OpKind::DeleteComment`] op.
    pub fn delete_comment(&mut self, target: CrdtId) {
        let op_id = self.ids.next();
        self.pending
            .push(Op::new(op_id, CommentOpKind::DeleteComment { target }));
    }
}

impl CrdtModel for CommentsBackend {
    type OpKind = CommentOpKind;
    type State = CommentsSnapshot;

    fn set_peer(&mut self, peer: PeerId) {
        self.ids.set_peer(peer);
    }

    fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq
    }

    fn apply_remote(&mut self, op: &Op<CommentOpKind>) -> Result<(), ApplyError> {
        let seq = op.seq.ok_or(ApplyError::Unconfirmed)?;
        if seq <= self.applied_seq {
            return Ok(()); // already applied
        }
        if seq != self.applied_seq + 1 {
            return Err(ApplyError::OutOfOrder {
                expected: self.applied_seq + 1,
                got: seq,
            });
        }

        match &op.kind {
            CommentOpKind::AddComment {
                anchor,
                parent,
                body,
            } => {
                let rec = self.comments.entry(op.id).or_insert_with(|| CommentRecord {
                    anchor: *anchor,
                    parent: *parent,
                    body: LwwRegister::default(),
                    deleted: LwwRegister::default(),
                });
                let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
                let _ = rec.body.apply(
                    &kyoso_crdt::types::LwwDelta { value: body.clone() },
                    &ctx,
                );
            }
            CommentOpKind::EditBody { target, body } => {
                if let Some(rec) = self.comments.get_mut(target) {
                    let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
                    let _ = rec.body.apply(
                        &kyoso_crdt::types::LwwDelta { value: body.clone() },
                        &ctx,
                    );
                }
            }
            CommentOpKind::DeleteComment { target } => {
                if let Some(rec) = self.comments.get_mut(target) {
                    let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
                    let _ = rec
                        .deleted
                        .apply(&kyoso_crdt::types::LwwDelta { value: true }, &ctx);
                }
            }
        }

        self.applied_seq = seq;
        Ok(())
    }

    /// Snapshot. Comments are sorted by [`CrdtId`] so structurally
    /// identical replicas produce equal snapshots — useful for
    /// snapshot diffing and the chaos-sim convergence check.
    fn snapshot(&self) -> CommentsSnapshot {
        let mut comments: Vec<CommentSnap> = self
            .comments
            .iter()
            .map(|(id, rec)| CommentSnap {
                id: *id,
                anchor: rec.anchor,
                parent: rec.parent,
                body: rec.body.get().cloned(),
                deleted: rec.deleted.get().copied().unwrap_or(false),
            })
            .collect();
        comments.sort_by_key(|c| c.id);
        CommentsSnapshot {
            at_seq: self.applied_seq,
            comments,
        }
    }

    fn restore(&mut self, snap: CommentsSnapshot) {
        self.comments.clear();
        self.applied_seq = snap.at_seq;

        let my_peer = self.ids.peer();
        let mut max_my_seq: Option<LocalSeq> = None;
        for c in &snap.comments {
            if c.id.peer == my_peer {
                max_my_seq = Some(max_my_seq.map_or(c.id.seq, |s| s.max(c.id.seq)));
            }
        }
        if let Some(seq) = max_my_seq {
            // Bump (don't replace) — other models sharing this handle
            // may already have minted higher seqs.
            self.ids.bump_to(seq + 1);
        }

        for c in snap.comments {
            // Snapshot stores resolved bodies; restoring them as LWW
            // means subsequent EditBody ops with a higher stamp will
            // still win, which is what we want.
            let mut body = LwwRegister::default();
            if let Some(b) = c.body {
                let mut ctx = CausalContext::new(c.id, Some(self.applied_seq), &mut self.causal);
                body.mutate(LwwMut::Set(b), &mut ctx);
            }
            let mut deleted = LwwRegister::default();
            if c.deleted {
                let mut ctx = CausalContext::new(c.id, Some(self.applied_seq), &mut self.causal);
                deleted.mutate(LwwMut::Set(true), &mut ctx);
            }
            self.comments.insert(
                c.id,
                CommentRecord {
                    anchor: c.anchor,
                    parent: c.parent,
                    body,
                    deleted,
                },
            );
        }
    }

    fn drain_pending(&mut self) -> Vec<Op<CommentOpKind>> {
        std::mem::take(&mut self.pending)
    }

    fn op_kind_label(op: &Op<CommentOpKind>) -> &'static str {
        match &op.kind {
            CommentOpKind::AddComment { .. } => "AddComment",
            CommentOpKind::EditBody { .. } => "EditBody",
            CommentOpKind::DeleteComment { .. } => "DeleteComment",
        }
    }
}
