//! Per-peer op timeline capture for scenarios.
//!
//! Each [`ScenarioApp`](crate::ScenarioApp) registers
//! [`TimelinePlugin`] which subscribes a `MessageReader` to the
//! per-peer [`kyoso_graph_sync::RemoteOpApplied`] stream and appends
//! `(monotonic_us, op_id, kind_label)` to a [`Timeline`] resource.
//!
//! When the scenario finalises, [`drain_timeline`] pulls the buffer
//! out, paired with a peer label. [`write_timeline_jsonl`] emits one
//! JSON object per applied op to
//! `target/harness-reports/scenario-<name>-timeline.jsonl`, mergeable
//! across peers by `applied_at_us`.
//!
//! Used by the agent (or a human) to localise where in a scenario a
//! divergent state emerged.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use bevy::prelude::*;
use kyoso_graph_sync::RemoteOpApplied;
use serde::{Deserialize, Serialize};

/// Per-peer ring of (timestamp, op-id, kind) triples for every
/// `RemoteOpApplied` the peer's `kyoso_graph_sync` plugin emits.
#[derive(Resource, Default)]
pub struct Timeline {
    pub entries: Vec<TimelineEntry>,
    started_at: Option<Instant>,
}

impl Timeline {
    fn start_clock(&mut self) {
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
    }
    fn since_start_us(&self) -> u128 {
        self.started_at
            .map(|t| t.elapsed().as_micros())
            .unwrap_or(0)
    }
}

/// One captured event. Kept JSON-friendly so the JSONL output is
/// directly readable without extra encoding.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// Microseconds since the app's first frame.
    pub applied_at_us: u128,
    /// Stable wire-format key: `{peer}:{seq}`.
    pub op_id: String,
    /// Op-kind discriminant from `kyoso_graph_crdt::OpKind` (no payload).
    pub kind: String,
    /// Server-assigned `GlobalSeq`, or `null` if the op hasn't been
    /// stamped yet (shouldn't happen for `RemoteOpApplied` since the
    /// engine only emits the event after `apply_remote` succeeded).
    pub seq: Option<u64>,
}

/// Bevy plugin: per-peer timeline buffer + capture system.
pub struct TimelinePlugin;

impl Plugin for TimelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Timeline>();
        app.add_systems(First, start_clock);
        app.add_systems(Update, capture_remote_ops);
    }
}

fn start_clock(mut timeline: ResMut<Timeline>) {
    if timeline.started_at.is_none() {
        timeline.start_clock();
    }
}

fn capture_remote_ops(
    mut events: MessageReader<RemoteOpApplied>,
    mut timeline: ResMut<Timeline>,
) {
    if events.is_empty() {
        return;
    }
    let applied_at_us = timeline.since_start_us();
    let entries: Vec<TimelineEntry> = events
        .read()
        .map(|ev| {
            let op = &ev.0;
            TimelineEntry {
                applied_at_us,
                op_id: format!("{}:{}", op.id.peer, op.id.seq),
                kind: kind_label(&op.kind),
                seq: op.seq,
            }
        })
        .collect();
    timeline.entries.extend(entries);
}

fn kind_label(kind: &kyoso_graph_crdt::OpKind) -> String {
    use kyoso_graph_crdt::OpKind;
    match kind {
        OpKind::AddNode => "AddNode".to_string(),
        OpKind::RemoveNode { .. } => "RemoveNode".to_string(),
        OpKind::Move { .. } => "Move".to_string(),
        OpKind::AddRefEdge { .. } => "AddRefEdge".to_string(),
        OpKind::RemoveRefEdge { .. } => "RemoveRefEdge".to_string(),
        OpKind::SetNodeProperty { .. } => "SetNodeProperty".to_string(),
        OpKind::SetRefEdgeProperty { .. } => "SetRefEdgeProperty".to_string(),
    }
}

/// Drain the timeline out of an app's world. Returns the captured
/// entries, leaving the resource empty.
pub fn drain_timeline(app: &mut App) -> Vec<TimelineEntry> {
    let mut t = app.world_mut().resource_mut::<Timeline>();
    std::mem::take(&mut t.entries)
}

/// Write one `TimelineEntry` per line as JSONL. The peer-label column
/// makes the file mergeable across peers by `applied_at_us`.
pub fn write_timeline_jsonl(
    scenario: &str,
    peer_streams: &[(String, Vec<TimelineEntry>)],
) -> std::io::Result<std::path::PathBuf> {
    let dir = Path::new("target/harness-reports");
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("scenario-{scenario}-timeline.jsonl"));
    let mut file = fs::File::create(&path)?;
    for (peer, entries) in peer_streams {
        for entry in entries {
            let payload = serde_json::json!({
                "peer": peer,
                "applied_at_us": entry.applied_at_us,
                "op_id": entry.op_id,
                "kind": entry.kind,
                "seq": entry.seq,
            });
            writeln!(file, "{}", payload)?;
        }
    }
    Ok(path)
}
