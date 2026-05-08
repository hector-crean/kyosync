//! Background workers for snapshotting and log compaction.
//!
//! Two periodic tasks, both spawned at startup and tied to the lifetime
//! of [`AppState`]. They iterate over all live rooms (those with at
//! least one prior snapshot/restore via the room manager) and run their
//! respective passes.
//!
//! ## Snapshot scheduler
//!
//! Every [`SchedulerConfig::snapshot_interval`] the worker calls
//! [`Room::take_snapshot`] on each live room. Snapshots are cheap
//! (in-memory traversal of the mirror's HashMaps + a postcard encode +
//! one INSERT) so we don't bother gating on "ops since last snapshot".
//! At rest, taking duplicate snapshots is a no-op insert.
//!
//! ## GC scheduler
//!
//! Every [`SchedulerConfig::gc_interval`] the worker calls
//! [`Room::run_gc`] on each live room. The compaction threshold is
//! `min(min_ack, latest_snapshot.at_seq)` so we never drop ops that
//! either (a) some peer hasn't acked yet or (b) aren't yet recoverable
//! from a persisted snapshot.

use std::sync::Arc;
use std::time::Duration;

use crate::services::RoomManager;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub snapshot_interval: Duration,
    pub gc_interval: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            snapshot_interval: Duration::from_secs(60),
            gc_interval: Duration::from_secs(120),
        }
    }
}

/// Spawn snapshot + GC workers. Returns `tokio::task::JoinHandle`s only
/// for tests that want to assert progress; production code typically
/// drops them.
pub fn spawn(rooms: Arc<RoomManager>, cfg: SchedulerConfig) -> Vec<tokio::task::JoinHandle<()>> {
    let snap = tokio::spawn(snapshot_loop(rooms.clone(), cfg.snapshot_interval));
    let gc = tokio::spawn(gc_loop(rooms, cfg.gc_interval));
    vec![snap, gc]
}

async fn snapshot_loop(rooms: Arc<RoomManager>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate initial tick — server just started, mirror is
    // already at head, snapshot would be redundant.
    tick.tick().await;
    loop {
        tick.tick().await;
        for room in rooms.live_rooms() {
            match room.take_snapshot().await {
                Ok(snap) => {
                    tracing::debug!(room = %room.id, at_seq = snap.at_seq, "snapshot taken")
                }
                Err(e) => {
                    tracing::warn!(room = %room.id, error = %e, "snapshot failed")
                }
            }
        }
    }
}

async fn gc_loop(rooms: Arc<RoomManager>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        for room in rooms.live_rooms() {
            match room.run_gc().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(room = %room.id, ops_dropped = n, "compacted"),
                Err(e) => tracing::warn!(room = %room.id, error = %e, "compaction failed"),
            }
        }
    }
}
