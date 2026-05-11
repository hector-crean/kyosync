//! Runnable peer that connects to a running [`kyoso_server`] and
//! prints the converged graph state every second.
//!
//! Pair with another instance — or with the existing
//! `kyoso_sync` integration tests — to see ops propagate live.
//!
//! Usage:
//!
//! ```bash
//! # Terminal 1 — start the server
//! cargo run -p kyoso_server
//!
//! # Terminal 2 — start peer "a"
//! cargo run -p kyoso_sync --example peer -- demo a
//!
//! # Terminal 3 — start peer "b"
//! cargo run -p kyoso_sync --example peer -- demo b
//! ```
//!
//! Each peer adds a new node every two seconds and prints the total
//! count it sees, so you can watch them converge.

use std::time::Duration;

use bevy::MinimalPlugins;
use bevy::prelude::*;
use kyoso_graph_sync::{ClientSyncEngine, GraphSyncPlugin};
use kyoso_sync::SyncStatus;

#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
struct Node;
#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
struct Edge;

type SyncedGraph = ClientSyncEngine;

#[derive(Resource)]
struct Args {
    label: String,
}

#[derive(Resource, Default)]
struct LocalCounter(usize);

fn main() {
    let mut argv = std::env::args().skip(1);
    let room = argv.next().unwrap_or_else(|| "demo".into());
    let label = argv.next().unwrap_or_else(|| "peer".into());

    let url = std::env::var("KYOSO_URL").unwrap_or_else(|_| "ws://127.0.0.1:7878/ws".into());
    println!("[{label}] connecting to {url} room={room}");

    // Build the app, then drive it with our own loop. The ScheduleRunnerPlugin
    // version triggers an obscure spin-loop on Bevy main when combined with the
    // sync plugin's runtime-thread workload; manual `update()` calls sidestep it.
    let mut app = App::new();
    app.insert_resource(Args {
        label: label.clone(),
    })
    .init_resource::<LocalCounter>()
    .add_plugins(MinimalPlugins)
    .add_plugins(GraphSyncPlugin::<Node, Edge>::new(url, room))
    .add_systems(
        Update,
        (announce_when_connected, mint_periodically, print_state),
    );

    let tick = Duration::from_millis(16);
    loop {
        let start = std::time::Instant::now();
        app.update();
        let elapsed = start.elapsed();
        if elapsed < tick {
            std::thread::sleep(tick - elapsed);
        }
    }
}

fn announce_when_connected(
    status: Res<SyncStatus>,
    args: Res<Args>,
    mut said: Local<bool>,
) {
    if !*said {
        if let SyncStatus::Connected { peer } = *status {
            println!("[{}] welcome — assigned peer={peer}", args.label);
            *said = true;
        }
    }
}

// Frame-based scheduling avoids any dependency on Bevy's Time plugin
// being wired up — at the configured 60-ish fps tick, 120 frames ≈ 2 s.
fn mint_periodically(
    mut graph: ResMut<SyncedGraph>,
    status: Res<SyncStatus>,
    mut frame: Local<u32>,
    mut counter: ResMut<LocalCounter>,
    args: Res<Args>,
) {
    *frame += 1;
    if *frame == 1 {
        println!("[{}] mint_periodically system started running", args.label);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    if !status.is_connected() {
        return;
    }
    if *frame % 120 != 0 {
        return;
    }
    let id = graph.add_node();
    counter.0 += 1;
    println!(
        "[{}] minted node {id}  (locally total: {}, frame {})",
        args.label, counter.0, *frame
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn print_state(
    graph: Res<SyncedGraph>,
    args: Res<Args>,
    counter: Res<LocalCounter>,
    mut frame: Local<u32>,
) {
    *frame += 1;
    if *frame % 60 != 0 {
        return;
    }
    println!(
        "[{}] room: nodes={} applied_seq={} (locally minted: {})",
        args.label,
        graph.node_count(),
        graph.applied_seq(),
        counter.0
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();
}
