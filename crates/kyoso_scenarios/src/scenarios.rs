//! Scenario implementations.
//!
//! Each scenario is an `async fn` that returns a [`ScenarioReport`].
//! Server lifecycle, client construction, and per-checkpoint diff are
//! handled here; the binary just dispatches by name.

use std::time::Duration;

use bevy::prelude::*;
use kyoso_circuit::{CircuitLayer, CircuitNode, ComponentKind};
use kyoso_circuit_client::msg::{AppCommand, Pos3 as ClientPos3};
use kyoso_circuit_client::tool::{PlaceCommand, Tool, ToolCommand};

use crate::capture::capture_peer_state;
use crate::harness::{build_app, pump_apps_until, pump_for, ScenarioApp, ScenarioHarness};
use crate::report::{
    diff_against_reference, ScenarioOutcome, ScenarioReport, ScenarioStatus,
};
use crate::timeline::{drain_timeline, write_timeline_jsonl, TimelineEntry};

/// Drain each peer's `Timeline` resource and write the merged stream
/// to `target/harness-reports/scenario-<name>-timeline.jsonl`. Safe to
/// call even when capture caught no events.
fn dump_timelines(scenario: &str, peers: &mut [(&str, &mut ScenarioApp)]) {
    let streams: Vec<(String, Vec<TimelineEntry>)> = peers
        .iter_mut()
        .map(|(label, app)| ((*label).to_string(), drain_timeline(&mut app.app)))
        .collect();
    if let Err(e) = write_timeline_jsonl(scenario, &streams) {
        tracing::warn!(scenario, ?e, "failed to write timeline");
    }
}

const ROOM_PREFIX: &str = "scenario";

/// Catalog of scenarios this binary knows how to run.
pub fn scenario_names() -> &'static [&'static str] {
    &[
        "late_join",
        "late_join_after_compaction",
        "disconnect_reconnect",
        "concurrent_joins",
        "noisy_join",
        "concurrent_same_field_lww",
        "multi_room_isolation",
        "snapshot_mid_traffic",
        "plugin_add_order_independence",
    ]
}

/// Run a named scenario. Returns `Err` only for unknown names — actual
/// scenario divergence is encoded in the returned [`ScenarioReport`].
pub async fn run_scenario(name: &str) -> Result<ScenarioReport, String> {
    match name {
        "late_join" => Ok(late_join().await),
        "late_join_after_compaction" => Ok(late_join_after_compaction().await),
        "disconnect_reconnect" => Ok(disconnect_reconnect().await),
        "concurrent_joins" => Ok(concurrent_joins().await),
        "noisy_join" => Ok(noisy_join().await),
        "concurrent_same_field_lww" => Ok(concurrent_same_field_lww().await),
        "multi_room_isolation" => Ok(multi_room_isolation().await),
        "snapshot_mid_traffic" => Ok(snapshot_mid_traffic().await),
        "plugin_add_order_independence" => Ok(plugin_add_order_independence().await),
        other => Err(format!(
            "unknown scenario `{other}`. known: {:?}",
            scenario_names()
        )),
    }
}

// ---------------------------------------------------------------------------
// Helpers used by multiple scenarios.
// ---------------------------------------------------------------------------

fn wait_for_connect(apps: &mut [&mut App], label: &str) -> bool {
    pump_apps_until(apps, Duration::from_secs(3), |apps| {
        apps.iter_mut()
            .all(|app| {
                app.world()
                    .get_resource::<kyoso_sync::SyncStatus>()
                    .map(|s| s.is_connected())
                    .unwrap_or(false)
            })
    }) || {
        tracing::warn!(label, "wait_for_connect timed out");
        false
    }
}

fn count_circuit_nodes(app: &mut App) -> usize {
    app.world_mut()
        .query::<&CircuitNode>()
        .iter(app.world())
        .count()
}

fn send(app: &ScenarioApp, cmd: AppCommand) {
    app.tx.send(cmd).expect("duplex tx alive");
}

fn spawn(layer: CircuitLayer, kind: ComponentKind, x: f32, z: f32) -> AppCommand {
    AppCommand::Tool(ToolCommand::Place(PlaceCommand::SpawnAt {
        position: ClientPos3 { x, y: 0.0, z },
        kind,
        layer,
    }))
}

fn finalize(
    scenario: &str,
    outcomes: Vec<ScenarioOutcome>,
    aborted: bool,
) -> ScenarioReport {
    let total_divs: usize = outcomes.iter().map(|o| o.divergences.len()).sum();
    let status = if aborted {
        ScenarioStatus::Aborted
    } else if total_divs == 0 {
        ScenarioStatus::Converged
    } else {
        ScenarioStatus::Diverged
    };
    let summary = match status {
        ScenarioStatus::Converged => format!(
            "{scenario}: converged across {} checkpoint(s)",
            outcomes.len()
        ),
        ScenarioStatus::Diverged => format!(
            "{scenario}: {} pairwise divergence(s) across {} checkpoint(s)",
            total_divs,
            outcomes.len()
        ),
        ScenarioStatus::Aborted => format!("{scenario}: aborted"),
    };
    ScenarioReport {
        scenario: scenario.to_string(),
        status,
        checkpoints: outcomes,
        summary,
    }
}

// ---------------------------------------------------------------------------
// Scenario: late_join
//
// 1. Peer A connects, places three components on three layers.
// 2. Peer B joins late.
// 3. Both peers settle.
// 4. Capture both; assert B sees the same world A does.
//
// What it catches: snapshot + diff replay basics. Should converge.
// ---------------------------------------------------------------------------

async fn late_join() -> ScenarioReport {
    let scenario = "late_join";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;

    tokio::task::spawn_blocking(move || {
        let room = format!("{ROOM_PREFIX}-late-join");
        let mut a = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app], "A connect") {
            return finalize(scenario, vec![], true);
        }

        // Peer A places three nodes across three layers.
        send(&a, AppCommand::SetTool(Tool::Place));
        send(&a, spawn(CircuitLayer::Signal, ComponentKind::Resistor, -2.0, 0.0));
        send(&a, spawn(CircuitLayer::Power, ComponentKind::Capacitor, 2.0, 0.0));
        send(&a, spawn(CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 2.0));

        // Wait until A sees all three locally before B joins, so the
        // snapshot has them.
        if !pump_apps_until(
            &mut [&mut a.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 3,
        ) {
            return finalize(scenario, vec![], true);
        }

        // Give the duplex a beat to settle Acks before B arrives.
        pump_for(&mut [&mut a.app], Duration::from_millis(300));

        // Peer B connects late.
        let mut b = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app, &mut b.app], "B late connect") {
            return finalize(scenario, vec![], true);
        }

        // Settle and converge.
        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 3 && count_circuit_nodes(apps[1]) == 3,
        );
        // Extra settle time so per-component schema sync finishes.
        pump_for(&mut [&mut a.app, &mut b.app], Duration::from_millis(500));

        let state_a = capture_peer_state(&mut a.app, "A");
        let state_b = capture_peer_state(&mut b.app, "B");
        let peers = vec![state_a, state_b];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_late_join".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(scenario, &mut [("A", &mut a), ("B", &mut b)]);
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

// ---------------------------------------------------------------------------
// Scenario: late_join_after_compaction
//
// Same as late_join but with `take_snapshot_all` + `run_gc_all` between
// the mutations and B's arrival. Forces B's catchup to come from the
// snapshot's typed-schema state, not the op log — exercises exactly
// the hydration path we suspect of racing with diff replay.
// ---------------------------------------------------------------------------

async fn late_join_after_compaction() -> ScenarioReport {
    let scenario = "late_join_after_compaction";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;
    let rooms = harness.rooms.clone();
    let room = format!("{ROOM_PREFIX}-compaction-join");
    let handle = tokio::runtime::Handle::current();

    tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app], "A connect") {
            return finalize(scenario, vec![], true);
        }
        send(&a, AppCommand::SetTool(Tool::Place));
        send(&a, spawn(CircuitLayer::Signal, ComponentKind::Resistor, -2.0, 0.0));
        send(&a, spawn(CircuitLayer::Power, ComponentKind::Capacitor, 2.0, 0.0));
        send(&a, spawn(CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 2.0));
        if !pump_apps_until(
            &mut [&mut a.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 3,
        ) {
            return finalize(scenario, vec![], true);
        }
        // Let A ack everything before we ask the server to GC.
        pump_for(&mut [&mut a.app], Duration::from_millis(400));

        // Force snapshot + GC. Use the held tokio Handle so we can
        // run this short async block from inside spawn_blocking
        // without round-tripping back to the main runtime.
        let room_for_async = room.clone();
        let rooms_for_async = rooms.clone();
        let dropped: u64 = handle.block_on(async move {
            let room_arc = rooms_for_async
                .get_or_create(&room_for_async)
                .await
                .expect("room");
            room_arc.take_snapshot_all().await;
            room_arc.run_gc_all().await
        });
        tracing::info!(scenario, dropped, "snapshot + GC complete");

        // Pre-B checkpoint: what does A see right after compaction?
        pump_for(&mut [&mut a.app], Duration::from_millis(300));
        let state_a_pre = capture_peer_state(&mut a.app, "A");

        // Late joiner B.
        let mut b = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app, &mut b.app], "B post-compaction connect") {
            return finalize(
                scenario,
                vec![ScenarioOutcome {
                    label: "after_compaction_pre_b".to_string(),
                    peers: vec![state_a_pre.clone()],
                    divergences: vec![],
                }],
                true,
            );
        }
        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 3 && count_circuit_nodes(apps[1]) == 3,
        );
        // Extra settle time for per-schema hydration.
        pump_for(&mut [&mut a.app, &mut b.app], Duration::from_millis(800));
        let state_a = capture_peer_state(&mut a.app, "A");
        let state_b = capture_peer_state(&mut b.app, "B");

        let after_compaction_peers = vec![state_a_pre];
        let after_join_peers = vec![state_a, state_b];
        let after_join_divergences = diff_against_reference(&after_join_peers);

        let outcomes = vec![
            ScenarioOutcome {
                label: "after_compaction_pre_b".to_string(),
                peers: after_compaction_peers,
                divergences: vec![],
            },
            ScenarioOutcome {
                label: "after_b_joined".to_string(),
                peers: after_join_peers,
                divergences: after_join_divergences,
            },
        ];
        dump_timelines(scenario, &mut [("A", &mut a), ("B", &mut b)]);
        let mut report = finalize(scenario, outcomes, false);
        report.summary = format!("{} (gc dropped {dropped} ops)", report.summary);
        report
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

// ---------------------------------------------------------------------------
// Scenario: disconnect_reconnect
//
// 1. Peer A connects, places one component.
// 2. Peer A disconnects.
// 3. Peer A reconnects (same room, fresh AppPlugin instance).
// 4. Capture state on both A (post-reconnect) and a control peer C
//    that was connected throughout.
//
// What it catches: pending-op leakage across reconnect, EntityCrdtIndex
// staleness, and any spurious ops the reconnecting peer might emit.
// ---------------------------------------------------------------------------

async fn disconnect_reconnect() -> ScenarioReport {
    let scenario = "disconnect_reconnect";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;
    let room = format!("{ROOM_PREFIX}-reconnect");

    tokio::task::spawn_blocking(move || {
        let mut c = build_app(addr, &room);
        let mut a1 = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut c.app, &mut a1.app], "C+A initial connect") {
            return finalize(scenario, vec![], true);
        }
        send(&a1, AppCommand::SetTool(Tool::Place));
        send(&a1, spawn(CircuitLayer::Signal, ComponentKind::Resistor, -1.5, 1.5));
        pump_apps_until(
            &mut [&mut c.app, &mut a1.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 1 && count_circuit_nodes(apps[1]) == 1,
        );
        // Give A1 time to ack the op.
        pump_for(&mut [&mut c.app, &mut a1.app], Duration::from_millis(300));

        // Drop A1 (simulates disconnect — the App is dropped; the
        // WS transport plugin closes the socket on Drop).
        drop(a1);
        // Let the server notice the disconnect.
        pump_for(&mut [&mut c.app], Duration::from_millis(500));

        // Reconnect as A2.
        let mut a2 = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut c.app, &mut a2.app], "A reconnect") {
            return finalize(scenario, vec![], true);
        }
        pump_for(&mut [&mut c.app, &mut a2.app], Duration::from_millis(500));

        let state_c = capture_peer_state(&mut c.app, "C");
        let state_a = capture_peer_state(&mut a2.app, "A2");
        let peers = vec![state_c, state_a];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_reconnect".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(scenario, &mut [("C", &mut c), ("A2", &mut a2)]);
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

// ---------------------------------------------------------------------------
// Scenario: concurrent_joins
//
// Three peers connect roughly simultaneously and each places one
// component on a different layer. They should all converge to the
// same 3-node world. What it catches: race conditions in the Welcome
// handler when multiple peers Hello-Welcome at the same time, or
// per-peer-id reuse / collision.
// ---------------------------------------------------------------------------

async fn concurrent_joins() -> ScenarioReport {
    let scenario = "concurrent_joins";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;
    let room = format!("{ROOM_PREFIX}-concurrent");

    tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, &room);
        let mut b = build_app(addr, &room);
        let mut c = build_app(addr, &room);
        if !wait_for_connect(
            &mut [&mut a.app, &mut b.app, &mut c.app],
            "concurrent connect",
        ) {
            return finalize(scenario, vec![], true);
        }
        send(&a, AppCommand::SetTool(Tool::Place));
        send(&b, AppCommand::SetTool(Tool::Place));
        send(&c, AppCommand::SetTool(Tool::Place));
        send(&a, spawn(CircuitLayer::Signal, ComponentKind::Resistor, -2.0, -2.0));
        send(&b, spawn(CircuitLayer::Power, ComponentKind::Capacitor, 2.0, -2.0));
        send(&c, spawn(CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 2.0));

        pump_apps_until(
            &mut [&mut a.app, &mut b.app, &mut c.app],
            Duration::from_secs(8),
            |apps| apps.iter_mut().all(|app| count_circuit_nodes(app) == 3),
        );
        pump_for(
            &mut [&mut a.app, &mut b.app, &mut c.app],
            Duration::from_millis(500),
        );

        let state_a = capture_peer_state(&mut a.app, "A");
        let state_b = capture_peer_state(&mut b.app, "B");
        let state_c = capture_peer_state(&mut c.app, "C");
        let peers = vec![state_a, state_b, state_c];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_concurrent_joins".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(
            scenario,
            &mut [("A", &mut a), ("B", &mut b), ("C", &mut c)],
        );
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}


// ---------------------------------------------------------------------------
// Scenario: noisy_join
//
// 1. Peer A connects, places one component, then keeps mutating
//    (placing additional components on a rolling timer).
// 2. Peer B joins *while* A is still actively emitting ops. The diff
//    arriving in B's Welcome should be non-trivial — exercises the
//    hydration-vs-diff-replay timing without the room going quiet
//    first.
// 3. After B joins, A keeps mutating for another beat to make sure
//    post-Welcome ops keep flowing through B correctly.
// 4. Capture both peers; assert convergence.
//
// What it catches: hydration/diff-replay races, and any
// EntityCrdtIndex / SchemaDoc inconsistencies that would surface
// only when ops keep flowing during Welcome.
// ---------------------------------------------------------------------------

async fn noisy_join() -> ScenarioReport {
    let scenario = "noisy_join";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;

    tokio::task::spawn_blocking(move || {
        let room = format!("{ROOM_PREFIX}-noisy-join");
        let mut a = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app], "A connect") {
            return finalize(scenario, vec![], true);
        }
        send(&a, AppCommand::SetTool(Tool::Place));
        // Six initial components so the room is non-trivial before B.
        let initial = [
            (CircuitLayer::Signal, ComponentKind::Resistor, -2.0, 0.0),
            (CircuitLayer::Power, ComponentKind::Capacitor, 2.0, 0.0),
            (CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 2.0),
            (CircuitLayer::Signal, ComponentKind::VoltageSource, -2.0, 2.0),
            (CircuitLayer::Power, ComponentKind::Resistor, 2.0, 2.0),
            (CircuitLayer::Mechanical, ComponentKind::Ground, 0.0, -2.0),
        ];
        for (layer, kind, x, z) in initial {
            send(&a, spawn(layer, kind, x, z));
        }
        if !pump_apps_until(
            &mut [&mut a.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 6,
        ) {
            return finalize(scenario, vec![], true);
        }

        // Start B without waiting for the room to go quiet. While B
        // is hello-welcoming, keep queueing ops from A every couple
        // of frames so the diff B receives is moving even mid-handshake.
        let mut b = build_app(addr, &room);
        let mut extra_spawned = 0u32;
        let max_extras = 6;
        let connect_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut connected = false;
        while std::time::Instant::now() < connect_deadline && !connected {
            a.app.update();
            b.app.update();
            // Drip-feed mutations from A every iteration.
            if extra_spawned < max_extras {
                let (kind, layer) = match extra_spawned % 3 {
                    0 => (ComponentKind::Resistor, CircuitLayer::Signal),
                    1 => (ComponentKind::Capacitor, CircuitLayer::Power),
                    _ => (ComponentKind::Inductor, CircuitLayer::Ground),
                };
                send(
                    &a,
                    spawn(
                        layer,
                        kind,
                        -3.0 + extra_spawned as f32 * 1.0,
                        4.0 + extra_spawned as f32 * 0.5,
                    ),
                );
                extra_spawned += 1;
            }
            connected = b
                .app
                .world()
                .get_resource::<kyoso_sync::SyncStatus>()
                .map(|s| s.is_connected())
                .unwrap_or(false);
            std::thread::sleep(Duration::from_millis(10));
        }
        if !connected {
            return finalize(scenario, vec![], true);
        }

        // Post-Welcome quiet period for both peers to converge.
        let target = (6 + extra_spawned) as usize;
        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(8),
            |apps| {
                count_circuit_nodes(apps[0]) == target && count_circuit_nodes(apps[1]) == target
            },
        );
        pump_for(&mut [&mut a.app, &mut b.app], Duration::from_millis(800));

        let state_a = capture_peer_state(&mut a.app, "A");
        let state_b = capture_peer_state(&mut b.app, "B");
        let peers = vec![state_a, state_b];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_noisy_join".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(scenario, &mut [("A", &mut a), ("B", &mut b)]);
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

// ---------------------------------------------------------------------------
// Scenario: concurrent_same_field_lww
//
// Two peers connect, A places a Resistor, both wait for the node to
// appear, then *both* peers mutate the same Resistor's resistance
// concurrently. Server-seq ordering picks the winner deterministically;
// both peers converge to the same value.
// ---------------------------------------------------------------------------

async fn concurrent_same_field_lww() -> ScenarioReport {
    let scenario = "concurrent_same_field_lww";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;

    tokio::task::spawn_blocking(move || {
        let room = format!("{ROOM_PREFIX}-concurrent-lww");
        let mut a = build_app(addr, &room);
        let mut b = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app, &mut b.app], "A+B connect") {
            return finalize(scenario, vec![], true);
        }

        send(&a, AppCommand::SetTool(Tool::Place));
        send(&a, spawn(CircuitLayer::Signal, ComponentKind::Resistor, 0.0, 0.0));

        // Wait until both peers see the node + the Resistor schema.
        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(5),
            |apps| {
                apps.iter_mut().all(|app| {
                    app.world_mut()
                        .query::<&kyoso_circuit::Resistor>()
                        .iter(app.world())
                        .count()
                        == 1
                })
            },
        );

        // Both peers mutate the resistance to different values
        // simultaneously. The mutation is direct on the Bevy
        // component; SchemaSync's `Changed<Resistor>` detector emits
        // the SetNodeProperty op.
        for (app, value) in [(&mut a.app, 999.0_f32), (&mut b.app, 47.0_f32)] {
            let mut q = app.world_mut().query::<&mut kyoso_circuit::Resistor>();
            for mut r in q.iter_mut(app.world_mut()) {
                r.resistance_ohms = value;
            }
        }

        // Let both peers settle.
        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(5),
            |apps| {
                // Convergence proxy: both peers show the same value.
                let a_v = read_resistance(apps[0]);
                let b_v = read_resistance(apps[1]);
                a_v.is_some() && a_v == b_v
            },
        );
        pump_for(&mut [&mut a.app, &mut b.app], Duration::from_millis(500));

        let state_a = capture_peer_state(&mut a.app, "A");
        let state_b = capture_peer_state(&mut b.app, "B");
        let peers = vec![state_a, state_b];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_concurrent_lww".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(scenario, &mut [("A", &mut a), ("B", &mut b)]);
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

fn read_resistance(app: &mut App) -> Option<f32> {
    let mut q = app.world_mut().query::<&kyoso_circuit::Resistor>();
    q.iter(app.world()).next().map(|r| r.resistance_ohms)
}

// ---------------------------------------------------------------------------
// Scenario: multi_room_isolation
//
// Two clients in room X and two in room Y. Each room runs its own
// independent mutation workload. Final assertion: every X-peer sees
// only the X-ops, every Y-peer sees only the Y-ops.
// ---------------------------------------------------------------------------

async fn multi_room_isolation() -> ScenarioReport {
    let scenario = "multi_room_isolation";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;

    tokio::task::spawn_blocking(move || {
        let room_x = format!("{ROOM_PREFIX}-iso-x");
        let room_y = format!("{ROOM_PREFIX}-iso-y");
        let mut x1 = build_app(addr, &room_x);
        let mut x2 = build_app(addr, &room_x);
        let mut y1 = build_app(addr, &room_y);
        let mut y2 = build_app(addr, &room_y);

        if !wait_for_connect(
            &mut [&mut x1.app, &mut x2.app, &mut y1.app, &mut y2.app],
            "rooms connect",
        ) {
            return finalize(scenario, vec![], true);
        }

        send(&x1, AppCommand::SetTool(Tool::Place));
        send(&y1, AppCommand::SetTool(Tool::Place));
        // Room X: 2 components.
        send(&x1, spawn(CircuitLayer::Signal, ComponentKind::Resistor, -1.0, 0.0));
        send(&x1, spawn(CircuitLayer::Power, ComponentKind::Capacitor, 1.0, 0.0));
        // Room Y: 3 components on different layers.
        send(&y1, spawn(CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 1.0));
        send(&y1, spawn(CircuitLayer::Mechanical, ComponentKind::Ground, 0.0, -1.0));
        send(&y1, spawn(CircuitLayer::Signal, ComponentKind::VoltageSource, 1.0, 1.0));

        pump_apps_until(
            &mut [&mut x1.app, &mut x2.app, &mut y1.app, &mut y2.app],
            Duration::from_secs(6),
            |apps| {
                count_circuit_nodes(apps[0]) == 2
                    && count_circuit_nodes(apps[1]) == 2
                    && count_circuit_nodes(apps[2]) == 3
                    && count_circuit_nodes(apps[3]) == 3
            },
        );
        pump_for(
            &mut [&mut x1.app, &mut x2.app, &mut y1.app, &mut y2.app],
            Duration::from_millis(500),
        );

        let sx1 = capture_peer_state(&mut x1.app, "X1");
        let sx2 = capture_peer_state(&mut x2.app, "X2");
        let sy1 = capture_peer_state(&mut y1.app, "Y1");
        let sy2 = capture_peer_state(&mut y2.app, "Y2");

        // Per-room convergence: X1 ≡ X2, Y1 ≡ Y2.
        let mut divergences = Vec::new();
        divergences.extend(diff_against_reference(&[sx1.clone(), sx2.clone()]));
        divergences.extend(diff_against_reference(&[sy1.clone(), sy2.clone()]));

        // Cross-room isolation: assert X-room peers have the
        // *content* (kinds + layers) of the X-room ops but NOT the
        // Y-room ops, and vice versa. We can't compare CrdtIds
        // across rooms — each room mints its own peer-id namespace
        // and CrdtId collisions across rooms are expected/correct.
        let x_kinds: std::collections::BTreeSet<String> = sx1
            .nodes
            .values()
            .filter_map(|n| n.kind.clone())
            .collect();
        let y_kinds: std::collections::BTreeSet<String> = sy1
            .nodes
            .values()
            .filter_map(|n| n.kind.clone())
            .collect();
        let expected_x: std::collections::BTreeSet<String> =
            ["Resistor", "Capacitor"].iter().map(|s| s.to_string()).collect();
        let expected_y: std::collections::BTreeSet<String> =
            ["Inductor", "Ground", "VoltageSource"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        if x_kinds != expected_x {
            divergences.push(crate::report::Divergence {
                peer_a: "X1".to_string(),
                peer_b: "expected_X".to_string(),
                kind: crate::report::DivergenceKind::NodeFieldMismatch,
                detail: format!("X-room kinds = {x_kinds:?}, expected {expected_x:?}"),
            });
        }
        if y_kinds != expected_y {
            divergences.push(crate::report::Divergence {
                peer_a: "Y1".to_string(),
                peer_b: "expected_Y".to_string(),
                kind: crate::report::DivergenceKind::NodeFieldMismatch,
                detail: format!("Y-room kinds = {y_kinds:?}, expected {expected_y:?}"),
            });
        }
        // X-room kinds must not contain any Y-room kinds (and v.v.).
        let overlap: Vec<&String> = x_kinds.intersection(&y_kinds).collect();
        if !overlap.is_empty() {
            divergences.push(crate::report::Divergence {
                peer_a: "X".to_string(),
                peer_b: "Y".to_string(),
                kind: crate::report::DivergenceKind::NodeFieldMismatch,
                detail: format!(
                    "kind leak across rooms: {} kind(s) appear in both — {overlap:?}",
                    overlap.len()
                ),
            });
        }

        let outcomes = vec![ScenarioOutcome {
            label: "after_multi_room".to_string(),
            peers: vec![sx1, sx2, sy1, sy2],
            divergences,
        }];
        dump_timelines(
            scenario,
            &mut [
                ("X1", &mut x1),
                ("X2", &mut x2),
                ("Y1", &mut y1),
                ("Y2", &mut y2),
            ],
        );
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

// ---------------------------------------------------------------------------
// Scenario: snapshot_mid_traffic
//
// A is actively placing components; the harness triggers
// `take_snapshot_all` while A is still emitting ops; B joins right
// after. Asserts:
// - the snapshot's `at_seq` is well-formed (no off-by-one),
// - B's hydrated state matches A's at-snapshot moment + the diff
//   (B and A converge after settling), and
// - GC ran cleanly (some ops were compacted).
// ---------------------------------------------------------------------------

async fn snapshot_mid_traffic() -> ScenarioReport {
    let scenario = "snapshot_mid_traffic";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;
    let rooms = harness.rooms.clone();
    let room = format!("{ROOM_PREFIX}-snap-mid-traffic");
    let handle = tokio::runtime::Handle::current();

    tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app], "A connect") {
            return finalize(scenario, vec![], true);
        }
        send(&a, AppCommand::SetTool(Tool::Place));

        // Drip-feed 8 spawns with interleaved pumps. Trigger snapshot
        // halfway through (after the 4th spawn) — so the snapshot
        // captures a partial graph while subsequent ops are in flight.
        let placements = [
            (CircuitLayer::Signal, ComponentKind::Resistor, -3.0, 0.0),
            (CircuitLayer::Power, ComponentKind::Capacitor, 3.0, 0.0),
            (CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 3.0),
            (CircuitLayer::Signal, ComponentKind::VoltageSource, -3.0, 3.0),
            (CircuitLayer::Mechanical, ComponentKind::Ground, 3.0, 3.0),
            (CircuitLayer::Power, ComponentKind::Resistor, 0.0, -3.0),
            (CircuitLayer::Ground, ComponentKind::Capacitor, -3.0, -3.0),
            (CircuitLayer::Signal, ComponentKind::Inductor, 3.0, -3.0),
        ];

        let mut snapshot_dropped_ops: Option<u64> = None;
        for (i, (layer, kind, x, z)) in placements.iter().enumerate() {
            send(&a, spawn(*layer, *kind, *x, *z));
            // Pump a few frames between spawns so the op gets stamped.
            for _ in 0..6 {
                a.app.update();
                std::thread::sleep(Duration::from_millis(15));
            }
            if i == 3 {
                // Mid-traffic: ask the server to snapshot + GC. The
                // snapshot captures state at the seq A has *just* ack'd.
                let room_for_async = room.clone();
                let rooms_clone = rooms.clone();
                let dropped: u64 = handle.block_on(async move {
                    let room_arc = rooms_clone
                        .get_or_create(&room_for_async)
                        .await
                        .expect("room");
                    room_arc.take_snapshot_all().await;
                    room_arc.run_gc_all().await
                });
                snapshot_dropped_ops = Some(dropped);
            }
        }

        // Wait until A's 8 nodes are all locally visible.
        pump_apps_until(
            &mut [&mut a.app],
            Duration::from_secs(5),
            |apps| count_circuit_nodes(apps[0]) == 8,
        );
        pump_for(&mut [&mut a.app], Duration::from_millis(300));

        // Late joiner B receives the snapshot (mid-traffic state) +
        // diff (post-snapshot ops). Must converge to 8 nodes.
        let mut b = build_app(addr, &room);
        if !wait_for_connect(&mut [&mut a.app, &mut b.app], "B mid-traffic connect") {
            return finalize(scenario, vec![], true);
        }
        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(6),
            |apps| count_circuit_nodes(apps[0]) == 8 && count_circuit_nodes(apps[1]) == 8,
        );
        pump_for(&mut [&mut a.app, &mut b.app], Duration::from_millis(800));

        let state_a = capture_peer_state(&mut a.app, "A");
        let state_b = capture_peer_state(&mut b.app, "B");
        let peers = vec![state_a, state_b];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_snapshot_mid_traffic".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(scenario, &mut [("A", &mut a), ("B", &mut b)]);
        let mut report = finalize(scenario, outcomes, false);
        if let Some(d) = snapshot_dropped_ops {
            report.summary = format!("{} (mid-traffic gc dropped {d} ops)", report.summary);
        }
        report
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

// ---------------------------------------------------------------------------
// Scenario: plugin_add_order_independence
//
// Build two apps with the same logical plugin set but in different
// add-orders, run the same workload through both. Convergence proves
// the kyoso plugin stack doesn't quietly depend on a specific
// initialization order.
// ---------------------------------------------------------------------------

async fn plugin_add_order_independence() -> ScenarioReport {
    let scenario = "plugin_add_order_independence";
    let harness = ScenarioHarness::spawn().await;
    let addr = harness.addr;

    tokio::task::spawn_blocking(move || {
        let room = format!("{ROOM_PREFIX}-plugin-order");
        // App A: canonical order (the order `build_app` uses).
        let mut a = build_app(addr, &room);
        // App B: reverse base-plugin order, same `AppPlugin`. Reused
        // helper inserts the same plugins but flips the Bevy stdlib
        // prefix order so any system-registration coupling on
        // first-add-wins / last-add-wins surfaces here.
        let mut b = build_app_reverse_order(addr, &room);

        if !wait_for_connect(&mut [&mut a.app, &mut b.app], "plugin-order connect") {
            return finalize(scenario, vec![], true);
        }
        send(&a, AppCommand::SetTool(Tool::Place));
        send(&a, spawn(CircuitLayer::Signal, ComponentKind::Resistor, -2.0, 0.0));
        send(&a, spawn(CircuitLayer::Power, ComponentKind::Capacitor, 2.0, 0.0));
        send(&a, spawn(CircuitLayer::Ground, ComponentKind::Inductor, 0.0, 2.0));

        pump_apps_until(
            &mut [&mut a.app, &mut b.app],
            Duration::from_secs(6),
            |apps| count_circuit_nodes(apps[0]) == 3 && count_circuit_nodes(apps[1]) == 3,
        );
        pump_for(&mut [&mut a.app, &mut b.app], Duration::from_millis(600));

        let state_a = capture_peer_state(&mut a.app, "A_canonical");
        let state_b = capture_peer_state(&mut b.app, "B_reversed");
        let peers = vec![state_a, state_b];
        let divergences = diff_against_reference(&peers);
        let outcomes = vec![ScenarioOutcome {
            label: "after_plugin_order".to_string(),
            peers,
            divergences,
        }];
        dump_timelines(scenario, &mut [("A", &mut a), ("B", &mut b)]);
        finalize(scenario, outcomes, false)
    })
    .await
    .unwrap_or_else(|e| {
        let mut report = finalize(scenario, vec![], true);
        report.summary = format!("{scenario}: worker panic — {e}");
        report
    })
}

/// Variant of [`build_app`] that adds the Bevy stdlib plugins in the
/// reverse order. Used by the plugin-add-order scenario to surface
/// any coupling on first-add wins / last-add wins behaviour.
fn build_app_reverse_order(
    addr: std::net::SocketAddr,
    room: &str,
) -> ScenarioApp {
    use kyoso_circuit_client::msg::{AppCommand, AppEvent, create_duplex_plugin};
    use kyoso_circuit_client::AppPlugin;
    let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();
    let mut app = App::new();
    // Reverse order vs `build_app`: input → state → asset → minimal
    // → duplex → AppPlugin → TimelinePlugin.
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(bevy::state::app::StatesPlugin);
    app.add_plugins(bevy::asset::AssetPlugin::default());
    app.add_plugins(bevy::MinimalPlugins);
    app.add_plugins(duplex);
    app.add_plugins(AppPlugin {
        server_url: format!("ws://{addr}/ws"),
        room: room.to_string(),
    });
    app.add_plugins(crate::timeline::TimelinePlugin);
    ScenarioApp {
        app,
        tx: ext_tx,
        rx: ext_rx,
    }
}
