//! Compact snapshot of the converged comments state.
//!
//! [`CommentsSnapshot`] is the persistable form: a flat list of
//! [`CommentSnap`]s (deleted ones included so apply-order survives
//! restore). The server uses this for log compaction; clients restore
//! it during the `Welcome` handshake.

use serde::{Deserialize, Serialize};

use kyoso_crdt::id::{CrdtId, GlobalSeq};

/// Snapshot of one room's comments at sequence [`CommentsSnapshot::at_seq`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommentsSnapshot {
    pub at_seq: GlobalSeq,
    pub comments: Vec<CommentSnap>,
}

/// One comment's persistable state. `body` is `Option<String>` — `None`
/// means the comment was created but no body was stamped (shouldn't
/// happen with current ops since [`crate::op::CommentOpKind::AddComment`]
/// carries the initial body, but the field is optional for forward
/// compatibility).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommentSnap {
    pub id: CrdtId,
    pub anchor: CrdtId,
    pub parent: Option<CrdtId>,
    pub body: Option<String>,
    pub deleted: bool,
}

impl CommentsSnapshot {
    pub fn empty(at_seq: GlobalSeq) -> Self {
        Self {
            at_seq,
            comments: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.comments.is_empty()
    }

    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
