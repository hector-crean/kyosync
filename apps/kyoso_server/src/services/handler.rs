//! [`RoomModelHandler`] and [`HandlerFactory`] traits.
//!
//! Each CRDT model the server hosts is a [`RoomModelHandler`] impl. The
//! handler owns its model's storage, mirror, and append-lock; [`Room`]
//! is just a router that dispatches inbound envelopes by [`ModelId`]
//! and broadcasts the encoded `Apply` payload that comes back. Adding a
//! new model is a new handler crate + a [`HandlerFactory`] registered
//! at server startup — no patches to `Room` or `room_ws.rs`.
//!
//! ## Why payloads cross the trait boundary as bytes
//!
//! The trait is `dyn`-safe so [`Room`] can hold `HashMap<ModelId,
//! Arc<dyn RoomModelHandler>>`. That rules out generic associated
//! types for the model's `K_M` / `S_M`. Each handler decodes inside
//! its own methods against its own typed `Op<K_M>` / `Diff<K_M>` /
//! snapshot. The wire envelope already carries opaque per-model bytes
//! ([`kyoso_crdt::EnvelopeServerMsg::Apply::payload`]); this just
//! defers the decode by one method-call boundary.

use std::sync::Arc;

use async_trait::async_trait;
use kyoso_crdt::{GlobalSeq, ModelId, PeerId, RoomId, Tier};

use crate::Result;

/// Server-side per-model handler. One impl per CRDT model.
///
/// Conceptually the inverse of the per-model client plugin: where
/// [`kyoso_graph_sync::GraphSyncPlugin`] / [`kyoso_comments_sync::CommentsSyncPlugin`]
/// own the *client* state for their model, [`RoomModelHandler`] impls
/// own the *server* state.
#[async_trait]
pub trait RoomModelHandler: Send + Sync + 'static {
    /// Stable string slug matching the model's [`ModelId`]. Must match
    /// the slug the per-model client plugin uses.
    fn model_id(&self) -> ModelId;

    /// Decode the `Submit` payload as `Op<K_M>`, append to this model's
    /// own log, fold into its mirror, return the encoded stamped op
    /// for [`Room`] to broadcast as
    /// [`kyoso_crdt::EnvelopeServerMsg::Apply::payload`].
    ///
    /// Implementors are responsible for serialising concurrent submits
    /// — typically via an internal append-lock — so the model's
    /// `GlobalSeq` is monotonic.
    async fn submit(&self, payload: Vec<u8>) -> Result<Vec<u8>>;

    /// True if a peer at this `tier` is allowed to submit `payload`.
    /// Default: `ReadWrite` peers may submit anything; `Read` peers may
    /// not. Override on tier-permissive models (e.g. comments) to allow
    /// observer-tier writes for specific op kinds.
    ///
    /// Called *before* [`submit`](Self::submit) so a rejected op never
    /// touches the log. Cost is per-submit, not per-broadcast — fine to
    /// decode the payload here if a fine-grained policy needs it.
    fn allows_submit(&self, tier: Tier, _payload: &[u8]) -> bool {
        matches!(tier, Tier::ReadWrite)
    }

    /// Build this model's piece of the `Welcome` greeting for a peer
    /// joining at `since`. Returns `(Option<snapshot_payload>,
    /// diff_payload)` — both already postcard-encoded so [`Room`] can
    /// drop them straight into a [`kyoso_crdt::ModelGreeting`].
    async fn welcome_for(
        &self,
        since: GlobalSeq,
    ) -> Result<(Option<Vec<u8>>, Vec<u8>)>;

    /// Reply to a `Catchup` request. Encoded `Diff<K_M>`.
    async fn catchup(&self, since: GlobalSeq) -> Result<Vec<u8>>;

    /// Record a peer's per-model ack. Defaulted no-op for models
    /// without compaction (e.g. a comments handler that keeps its
    /// full log forever).
    async fn record_ack(
        &self,
        _peer: PeerId,
        _applied: GlobalSeq,
    ) -> Result<()> {
        Ok(())
    }

    /// Drop a peer's ack row (called on disconnect). Defaulted no-op
    /// for models that don't track acks.
    async fn release_peer(&self, _peer: PeerId) -> Result<()> {
        Ok(())
    }

    /// Take a snapshot for compaction. Defaulted no-op for models that
    /// don't snapshot.
    async fn take_snapshot(&self) -> Result<()> {
        Ok(())
    }

    /// Run one round of compaction GC. Returns the number of ops
    /// dropped. Defaulted no-op.
    async fn run_gc(&self) -> Result<u64> {
        Ok(0)
    }
}

/// Server-startup-time factory for [`RoomModelHandler`]s.
///
/// `AppState` holds a `Vec<Box<dyn HandlerFactory>>`; when a peer
/// joins a room for the first time, [`crate::services::RoomManager::get_or_create`]
/// iterates the factories and calls [`build`](Self::build) for each so
/// the room ends up with one handler per registered model.
///
/// To deploy a graph-only server: register only `GraphHandlerFactory`.
/// To deploy a server hosting graph + comments + a future presence log:
/// register all three. Same binary, different startup config.
#[async_trait]
pub trait HandlerFactory: Send + Sync + 'static {
    /// String slug matching what the produced handler will return from
    /// [`RoomModelHandler::model_id`].
    fn model_id(&self) -> ModelId;

    /// Construct a fresh handler for `room_id`. Implementors typically
    /// hydrate their model's mirror from persistent storage here so
    /// the handler is ready to serve a Welcome immediately.
    async fn build(&self, room_id: &RoomId) -> Result<Arc<dyn RoomModelHandler>>;
}
