//! Two-client convergence test for the 3D circuit client.
//!
//! Spins up `kyoso_server` and two headless `AppPlugin` instances pointed
//! at the same room. Drives client A through the duplex bus to:
//!   1. Place a Resistor on the Signal layer and a Capacitor on the
//!      Power layer (validates per-component schema sync AND `OnLayer`
//!      schema sync — including the cross-layer assignment).
//!   2. Connect them with a typed `Wire` edge (validates per-edge
//!      category dispatch).
//!   3. Place a third component on the Ground layer and connect it
//!      with a `SameNet` edge (validates a second category and a third
//!      layer).
//!
//! Then asserts on client B's ECS state:
//!   - All three components present with their kind-specific schemas.
//!   - Each carries an `OnLayer` matching peer A's layer assignment.
//!   - One `CircuitEdge` carrying `WireMarker` + one carrying
//!     `SameNetMarker`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use kyoso_circuit::{
    Capacitor, CircuitEdge, CircuitEdgeKind, CircuitLayer, CircuitNode, ComponentKind, OnLayer,
    Resistor, SameNetMarker, WireMarker,
};
use kyoso_circuit_client::msg::{
    AppCommand, AppEvent, GraphCommandExt, Pos3, create_duplex_plugin,
};
use kyoso_circuit_client::tool::{PlaceCommand, Tool, ToolCommand};
use kyoso_circuit_client::AppPlugin;
use kyoso_graph_sync::EntityCrdtIndex;
use kyoso_server::{AppState, app};
use tokio::net::TcpListener;

async fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = AppState::in_memory();
    let router = app(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// Build a headless circuit-client App. Mirrors `kyoso_client`'s
/// `build_app` helper — adds just enough plugins (asset / states /
/// input) for the AppPlugin systems that wrap their `Assets<...>` in
/// `Option` so they no-op headless.
fn build_app(
    addr: SocketAddr,
    room: &str,
) -> (
    App,
    crossbeam_channel::Sender<AppCommand>,
    crossbeam_channel::Receiver<AppEvent>,
) {
    let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();
    let mut app = App::new();
    app.add_plugins((
        bevy::MinimalPlugins,
        bevy::asset::AssetPlugin::default(),
        bevy::state::app::StatesPlugin,
        bevy::input::InputPlugin,
        duplex,
        AppPlugin {
            server_url: format!("ws://{addr}/ws"),
            room: room.to_string(),
        },
    ));
    (app, ext_tx, ext_rx)
}

/// Pump both apps in lockstep until `pred` returns true on either, with
/// a hard deadline. Useful when the predicate observes either app's
/// state.
fn pump_pair_until(
    app_a: &mut App,
    app_b: &mut App,
    timeout: Duration,
    label: &str,
    mut pred: impl FnMut(&mut App, &mut App) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        app_a.update();
        app_b.update();
        if pred(app_a, app_b) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for: {label}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_converge_on_typed_circuit_graph() {
    let addr = spawn_server().await;

    let join = tokio::task::spawn_blocking(move || {
        let (mut app_a, tx_a, _rx_a) = build_app(addr, "circuit-converge");
        let (mut app_b, _tx_b, _rx_b) = build_app(addr, "circuit-converge");

        // Bring the welcome handshake through on both apps.
        for _ in 0..30 {
            app_a.update();
            app_b.update();
            std::thread::sleep(Duration::from_millis(20));
        }

        // ── Phase 1: place a resistor on the Signal layer and a
        // capacitor on the Power layer (different layer to also
        // exercise the OnLayer schema-sync path).
        tx_a.send(AppCommand::SetTool(Tool::Place))
            .expect("tx_a alive");
        tx_a.send(AppCommand::Tool(ToolCommand::Place(
            PlaceCommand::SpawnAt {
                position: Pos3 {
                    x: -2.5,
                    y: 0.0,
                    z: 0.0,
                },
                kind: ComponentKind::Resistor,
                layer: CircuitLayer::Signal,
            },
        )))
        .expect("tx_a alive");
        tx_a.send(AppCommand::Tool(ToolCommand::Place(
            PlaceCommand::SpawnAt {
                position: Pos3 {
                    x: 2.5,
                    y: 0.0,
                    z: 0.0,
                },
                kind: ComponentKind::Capacitor,
                layer: CircuitLayer::Power,
            },
        )))
        .expect("tx_a alive");

        // Wait until both peers see two CircuitNodes AND both the
        // kind-specific schemas (Resistor / Capacitor) AND the layer
        // assignments (OnLayer) have replicated to app B. Each of
        // those is a separate schema-sync path; failing any one
        // means the corresponding `SchemaSyncedNodeComponentPlugin`
        // path is broken.
        pump_pair_until(
            &mut app_a,
            &mut app_b,
            Duration::from_secs(8),
            "both peers see Resistor + Capacitor with the right OnLayer",
            |a, b| {
                let a_nodes = a
                    .world_mut()
                    .query::<&CircuitNode>()
                    .iter(a.world())
                    .count();
                let b_nodes = b
                    .world_mut()
                    .query::<&CircuitNode>()
                    .iter(b.world())
                    .count();
                let b_resistor_signal = b
                    .world_mut()
                    .query::<(&Resistor, &OnLayer)>()
                    .iter(b.world())
                    .filter(|(_, l)| l.layer() == Some(CircuitLayer::Signal))
                    .count();
                let b_capacitor_power = b
                    .world_mut()
                    .query::<(&Capacitor, &OnLayer)>()
                    .iter(b.world())
                    .filter(|(_, l)| l.layer() == Some(CircuitLayer::Power))
                    .count();
                a_nodes == 2
                    && b_nodes == 2
                    && b_resistor_signal == 1
                    && b_capacitor_power == 1
            },
        );

        // ── Phase 2: connect the two with a Wire edge from app A. ────
        // Look up CrdtIds via app A's index. Need to read it
        // immutably; the index is keyed by Bevy entity id.
        let (resistor_id, capacitor_id) = {
            let world = app_a.world_mut();
            let mut node_q = world.query::<(Entity, &Transform, &CircuitNode)>();
            let mut a_id = None;
            let mut b_id = None;
            let nodes: Vec<(Entity, f32)> = node_q
                .iter(world)
                .map(|(e, t, _)| (e, t.translation.x))
                .collect();
            let index = world.resource::<EntityCrdtIndex>();
            for (entity, x) in nodes {
                let id = *index.node_of_entity.get(&entity).expect("node has id");
                if x < 0.0 {
                    a_id = Some(id);
                } else {
                    b_id = Some(id);
                }
            }
            (a_id.expect("resistor id"), b_id.expect("capacitor id"))
        };

        tx_a.send(AppCommand::SetWireKind(CircuitEdgeKind::Wire))
            .expect("tx_a alive");
        tx_a.send(AppCommand::Graph(GraphCommandExt::Connect {
            from: resistor_id,
            to: capacitor_id,
            kind: CircuitEdgeKind::Wire,
        }))
        .expect("tx_a alive");

        pump_pair_until(
            &mut app_a,
            &mut app_b,
            Duration::from_secs(6),
            "app B receives the wire edge with WireMarker",
            |_a, b| {
                let with_marker = b
                    .world_mut()
                    .query_filtered::<&CircuitEdge, With<WireMarker>>()
                    .iter(b.world())
                    .count();
                with_marker == 1
            },
        );

        // ── Phase 3: also issue a SameNet edge to verify per-category
        // dispatch. Place a third component (Inductor) on the Ground
        // layer and same-net it to the resistor.
        tx_a.send(AppCommand::Tool(ToolCommand::Place(
            PlaceCommand::SpawnAt {
                position: Pos3 {
                    x: 0.0,
                    y: 0.0,
                    z: 2.5,
                },
                kind: ComponentKind::Inductor,
                layer: CircuitLayer::Ground,
            },
        )))
        .expect("tx_a alive");

        pump_pair_until(
            &mut app_a,
            &mut app_b,
            Duration::from_secs(6),
            "third node arrives on both peers",
            |a, b| {
                let a_count = a
                    .world_mut()
                    .query::<&CircuitNode>()
                    .iter(a.world())
                    .count();
                let b_count = b
                    .world_mut()
                    .query::<&CircuitNode>()
                    .iter(b.world())
                    .count();
                a_count == 3 && b_count == 3
            },
        );

        let inductor_id = {
            let world = app_a.world_mut();
            let mut node_q = world.query::<(Entity, &Transform, &CircuitNode)>();
            // The inductor was placed at z=2.5; the resistor and
            // capacitor at z=0. Use z to disambiguate.
            let entity = node_q
                .iter(world)
                .find(|(_, t, _)| t.translation.z > 1.0)
                .map(|(e, _, _)| e)
                .expect("inductor entity");
            let index = world.resource::<EntityCrdtIndex>();
            *index.node_of_entity.get(&entity).expect("inductor id")
        };

        tx_a.send(AppCommand::Graph(GraphCommandExt::Connect {
            from: resistor_id,
            to: inductor_id,
            kind: CircuitEdgeKind::SameNet,
        }))
        .expect("tx_a alive");

        pump_pair_until(
            &mut app_a,
            &mut app_b,
            Duration::from_secs(6),
            "SameNet edge replicates with the right marker",
            |_a, b| {
                let same_net = b
                    .world_mut()
                    .query_filtered::<&CircuitEdge, With<SameNetMarker>>()
                    .iter(b.world())
                    .count();
                let wires = b
                    .world_mut()
                    .query_filtered::<&CircuitEdge, With<WireMarker>>()
                    .iter(b.world())
                    .count();
                // Exactly one wire and one same-net edge — neither
                // should pick up the wrong marker.
                wires == 1 && same_net == 1
            },
        );
    });
    join.await.expect("worker panic");
}
