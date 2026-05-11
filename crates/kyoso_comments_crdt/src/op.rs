//! Comments-model operation kinds.
//!
//! Each variant is wrapped in [`kyoso_crdt::Op`]`<CommentOpKind>` and
//! shipped through the wire envelope as the comments model's payload.

use serde::{Deserialize, Serialize};

use kyoso_crdt::id::CrdtId;

/// One CRDT operation against the comments backend.
///
/// `AddComment` uses the operation's own [`CrdtId`] as the new comment
/// id (matching the convention from the graph model).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CommentOpKind {
    /// Create a comment with id = enclosing op's [`CrdtId`].
    ///
    /// `anchor` references the element this thread is attached to —
    /// typically a [`kyoso_graph_crdt`](kyoso_graph_crdt)::OpKind-minted
    /// node id, but the comments backend doesn't validate it; the
    /// peer-shared [`kyoso_crdt::IdGen`] is what makes the cross-model
    /// reference safe.
    ///
    /// `parent` is `None` for thread roots, `Some(comment_id)` for replies.
    /// The initial `body` rides along on the create op so a fresh comment
    /// is never observed empty.
    AddComment {
        anchor: CrdtId,
        parent: Option<CrdtId>,
        body: String,
    },
    /// Replace the body of an existing comment. LWW by `(GlobalSeq, PeerId)`
    /// of the enclosing op.
    EditBody { target: CrdtId, body: String },
    /// Soft-delete a comment. LWW: a concurrent `EditBody` with a higher
    /// stamp wins.
    DeleteComment { target: CrdtId },
}
