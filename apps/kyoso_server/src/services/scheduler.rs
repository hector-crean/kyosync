//! Background workers for snapshotting and log compaction.
//!
//! Two periodic tasks, both spawned at startup and tied to the lifetime
//! of [`AppState`]. They iterate over all live rooms (those with at
//! least one prior snapshot/restore via the room manager) and call the
//! per-model fan-out methods.
//!
//! ## Snapshot scheduler
//!
//! Every [`SchedulerConfig::snapshot_interval`] the worker calls
//! [`Room::take_snapshot_all`] on each live room, which delegates to
//! every registered [`RoomModelHandler`]. Models that don't snapshot
//! (default trait impl, e.g. comments) are no-ops.
//!
//! ## GC scheduler
//!
//! Every [`SchedulerConfig::gc_interval`] the worker calls
//! [`Room::run_gc_all`] on each live room. Per-handler GC: graph
//! compacts ops below `min(min_ack, latest_snapshot.at_seq)`; comments
//! is currently a no-op (no compaction in v1).

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
            room.take_snapshot_all().await;
            tracing::debug!(room = %room.id, "snapshot pass complete");
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
            let dropped = room.run_gc_all().await;
            if dropped > 0 {
                tracing::info!(room = %room.id, ops_dropped = dropped, "compacted");
            }
        }
    }
}
