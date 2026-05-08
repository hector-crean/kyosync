//! [`Room`] — one CRDT-replicated document with all its connected peers.
//!
//! Backed by an [`OpStore`] (in-memory or postgres). On construction the
//! room restores its server-side **mirror** ([`CrdtBackend`]) from the
//! most recent persisted snapshot plus any ops since, so that snapshot
//! generation is a cheap in-memory operation rather than a full replay
//! of the persistent log.
//!
//! Concurrency:
//!
//! - `append_lock` serialises [`Room::submit`] across concurrent
//!   submitters so seq assignment + mirror update + broadcast happen as
//!   one logical step.
//! - `mirror` itself is a `Mutex` so background workers (snapshot
//!   scheduler) can read state without contending with submit.
//! - `broadcast` is the standard tokio fan-out channel; per-connection
//!   tasks subscribe once and run independently.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dashmap::DashMap;
use kyoso_crdt::{CrdtModel, GlobalSeq, PeerId, RoomId};
use kyoso_graph_crdt::CrdtBackend;
use tokio::sync::{Mutex, broadcast};

use crate::error::{AppError, Result};
use crate::model::{Diff, Op, ServerMsg, ServerModel, Snapshot};
use crate::services::store::OpStore;

const BROADCAST_CAPACITY: usize = 256;

/// Mirror node/edge attribute type. The server doesn't care about
/// consumer-side attribute components — only topology, tree-shape, and
/// snapshots. `()` satisfies the `Debug + Send + Sync + 'static` bounds
/// without dragging in any consumer types.
type ServerMirror = CrdtBackend<(), ()>;

pub struct Room {
    pub id: RoomId,
    store: OpStore,
    mirror: Mutex<ServerMirror>,
    append_lock: Mutex<()>,
    broadcast: broadcast::Sender<ServerMsg>,
    next_peer: AtomicU32,
    /// Ephemeral per-peer presence/awareness state. `Vec<u8>` is opaque
    /// — consumers postcard-encode their own struct. Cleared on peer
    /// disconnect; never persisted to the op log.
    presence: Mutex<HashMap<PeerId, Vec<u8>>>,
}

impl Room {
    /// Restore room state from the persistent store and return a ready
    /// `Room`. Does:
    ///
    /// 1. `ensure_room` (no-op if it already exists).
    /// 2. Hydrate the mirror from the latest snapshot, if any.
    /// 3. Apply every op since the snapshot to bring the mirror to head.
    pub async fn restore(id: RoomId, store: OpStore) -> Result<Self> {
        store.ensure_room(&id).await?;
        let mut mirror = ServerMirror::with_peer(0); // server peer is reserved as 0
        if let Some(snap) = store.latest_snapshot(&id).await? {
            mirror.restore(snap);
        }
        let head = store.head(&id).await?;
        let from = mirror.applied_seq();
        if head > from {
            let ops = store.slice(&id, from, head).await?;
            for op in &ops {
                mirror
                    .apply_remote(op)
                    .map_err(|e| AppError::Internal(format!("mirror replay {}: {e}", &id)))?;
            }
        }

        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            id,
            store,
            mirror: Mutex::new(mirror),
            append_lock: Mutex::new(()),
            broadcast: tx,
            next_peer: AtomicU32::new(1),
            presence: Mutex::new(HashMap::new()),
        })
    }

    pub fn assign_peer(&self) -> PeerId {
        self.next_peer.fetch_add(1, Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerMsg> {
        self.broadcast.subscribe()
    }

    pub async fn head(&self) -> Result<GlobalSeq> {
        self.store.head(&self.id).await
    }

    /// Append `op` to the durable log, fold it into the server-side
    /// mirror, and broadcast to every subscribed peer.
    pub async fn submit(&self, op: Op) -> Result<Op> {
        let _guard = self.append_lock.lock().await;
        let stamped = self.store.append(&self.id, op).await?;
        self.mirror
            .lock()
            .await
            .apply_remote(&stamped)
            .map_err(|e| AppError::Internal(format!("mirror apply {}: {e}", &self.id)))?;
        let subscribers = self
            .broadcast
            .send(ServerMsg::Apply(stamped.clone()))
            .unwrap_or(0);
        tracing::debug!(
            room = %self.id,
            seq = stamped.seq,
            peer = stamped.id.peer,
            kind = op_kind_label(&stamped),
            subscribers,
            "op appended"
        );
        Ok(stamped)
    }

    /// Materialise everything a client needs to bridge from `since` to
    /// `head`. Returns `(Option<Snapshot>, Diff)`:
    ///
    /// - If a snapshot newer than `since` is available, hand it back; the
    ///   diff is then ops `(snapshot.at_seq, head]`.
    /// - Otherwise just hand back ops `(since, head]`.
    pub async fn welcome_for(&self, since: GlobalSeq) -> Result<(Option<Snapshot>, Diff)> {
        let head = self.store.head(&self.id).await?;
        if head == 0 || head == since {
            return Ok((None, Diff::empty(head)));
        }
        let snap = self.store.latest_snapshot(&self.id).await?;
        match snap {
            Some(s) if s.at_seq > since => {
                let ops = self.store.slice(&self.id, s.at_seq, head).await?;
                let diff = Diff {
                    from_seq: s.at_seq,
                    to_seq: head,
                    ops,
                };
                Ok((Some(s), diff))
            }
            _ => {
                let ops = self.store.slice(&self.id, since, head).await?;
                let diff = Diff {
                    from_seq: since,
                    to_seq: head,
                    ops,
                };
                Ok((None, diff))
            }
        }
    }

    pub async fn catchup(&self, since: GlobalSeq) -> Result<Diff> {
        let head = self.store.head(&self.id).await?;
        let ops = self.store.slice(&self.id, since, head).await?;
        Ok(Diff {
            from_seq: since,
            to_seq: head,
            ops,
        })
    }

    pub async fn record_ack(&self, peer: PeerId, applied_seq: GlobalSeq) -> Result<()> {
        self.store.record_ack(&self.id, peer, applied_seq).await
    }

    pub async fn release_peer(&self, peer: PeerId) -> Result<()> {
        self.clear_presence(peer).await;
        self.store.clear_peer(&self.id, peer).await
    }

    /// Replace `peer`'s presence entry and broadcast the update.
    /// Bypasses the op log entirely — presence is ephemeral, in-memory,
    /// not persisted, and not seq'd.
    pub async fn update_presence(&self, peer: PeerId, state: Vec<u8>) {
        self.presence.lock().await.insert(peer, state.clone());
        let _ = self
            .broadcast
            .send(ServerMsg::PresenceUpdate { peer, state });
    }

    /// Remove `peer` from the presence map and broadcast the departure.
    /// No-op if the peer wasn't in the map.
    pub async fn clear_presence(&self, peer: PeerId) {
        let removed = self.presence.lock().await.remove(&peer).is_some();
        if removed {
            let _ = self.broadcast.send(ServerMsg::PresenceLeft { peer });
        }
    }

    /// Snapshot of the current presence map. Sent inside [`ServerMsg::Welcome`]
    /// so a joining peer can hydrate its UI without a round-trip.
    pub async fn presence_snapshot(&self) -> Vec<(PeerId, Vec<u8>)> {
        self.presence
            .lock()
            .await
            .iter()
            .map(|(p, s)| (*p, s.clone()))
            .collect()
    }

    /// Take a snapshot of the current mirror and persist it. Returns the
    /// snapshot for callers that want to log or broadcast the cut.
    pub async fn take_snapshot(&self) -> Result<Snapshot> {
        let snap = self.mirror.lock().await.snapshot();
        self.store.save_snapshot(&self.id, &snap).await?;
        Ok(snap)
    }

    /// Run one round of compaction: drop ops below
    /// `min(min_ack across connected peers, latest snapshot.at_seq)`.
    /// Returns the number of ops removed (`0` if no compaction was
    /// performed because either no peers are connected or no snapshot
    /// has been taken yet).
    pub async fn run_gc(&self) -> Result<u64> {
        let min_ack = self.store.min_ack(&self.id).await?;
        let Some(min_ack) = min_ack else { return Ok(0) };
        let snap_seq = self
            .store
            .latest_snapshot(&self.id)
            .await?
            .map_or(0, |s| s.at_seq);
        let upper = min_ack.min(snap_seq);
        if upper == 0 {
            return Ok(0);
        }
        self.store.compact_below(&self.id, upper).await
    }
}

pub struct RoomManager {
    store: OpStore,
    rooms: DashMap<RoomId, Arc<Room>>,
}

impl RoomManager {
    pub fn new(store: OpStore) -> Self {
        Self {
            store,
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
        let room = Arc::new(Room::restore(id.to_string(), self.store.clone()).await?);
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

    pub fn store(&self) -> &OpStore {
        &self.store
    }
}

/// Stable label for an op variant, used in tracing fields.
///
/// Delegates to the configured [`ServerModel`]'s `op_kind_label` impl
/// so it stays accurate when [`crate::model::ServerModel`] is swapped
/// for a different CRDT.
fn op_kind_label(op: &Op) -> &'static str {
    <ServerModel as CrdtModel>::op_kind_label(op)
}
