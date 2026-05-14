//! Graph model server-side handler.
//!
//! Owns the graph [`OpStore`] (postgres or in-memory), the server-side
//! mirror ([`GraphBackend<OpaqueSchemaState>`]), the append-lock, and the
//! snapshot/compaction logic that used to live in [`crate::services::Room`]
//! before the per-model-handler refactor.

use std::sync::Arc;

use async_trait::async_trait;
use kyoso_crdt::{CrdtModel, GlobalSeq, ModelId, PeerId, RoomId};
use kyoso_graph_crdt::{GraphBackend, graph_model};
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::services::handler::{HandlerFactory, RoomModelHandler};
use crate::services::store::OpStore;
use crate::Result;

type ServerMirror = GraphBackend<kyoso_crdt::OpaqueSchemaState>;
type GraphOp = kyoso_crdt::Op<<ServerMirror as CrdtModel>::OpKind>;
type GraphDiff = kyoso_crdt::Diff<<ServerMirror as CrdtModel>::OpKind>;
type GraphState = <ServerMirror as CrdtModel>::State;

/// Per-room graph state.
pub struct GraphRoomHandler {
    room_id: RoomId,
    store: OpStore,
    mirror: Mutex<ServerMirror>,
    append_lock: Mutex<()>,
}

impl GraphRoomHandler {
    /// Hydrate the mirror from the latest snapshot + ops since.
    ///
    /// Mirrors the previous `Room::restore` flow exactly:
    /// 1. `ensure_room` (no-op if it already exists).
    /// 2. Restore the mirror from the latest snapshot, if any.
    /// 3. Apply every op since the snapshot to bring the mirror to head.
    pub async fn restore(room_id: RoomId, store: OpStore) -> Result<Self> {
        store.ensure_room(&room_id).await?;
        let mut mirror = ServerMirror::with_peer(0); // server peer is reserved as 0
        if let Some(snap) = store.latest_snapshot(&room_id).await? {
            mirror.restore(snap);
        }
        let head = store.head(&room_id).await?;
        let from = mirror.applied_seq();
        if head > from {
            let ops = store.slice(&room_id, from, head).await?;
            for op in &ops {
                mirror
                    .apply_remote(op)
                    .map_err(|e| AppError::Internal(format!("graph mirror replay {room_id}: {e}")))?;
            }
        }
        Ok(Self {
            room_id,
            store,
            mirror: Mutex::new(mirror),
            append_lock: Mutex::new(()),
        })
    }

    fn graph_model_id(&self) -> ModelId {
        graph_model()
    }
}

#[async_trait]
impl RoomModelHandler for GraphRoomHandler {
    fn model_id(&self) -> ModelId {
        self.graph_model_id()
    }

    async fn submit(&self, payload: Vec<u8>) -> Result<Vec<u8>> {
        let op: GraphOp = postcard::from_bytes(&payload)
            .map_err(|e| AppError::Internal(format!("decode graph Submit: {e}")))?;
        let _guard = self.append_lock.lock().await;
        let stamped = self.store.append(&self.room_id, op).await?;
        self.mirror
            .lock()
            .await
            .apply_remote(&stamped)
            .map_err(|e| AppError::Internal(format!("graph mirror apply {}: {e}", &self.room_id)))?;
        let bytes = postcard::to_allocvec(&stamped)
            .map_err(|e| AppError::Internal(format!("encode stamped graph op: {e}")))?;
        tracing::debug!(
            room = %self.room_id,
            seq = stamped.seq,
            peer = stamped.id.peer,
            kind = <ServerMirror as CrdtModel>::op_kind_label(&stamped),
            "graph op appended"
        );
        Ok(bytes)
    }

    async fn welcome_for(
        &self,
        since: GlobalSeq,
    ) -> Result<(Option<Vec<u8>>, Vec<u8>)> {
        let head = self.store.head(&self.room_id).await?;
        let (snapshot, diff): (Option<GraphState>, GraphDiff) = if head == 0 || head == since {
            (None, GraphDiff::empty(head))
        } else {
            let snap = self.store.latest_snapshot(&self.room_id).await?;
            match snap {
                Some(s) if s.at_seq > since => {
                    let ops = self.store.slice(&self.room_id, s.at_seq, head).await?;
                    let diff = GraphDiff {
                        from_seq: s.at_seq,
                        to_seq: head,
                        ops,
                    };
                    (Some(s), diff)
                }
                _ => {
                    let ops = self.store.slice(&self.room_id, since, head).await?;
                    let diff = GraphDiff {
                        from_seq: since,
                        to_seq: head,
                        ops,
                    };
                    (None, diff)
                }
            }
        };
        let snapshot_payload = match snapshot.as_ref().map(postcard::to_allocvec).transpose() {
            Ok(p) => p,
            Err(e) => {
                return Err(AppError::Internal(format!("encode graph snapshot: {e}")));
            }
        };
        let diff_payload = postcard::to_allocvec(&diff)
            .map_err(|e| AppError::Internal(format!("encode graph diff: {e}")))?;
        Ok((snapshot_payload, diff_payload))
    }

    async fn catchup(&self, since: GlobalSeq) -> Result<Vec<u8>> {
        let head = self.store.head(&self.room_id).await?;
        let ops = self.store.slice(&self.room_id, since, head).await?;
        let diff = GraphDiff {
            from_seq: since,
            to_seq: head,
            ops,
        };
        postcard::to_allocvec(&diff)
            .map_err(|e| AppError::Internal(format!("encode graph catchup: {e}")))
    }

    async fn record_ack(&self, peer: PeerId, applied: GlobalSeq) -> Result<()> {
        self.store.record_ack(&self.room_id, peer, applied).await
    }

    async fn release_peer(&self, peer: PeerId) -> Result<()> {
        self.store.clear_peer(&self.room_id, peer).await
    }

    async fn take_snapshot(&self) -> Result<()> {
        let snap = self.mirror.lock().await.snapshot();
        self.store.save_snapshot(&self.room_id, &snap).await
    }

    async fn run_gc(&self) -> Result<u64> {
        let min_ack = self.store.min_ack(&self.room_id).await?;
        let Some(min_ack) = min_ack else { return Ok(0) };
        let snap_seq = self
            .store
            .latest_snapshot(&self.room_id)
            .await?
            .map_or(0, |s| s.at_seq);
        let upper = min_ack.min(snap_seq);
        if upper == 0 {
            return Ok(0);
        }
        self.store.compact_below(&self.room_id, upper).await
    }
}

/// Factory for [`GraphRoomHandler`]. Holds the shared [`OpStore`] so
/// every room gets a handler bound to the same backend (postgres or
/// in-memory) — graph state is process-wide, not per-room.
pub struct GraphHandlerFactory {
    store: OpStore,
}

impl GraphHandlerFactory {
    pub fn new(store: OpStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl HandlerFactory for GraphHandlerFactory {
    fn model_id(&self) -> ModelId {
        graph_model()
    }

    async fn build(&self, room_id: &RoomId) -> Result<Arc<dyn RoomModelHandler>> {
        Ok(Arc::new(
            GraphRoomHandler::restore(room_id.clone(), self.store.clone()).await?,
        ))
    }
}
