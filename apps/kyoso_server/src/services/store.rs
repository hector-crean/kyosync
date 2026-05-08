//! Persistent storage for op logs, snapshots, and peer acks.
//!
//! [`OpStore`] is the single abstraction the rest of the server talks to.
//! Two backends:
//!
//! - **In-memory** — `OpStore::in_memory()`. No persistence. Used by tests.
//! - **Postgres** — `OpStore::postgres(url)`. Schema in
//!   `migrations/`; migrations run on connect.
//!
//! Per-room concurrency: append operations are serialized inside [`Room`]
//! via its async mutex, so the store doesn't need its own locking around
//! seq assignment for single-instance deployments. Multi-instance setups
//! would need either Postgres advisory locks or a leader-elected log
//! writer — out of scope for v1.
//!
//! All `Op` and `Snapshot` blobs are postcard-encoded so the wire format,
//! the on-disk format, and the in-memory format match exactly. Future
//! schema migrations on the op shape can be handled with a version byte
//! prefix; we don't need that yet.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use kyoso_crdt::{GlobalSeq, PeerId, RoomId};
use sqlx::postgres::{PgPool, PgPoolOptions};
use tokio::sync::Mutex;

use crate::error::{AppError, Result};
use crate::model::{Op, Snapshot};

#[derive(Clone)]
pub struct OpStore {
    inner: OpStoreInner,
}

#[derive(Clone)]
enum OpStoreInner {
    InMemory(Arc<Mutex<InMemoryStorage>>),
    Postgres(PgPool),
}

impl OpStore {
    /// Construct an in-memory store. State is lost on drop; use
    /// [`OpStore::postgres`] for anything you want to survive restarts.
    pub fn in_memory() -> Self {
        Self {
            inner: OpStoreInner::InMemory(Arc::new(Mutex::new(InMemoryStorage::default()))),
        }
    }

    /// Connect to Postgres at `database_url`, run migrations, and clear
    /// stale `peer_acks` rows from the previous run (no peers are
    /// connected at startup).
    pub async fn postgres(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .connect(database_url)
            .await
            .map_err(|e| AppError::Internal(format!("connect: {e}")))?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| AppError::Internal(format!("migrate: {e}")))?;
        sqlx::query("DELETE FROM peer_acks")
            .execute(&pool)
            .await
            .map_err(|e| AppError::Internal(format!("clear acks: {e}")))?;
        Ok(Self {
            inner: OpStoreInner::Postgres(pool),
        })
    }

    // ------------------------------------------------------------------
    // Rooms
    // ------------------------------------------------------------------

    /// Create the room row if absent. Idempotent.
    pub async fn ensure_room(&self, room: &RoomId) -> Result<()> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                s.lock().await.rooms.entry(room.clone()).or_default();
                Ok(())
            }
            OpStoreInner::Postgres(pool) => {
                sqlx::query("INSERT INTO rooms (id) VALUES ($1) ON CONFLICT (id) DO NOTHING")
                    .bind(room)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Internal(format!("ensure_room: {e}")))?;
                Ok(())
            }
        }
    }

    pub async fn list_rooms(&self) -> Result<Vec<RoomId>> {
        match &self.inner {
            OpStoreInner::InMemory(s) => Ok(s.lock().await.rooms.keys().cloned().collect()),
            OpStoreInner::Postgres(pool) => {
                let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM rooms")
                    .fetch_all(pool)
                    .await
                    .map_err(|e| AppError::Internal(format!("list_rooms: {e}")))?;
                Ok(rows.into_iter().map(|(s,)| s).collect())
            }
        }
    }

    // ------------------------------------------------------------------
    // Op log
    // ------------------------------------------------------------------

    /// Append `op`, assigning the next [`GlobalSeq`]. Returns the stamped
    /// op so the caller can broadcast it.
    pub async fn append(&self, room: &RoomId, op: Op) -> Result<Op> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let mut g = s.lock().await;
                let state = g.rooms.entry(room.clone()).or_default();
                state.head += 1;
                let stamped = op.with_seq(state.head);
                state.ops.insert(state.head, stamped.clone());
                Ok(stamped)
            }
            OpStoreInner::Postgres(pool) => {
                let blob = postcard::to_allocvec(&op)?;
                let row: (i64,) = sqlx::query_as(
                    r#"
                    WITH bumped AS (
                        UPDATE rooms
                           SET next_seq = next_seq + 1
                         WHERE id = $1
                     RETURNING next_seq - 1 AS assigned
                    )
                    INSERT INTO ops (room_id, global_seq, op_blob)
                         SELECT $1, assigned, $2 FROM bumped
                      RETURNING global_seq
                    "#,
                )
                .bind(room)
                .bind(&blob)
                .fetch_one(pool)
                .await
                .map_err(|e| AppError::Internal(format!("append: {e}")))?;
                Ok(op.with_seq(row.0 as GlobalSeq))
            }
        }
    }

    pub async fn head(&self, room: &RoomId) -> Result<GlobalSeq> {
        match &self.inner {
            OpStoreInner::InMemory(s) => Ok(s
                .lock()
                .await
                .rooms
                .get(room)
                .map_or(0, |r| r.head)),
            OpStoreInner::Postgres(pool) => {
                let row: Option<(i64,)> =
                    sqlx::query_as("SELECT next_seq - 1 FROM rooms WHERE id = $1")
                        .bind(room)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| AppError::Internal(format!("head: {e}")))?;
                Ok(row.map_or(0, |(n,)| n as GlobalSeq))
            }
        }
    }

    /// `(from_seq, to_seq]`. Returns ops in seq order. May exclude ops
    /// below `compacted_below`; callers expecting a complete history
    /// should use [`OpStore::latest_snapshot`] first.
    pub async fn slice(
        &self,
        room: &RoomId,
        from_seq: GlobalSeq,
        to_seq: GlobalSeq,
    ) -> Result<Vec<Op>> {
        if from_seq >= to_seq {
            return Ok(Vec::new());
        }
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let g = s.lock().await;
                let Some(state) = g.rooms.get(room) else {
                    return Ok(Vec::new());
                };
                Ok(state
                    .ops
                    .range((from_seq + 1)..=to_seq)
                    .map(|(_, op)| op.clone())
                    .collect())
            }
            OpStoreInner::Postgres(pool) => {
                let rows: Vec<(Vec<u8>,)> = sqlx::query_as(
                    "SELECT op_blob FROM ops
                     WHERE room_id = $1 AND global_seq > $2 AND global_seq <= $3
                     ORDER BY global_seq",
                )
                .bind(room)
                .bind(from_seq as i64)
                .bind(to_seq as i64)
                .fetch_all(pool)
                .await
                .map_err(|e| AppError::Internal(format!("slice: {e}")))?;
                rows.into_iter()
                    .map(|(blob,)| postcard::from_bytes::<Op>(&blob).map_err(AppError::Codec))
                    .collect()
            }
        }
    }

    // ------------------------------------------------------------------
    // Snapshots
    // ------------------------------------------------------------------

    pub async fn save_snapshot(&self, room: &RoomId, snap: &Snapshot) -> Result<()> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let mut g = s.lock().await;
                let state = g.rooms.entry(room.clone()).or_default();
                state.snapshots.insert(snap.at_seq, snap.clone());
                state.snapshot_seq = state.snapshot_seq.max(snap.at_seq);
                Ok(())
            }
            OpStoreInner::Postgres(pool) => {
                let blob = snap.encode()?;
                let mut tx = pool
                    .begin()
                    .await
                    .map_err(|e| AppError::Internal(format!("tx begin: {e}")))?;
                sqlx::query(
                    "INSERT INTO snapshots (room_id, at_seq, blob) VALUES ($1, $2, $3)
                     ON CONFLICT (room_id, at_seq) DO UPDATE SET blob = EXCLUDED.blob",
                )
                .bind(room)
                .bind(snap.at_seq as i64)
                .bind(&blob)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Internal(format!("snapshot insert: {e}")))?;
                sqlx::query(
                    "UPDATE rooms SET snapshot_seq = GREATEST(snapshot_seq, $2) WHERE id = $1",
                )
                .bind(room)
                .bind(snap.at_seq as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Internal(format!("snapshot bump: {e}")))?;
                tx.commit()
                    .await
                    .map_err(|e| AppError::Internal(format!("tx commit: {e}")))?;
                Ok(())
            }
        }
    }

    /// Most recent snapshot for `room`, or `None` if none exists.
    pub async fn latest_snapshot(&self, room: &RoomId) -> Result<Option<Snapshot>> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let g = s.lock().await;
                Ok(g.rooms
                    .get(room)
                    .and_then(|r| r.snapshots.values().next_back().cloned()))
            }
            OpStoreInner::Postgres(pool) => {
                let row: Option<(Vec<u8>,)> = sqlx::query_as(
                    "SELECT blob FROM snapshots
                     WHERE room_id = $1
                     ORDER BY at_seq DESC LIMIT 1",
                )
                .bind(room)
                .fetch_optional(pool)
                .await
                .map_err(|e| AppError::Internal(format!("latest_snapshot: {e}")))?;
                row.map(|(blob,)| Snapshot::decode(&blob).map_err(AppError::Codec))
                    .transpose()
            }
        }
    }

    // ------------------------------------------------------------------
    // Peer acks (for compaction)
    // ------------------------------------------------------------------

    pub async fn record_ack(
        &self,
        room: &RoomId,
        peer: PeerId,
        last_seen: GlobalSeq,
    ) -> Result<()> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let mut g = s.lock().await;
                let state = g.rooms.entry(room.clone()).or_default();
                let entry = state.acks.entry(peer).or_insert(0);
                *entry = (*entry).max(last_seen);
                Ok(())
            }
            OpStoreInner::Postgres(pool) => {
                sqlx::query(
                    "INSERT INTO peer_acks (room_id, peer_id, last_seen_seq)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (room_id, peer_id) DO UPDATE
                       SET last_seen_seq = GREATEST(peer_acks.last_seen_seq, EXCLUDED.last_seen_seq),
                           updated_at = NOW()",
                )
                .bind(room)
                .bind(i64::from(peer))
                .bind(last_seen as i64)
                .execute(pool)
                .await
                .map_err(|e| AppError::Internal(format!("record_ack: {e}")))?;
                Ok(())
            }
        }
    }

    pub async fn clear_peer(&self, room: &RoomId, peer: PeerId) -> Result<()> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let mut g = s.lock().await;
                if let Some(state) = g.rooms.get_mut(room) {
                    state.acks.remove(&peer);
                }
                Ok(())
            }
            OpStoreInner::Postgres(pool) => {
                sqlx::query("DELETE FROM peer_acks WHERE room_id = $1 AND peer_id = $2")
                    .bind(room)
                    .bind(i64::from(peer))
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Internal(format!("clear_peer: {e}")))?;
                Ok(())
            }
        }
    }

    /// Lowest `last_seen_seq` across all currently-tracked peers in this
    /// room. `None` when no peers are connected.
    pub async fn min_ack(&self, room: &RoomId) -> Result<Option<GlobalSeq>> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let g = s.lock().await;
                Ok(g.rooms
                    .get(room)
                    .and_then(|r| r.acks.values().min().copied()))
            }
            OpStoreInner::Postgres(pool) => {
                let row: Option<(i64,)> =
                    sqlx::query_as("SELECT MIN(last_seen_seq) FROM peer_acks WHERE room_id = $1")
                        .bind(room)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| AppError::Internal(format!("min_ack: {e}")))?;
                Ok(row.and_then(|(n,)| (n >= 0).then_some(n as GlobalSeq)))
            }
        }
    }

    // ------------------------------------------------------------------
    // Compaction
    // ------------------------------------------------------------------

    /// Delete ops with `global_seq <= upper`. Returns the number of ops
    /// deleted. Updates `compacted_below` so subsequent slices can detect
    /// when callers ask for ops that are no longer present.
    pub async fn compact_below(&self, room: &RoomId, upper: GlobalSeq) -> Result<u64> {
        match &self.inner {
            OpStoreInner::InMemory(s) => {
                let mut g = s.lock().await;
                let Some(state) = g.rooms.get_mut(room) else {
                    return Ok(0);
                };
                let before = state.ops.len();
                state.ops.retain(|seq, _| *seq > upper);
                let deleted = (before - state.ops.len()) as u64;
                state.compacted_below = state.compacted_below.max(upper);
                // Also drop snapshots strictly below the new threshold —
                // we keep at most one snapshot at-or-above compacted_below.
                let keep_at_or_above: GlobalSeq = state
                    .snapshots
                    .keys()
                    .copied()
                    .filter(|s| *s <= upper)
                    .max()
                    .unwrap_or(0);
                state.snapshots.retain(|s, _| *s >= keep_at_or_above);
                Ok(deleted)
            }
            OpStoreInner::Postgres(pool) => {
                let mut tx = pool
                    .begin()
                    .await
                    .map_err(|e| AppError::Internal(format!("tx begin: {e}")))?;
                let result = sqlx::query(
                    "DELETE FROM ops WHERE room_id = $1 AND global_seq <= $2",
                )
                .bind(room)
                .bind(upper as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Internal(format!("compact ops: {e}")))?;
                let deleted = result.rows_affected();
                sqlx::query(
                    "UPDATE rooms SET compacted_below = GREATEST(compacted_below, $2)
                     WHERE id = $1",
                )
                .bind(room)
                .bind(upper as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Internal(format!("compact rooms: {e}")))?;
                // Drop snapshots strictly below the most recent at-or-equal
                // snapshot, so we keep one anchor.
                sqlx::query(
                    "DELETE FROM snapshots WHERE room_id = $1 AND at_seq < (
                        SELECT COALESCE(MAX(at_seq), 0) FROM snapshots
                        WHERE room_id = $1 AND at_seq <= $2
                     )",
                )
                .bind(room)
                .bind(upper as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Internal(format!("compact snapshots: {e}")))?;
                tx.commit()
                    .await
                    .map_err(|e| AppError::Internal(format!("tx commit: {e}")))?;
                Ok(deleted)
            }
        }
    }

    pub async fn compacted_below(&self, room: &RoomId) -> Result<GlobalSeq> {
        match &self.inner {
            OpStoreInner::InMemory(s) => Ok(s
                .lock()
                .await
                .rooms
                .get(room)
                .map_or(0, |r| r.compacted_below)),
            OpStoreInner::Postgres(pool) => {
                let row: Option<(i64,)> =
                    sqlx::query_as("SELECT compacted_below FROM rooms WHERE id = $1")
                        .bind(room)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| AppError::Internal(format!("compacted_below: {e}")))?;
                Ok(row.map_or(0, |(n,)| n as GlobalSeq))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// In-memory backend
// ---------------------------------------------------------------------------

#[derive(Default)]
struct InMemoryStorage {
    rooms: HashMap<RoomId, InMemoryRoom>,
}

#[derive(Default)]
struct InMemoryRoom {
    head: GlobalSeq,
    ops: BTreeMap<GlobalSeq, Op>,
    snapshots: BTreeMap<GlobalSeq, Snapshot>,
    snapshot_seq: GlobalSeq,
    compacted_below: GlobalSeq,
    acks: HashMap<PeerId, GlobalSeq>,
}

