//! [`Room`] — one collaboration boundary, multiple per-model handlers.
//!
//! Post per-model-handler refactor `Room` is a thin router:
//!
//! - **`handlers`** — `HashMap<ModelId, Arc<dyn RoomModelHandler>>`
//!   built by [`RoomManager::get_or_create`] from the
//!   [`HandlerFactory`] list in [`crate::AppState`]. Each handler owns
//!   its model's storage, mirror, append-lock, and snapshot policy.
//! - **`broadcast`** — multi-model fan-out. Per-handler `submit`
//!   returns the encoded `Apply` payload; `Room` wraps it in
//!   [`EnvelopeServerMsg::Apply`] and broadcasts.
//! - **`presence`** — model-agnostic. Lives at the room level because
//!   presence isn't per-model (a peer is "in the room", not "in the
//!   graph model"). Bypasses every handler.
//! - **`next_peer`** — room-wide peer-id assignment, model-agnostic.
//!
//! The graph and comments fields, locks, and per-model methods that
//! used to live here have all moved to [`crate::services::handlers`].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dashmap::DashMap;
use kyoso_crdt::{EnvelopeServerMsg, GlobalSeq, ModelGreeting, ModelId, PeerId, RoomId, Tier};
use tokio::sync::{Mutex, broadcast};

use crate::error::{AppError, Result};
use crate::services::handler::{HandlerFactory, RoomModelHandler};

const BROADCAST_CAPACITY: usize = 256;

pub struct Room {
    pub id: RoomId,
    handlers: HashMap<ModelId, Arc<dyn RoomModelHandler>>,
    broadcast: broadcast::Sender<EnvelopeServerMsg>,
    next_peer: AtomicU32,
    /// Ephemeral per-peer presence/awareness state. `Vec<u8>` is opaque
    /// — consumers postcard-encode their own struct. Cleared on peer
    /// disconnect; never persisted, model-agnostic.
    presence: Mutex<HashMap<PeerId, Vec<u8>>>,
}

impl Room {
    /// Construct a room hosting the given pre-built handlers. Typically
    /// called by [`RoomManager::get_or_create`] after iterating the
    /// app's [`HandlerFactory`] list.
    pub fn from_handlers(id: RoomId, handlers: Vec<Arc<dyn RoomModelHandler>>) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let handlers = handlers
            .into_iter()
            .map(|h| (h.model_id(), h))
            .collect();
        Self {
            id,
            handlers,
            broadcast: tx,
            next_peer: AtomicU32::new(1),
            presence: Mutex::new(HashMap::new()),
        }
    }

    /// Run every registered factory and assemble a fresh room. Hook
    /// for [`RoomManager::get_or_create`].
    pub async fn restore(
        id: RoomId,
        factories: &[Box<dyn HandlerFactory>],
    ) -> Result<Self> {
        let mut handlers = Vec::with_capacity(factories.len());
        for factory in factories {
            handlers.push(factory.build(&id).await?);
        }
        Ok(Self::from_handlers(id, handlers))
    }

    pub fn assign_peer(&self) -> PeerId {
        self.next_peer.fetch_add(1, Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EnvelopeServerMsg> {
        self.broadcast.subscribe()
    }

    /// True if this room has a handler registered for `model`.
    #[must_use]
    pub fn has_model(&self, model: &ModelId) -> bool {
        self.handlers.contains_key(model)
    }

    // -----------------------------------------------------------------
    // Per-model dispatch — the router fits in three methods.
    // -----------------------------------------------------------------

    /// Forward a `Submit` payload to the handler for `model`, then
    /// broadcast the encoded `Apply` payload it returns. The handler's
    /// own [`RoomModelHandler::allows_submit`] gates the submit by the
    /// connection's `tier` — a `Tier::Read` connection trying to
    /// submit a graph op gets [`AppError::PermissionDenied`].
    pub async fn submit(
        &self,
        model: &ModelId,
        tier: Tier,
        payload: Vec<u8>,
    ) -> Result<()> {
        let handler = self.lookup(model)?;
        if !handler.allows_submit(tier, &payload) {
            return Err(AppError::PermissionDenied(format!(
                "tier {tier:?} cannot submit to model {model}"
            )));
        }
        let apply_payload = handler.submit(payload).await?;
        let _ = self.broadcast.send(EnvelopeServerMsg::Apply {
            model: model.clone(),
            payload: apply_payload,
        });
        Ok(())
    }

    /// Build the per-model greeting list for a `Welcome`. `models` is
    /// the `Hello.models` list the client sent — `(ModelId, since)`
    /// pairs.
    pub async fn welcome_for(
        &self,
        models: &[(ModelId, GlobalSeq)],
    ) -> Result<Vec<ModelGreeting>> {
        let mut out = Vec::with_capacity(models.len());
        for (model, since) in models {
            let handler = self.lookup(model)?;
            let (snapshot_payload, diff_payload) = handler.welcome_for(*since).await?;
            out.push(ModelGreeting {
                model: model.clone(),
                snapshot_payload,
                diff_payload,
            });
        }
        Ok(out)
    }

    pub async fn catchup(&self, model: &ModelId, since: GlobalSeq) -> Result<Vec<u8>> {
        self.lookup(model)?.catchup(since).await
    }

    pub async fn record_ack(
        &self,
        model: &ModelId,
        peer: PeerId,
        applied_seq: GlobalSeq,
    ) -> Result<()> {
        // Models without compaction silently no-op via the trait
        // default. Unknown model is still an error.
        self.lookup(model)?.record_ack(peer, applied_seq).await
    }

    /// Disconnect cleanup: tell every handler to drop the peer's ack
    /// row and clear room-wide presence.
    pub async fn release_peer(&self, peer: PeerId) -> Result<()> {
        for handler in self.handlers.values() {
            handler.release_peer(peer).await?;
        }
        self.clear_presence(peer).await;
        Ok(())
    }

    fn lookup(&self, model: &ModelId) -> Result<&Arc<dyn RoomModelHandler>> {
        self.handlers
            .get(model)
            .ok_or_else(|| AppError::Internal(format!("unknown model: {model}")))
    }

    // -----------------------------------------------------------------
    // Presence — model-agnostic, lives at the room level.
    // -----------------------------------------------------------------

    /// Replace `peer`'s presence entry and broadcast the update.
    /// Bypasses the op log entirely — presence is ephemeral, in-memory,
    /// not persisted, and not seq'd.
    pub async fn update_presence(&self, peer: PeerId, state: Vec<u8>) {
        self.presence.lock().await.insert(peer, state.clone());
        let _ = self
            .broadcast
            .send(EnvelopeServerMsg::PresenceUpdate { peer, state });
    }

    /// Remove `peer` from the presence map and broadcast the departure.
    /// No-op if the peer wasn't in the map.
    pub async fn clear_presence(&self, peer: PeerId) {
        let removed = self.presence.lock().await.remove(&peer).is_some();
        if removed {
            let _ = self
                .broadcast
                .send(EnvelopeServerMsg::PresenceLeft { peer });
        }
    }

    /// Snapshot of the current presence map. Sent inside the welcome
    /// envelope so a joining peer can hydrate its UI without a
    /// round-trip.
    pub async fn presence_snapshot(&self) -> Vec<(PeerId, Vec<u8>)> {
        self.presence
            .lock()
            .await
            .iter()
            .map(|(p, s)| (*p, s.clone()))
            .collect()
    }

    // -----------------------------------------------------------------
    // Scheduler hooks — fan out across every handler.
    // -----------------------------------------------------------------

    /// Take a snapshot for every handler. Models that don't snapshot
    /// (default trait impl) are no-ops. Errors from individual handlers
    /// are logged but don't abort the loop — one model's GC failure
    /// shouldn't take down a sibling model's snapshot.
    pub async fn take_snapshot_all(&self) {
        for (model, handler) in &self.handlers {
            if let Err(e) = handler.take_snapshot().await {
                tracing::warn!(room = %self.id, %model, error = %e, "take_snapshot failed");
            }
        }
    }

    /// Run GC for every handler; return the total number of ops
    /// dropped across all models (for telemetry).
    pub async fn run_gc_all(&self) -> u64 {
        let mut total = 0;
        for (model, handler) in &self.handlers {
            match handler.run_gc().await {
                Ok(n) => total += n,
                Err(e) => {
                    tracing::warn!(room = %self.id, %model, error = %e, "run_gc failed");
                }
            }
        }
        total
    }
}

pub struct RoomManager {
    factories: Arc<Vec<Box<dyn HandlerFactory>>>,
    rooms: DashMap<RoomId, Arc<Room>>,
}

impl RoomManager {
    pub fn new(factories: Arc<Vec<Box<dyn HandlerFactory>>>) -> Self {
        Self {
            factories,
            rooms: DashMap::new(),
        }
    }

    /// Resolve `id` to a `Room`, creating + restoring on first access.
    /// Two concurrent calls for the same id return the same `Arc`; the
    /// loser's freshly-built `Room` is dropped harmlessly.
    pub async fn get_or_create(&self, id: &str) -> Result<Arc<Room>> {
        if let Some(existing) = self.rooms.get(id) {
            return Ok(existing.clone());
        }
        let room = Arc::new(Room::restore(id.to_string(), &self.factories).await?);
        let stored = self
            .rooms
            .entry(id.to_string())
            .or_insert(room)
            .clone();
        Ok(stored)
    }

    pub fn live_rooms(&self) -> Vec<Arc<Room>> {
        self.rooms.iter().map(|r| r.value().clone()).collect()
    }

    pub fn count(&self) -> usize {
        self.rooms.len()
    }

    /// The startup-time factory list. Useful for tests that want to
    /// peek at which models the server hosts.
    #[must_use]
    pub fn factories(&self) -> &[Box<dyn HandlerFactory>] {
        &self.factories
    }
}
