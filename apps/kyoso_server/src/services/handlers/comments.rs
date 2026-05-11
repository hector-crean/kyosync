//! Comments model server-side handler.
//!
//! In-memory only for v1: holds an [`InMemoryOpLog<CommentOpKind>`]
//! and a [`CommentsBackend`] mirror per room. No GC, no snapshots —
//! comments are kept in their full log indefinitely. A persistent
//! comments handler would mirror the graph handler's storage shape
//! (an `OpStore`-equivalent for comments).

use std::sync::Arc;

use async_trait::async_trait;
use kyoso_comments_crdt::{CommentOpKind, CommentsBackend, comments_model};
use kyoso_crdt::{
    CrdtModel, Diff, GlobalSeq, InMemoryOpLog, ModelId, OpLogRead, OpLogWrite, RoomId, Tier,
};
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::services::handler::{HandlerFactory, RoomModelHandler};
use crate::Result;

type CommentOp = kyoso_crdt::Op<CommentOpKind>;

struct CommentsState {
    log: InMemoryOpLog<CommentOpKind>,
    mirror: CommentsBackend,
}

impl Default for CommentsState {
    fn default() -> Self {
        Self {
            log: InMemoryOpLog::new(),
            // Server peer reserved as 0 (matches graph handler).
            mirror: CommentsBackend::with_peer(0),
        }
    }
}

/// Per-room comments state.
pub struct CommentsRoomHandler {
    room_id: RoomId,
    state: Mutex<CommentsState>,
    /// Append-lock for the comments log. Independent of the graph
    /// handler's lock — concurrent graph submits don't block comment
    /// submits and vice versa.
    append_lock: Mutex<()>,
}

impl CommentsRoomHandler {
    pub fn new(room_id: RoomId) -> Self {
        Self {
            room_id,
            state: Mutex::new(CommentsState::default()),
            append_lock: Mutex::new(()),
        }
    }
}

#[async_trait]
impl RoomModelHandler for CommentsRoomHandler {
    fn model_id(&self) -> ModelId {
        comments_model()
    }

    /// Comments allow observer-tier writes. The CRDT itself is
    /// commutative regardless of who issues an op, and "any reader can
    /// comment" is the common product expectation. A future
    /// owner-only restriction would decode `payload` here and gate
    /// `EditBody` / `DeleteComment` on the embedded peer matching the
    /// connection's peer.
    fn allows_submit(&self, _tier: Tier, _payload: &[u8]) -> bool {
        true
    }

    async fn submit(&self, payload: Vec<u8>) -> Result<Vec<u8>> {
        let op: CommentOp = postcard::from_bytes(&payload)
            .map_err(|e| AppError::Internal(format!("decode comments Submit: {e}")))?;
        let _guard = self.append_lock.lock().await;
        let mut state = self.state.lock().await;
        let stamped = state.log.append(op);
        state.mirror.apply_remote(&stamped).map_err(|e| {
            AppError::Internal(format!("comments mirror apply {}: {e}", &self.room_id))
        })?;
        let bytes = postcard::to_allocvec(&stamped)
            .map_err(|e| AppError::Internal(format!("encode stamped comment op: {e}")))?;
        tracing::debug!(
            room = %self.room_id,
            seq = stamped.seq,
            peer = stamped.id.peer,
            kind = CommentsBackend::op_kind_label(&stamped),
            "comment op appended"
        );
        Ok(bytes)
    }

    async fn welcome_for(
        &self,
        since: GlobalSeq,
    ) -> Result<(Option<Vec<u8>>, Vec<u8>)> {
        let state = self.state.lock().await;
        let head = state.log.head();
        let ops = if head > since {
            state.log.slice(since, head)
        } else {
            Vec::new()
        };
        let diff: Diff<CommentOpKind> = Diff {
            from_seq: since,
            to_seq: head,
            ops,
        };
        let diff_payload = postcard::to_allocvec(&diff)
            .map_err(|e| AppError::Internal(format!("encode comments diff: {e}")))?;
        // No snapshots for comments yet — full log keeps everything.
        Ok((None, diff_payload))
    }

    async fn catchup(&self, since: GlobalSeq) -> Result<Vec<u8>> {
        let state = self.state.lock().await;
        let head = state.log.head();
        let ops = if head > since {
            state.log.slice(since, head)
        } else {
            Vec::new()
        };
        let diff: Diff<CommentOpKind> = Diff {
            from_seq: since,
            to_seq: head,
            ops,
        };
        postcard::to_allocvec(&diff)
            .map_err(|e| AppError::Internal(format!("encode comments catchup: {e}")))
    }

    // record_ack / release_peer / take_snapshot / run_gc default to no-op:
    // comments has no compaction in v1.
}

/// Factory for [`CommentsRoomHandler`]. Stateless — every handler is a
/// fresh in-memory log. A persistent comments deployment would carry
/// shared storage here, like [`super::GraphHandlerFactory`].
pub struct CommentsHandlerFactory;

impl CommentsHandlerFactory {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CommentsHandlerFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HandlerFactory for CommentsHandlerFactory {
    fn model_id(&self) -> ModelId {
        comments_model()
    }

    async fn build(&self, room_id: &RoomId) -> Result<Arc<dyn RoomModelHandler>> {
        Ok(Arc::new(CommentsRoomHandler::new(room_id.clone())))
    }
}
