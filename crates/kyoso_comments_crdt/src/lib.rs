//! Comments / threads / annotations CRDT data model.
//!
//! Implements [`kyoso_crdt::CrdtModel`] for a tree of comments. Each
//! comment is `(anchor, parent, body, deleted)` where:
//!
//! - `anchor` is a [`kyoso_crdt::CrdtId`] referring to an element in
//!   another model (typically a graph node from `kyoso_graph_crdt`).
//!   Cross-model references are safe because all models on a peer mint
//!   IDs from the same shared [`kyoso_crdt::IdGen`].
//! - `parent` is `None` for thread roots, `Some(comment_id)` for replies.
//! - `body` is an `LwwRegister<String>` — concurrent edits resolve by
//!   `(GlobalSeq, PeerId)`.
//! - `deleted` is an `LwwRegister<bool>` — soft-delete (the op record
//!   stays for tombstone GC; `deleted=true` makes it invisible to UI).
//!
//! See [`CommentsBackend`] for the entry point and [`CommentOpKind`]
//! for the wire ops. The comments model multiplexes onto the same
//! [`kyoso_crdt::EnvelopeServerMsg`] connection as the graph via
//! [`COMMENTS_MODEL_ID`].

pub mod backend;
pub mod op;
pub mod snapshot;

pub use backend::CommentsBackend;
pub use op::CommentOpKind;
pub use snapshot::{CommentSnap, CommentsSnapshot};

/// String-slug identifying the comments model on the multi-model wire
/// envelope. Stable; clients and servers must agree on the slug.
pub const COMMENTS_MODEL_ID: &str = "comments";

/// Convenience constructor for the comments model id.
#[must_use]
pub fn comments_model() -> kyoso_crdt::ModelId {
    kyoso_crdt::ModelId::new(COMMENTS_MODEL_ID)
}
