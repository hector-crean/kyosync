//! Drive the client through the Duplex bridge end-to-end + via the
//! process-wide [`GLOBAL`] channel.
//!
//! Three test surfaces:
//!
//! 1. `spawn_via_duplex_lands_in_ecs` — push `AppCommand` through the
//!    explicit duplex sender; verify it dispatches into the right
//!    tool plugin and lands as an ECS mutation.
//! 2. `set_tool_changes_state` — exercise an app-wide command
//!    (`SetTool`) and assert the Bevy state transitions.
//! 3. `global_channel_round_trip` — same flow as (1) but via
//!    [`GLOBAL`] (mirrors how an MCP server / agent / wasm-bindgen
//!    handler reaches the bus without a runtime handle).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender};
use kyoso_client::msg::{create_duplex_plugin, AppCommand, AppEvent, Pos2, Rgb, GLOBAL};
use kyoso_figma::Frame;
use kyoso_client::tool::{CreateCommand, Tool, ToolCommand};
use kyoso_client::AppPlugin;
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

fn pump_until(
    app: &mut App,
    timeout: Duration,
    label: &str,
    mut pred: impl FnMut(&mut App) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        app.update();
        if pred(app) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for: {label}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn build_app(
    addr: SocketAddr,
    room: &str,
) -> (App, Sender<AppCommand>, Receiver<AppEvent>) {
    let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();
    let mut app = App::new();
    // The kyoso_client AppPlugin pulls in spawn/scene systems that
    // depend on `Assets<Mesh>` (`AssetPlugin`), `State<Tool>`
    // (`StatesPlugin`), and `AppTypeRegistry` (provided through
    // `bevy::reflect`). MinimalPlugins alone is not enough — we add
    // the specific extras the test needs without pulling in the
    // windowing/render stack.
    // The kyoso_client AppPlugin pulls in systems that depend on
    // various Bevy resources. MinimalPlugins lacks them, and using
    // SpritePlugin/RenderPlugin pulls in render-stack systems
    // (`text2d::update_text2d_layout`, `picking_backend`,
    // `calculate_bounds_2d`, …) that fail when the corresponding
    // assets aren't registered. The scene/presence/select systems in
    // kyoso_client wrap their `ResMut<Assets<...>>` in `Option<...>`
    // so they no-op headless; here we register *just* the asset
    // collections those systems would touch, without the sprite
    // pipeline that requires them.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_via_duplex_lands_in_ecs() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let (mut app, ext_tx, _ext_rx) = build_app(addr, "duplex-spawn");

        // Bring the welcome handshake through.
        for _ in 0..20 {
            app.update();
            std::thread::sleep(Duration::from_millis(20));
        }

        // Switch to the Create tool first; CreateCommand is gated on
        // `Tool::Create`. Then push the spawn command. Same frame is
        // fine because the dispatch + tool handler are in the same
        // Update set.
        ext_tx
            .send(AppCommand::SetTool(Tool::Create))
            .expect("ext_tx alive");
        ext_tx
            .send(AppCommand::Tool(ToolCommand::Create(
                CreateCommand::SpawnNodeAt {
                    position: Pos2 { x: 50.0, y: 80.0 },
                    color: Rgb {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                    },
                },
            )))
            .expect("ext_tx alive");

        pump_until(
            &mut app,
            Duration::from_secs(2),
            "Create tool spawns the node",
            |app| {
                let mut q = app.world_mut().query::<(&Frame, &Transform)>();
                q.iter(app.world()).any(|(_, t)| {
                    (t.translation.x - 50.0).abs() < 0.001
                        && (t.translation.y - 80.0).abs() < 0.001
                })
            },
        );
    });
    join.await.expect("worker panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_tool_changes_state() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let (mut app, ext_tx, _ext_rx) = build_app(addr, "tool-state");
        for _ in 0..10 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }

        // Default is Select.
        let initial = *app.world().resource::<State<Tool>>().get();
        assert_eq!(initial, Tool::Select);

        ext_tx.send(AppCommand::SetTool(Tool::Connect)).unwrap();

        pump_until(
            &mut app,
            Duration::from_secs(1),
            "Tool transitions to Connect",
            |app| *app.world().resource::<State<Tool>>().get() == Tool::Connect,
        );
    });
    join.await.expect("worker panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_channel_round_trip() {
    // Wire GLOBAL up to the duplex's endpoints (as `run()` does in
    // production). Then push AppCommands via GLOBAL.send(...) — the
    // path an MCP server or wasm-bindgen handler would take.
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();
        GLOBAL.set_sender(ext_tx);
        GLOBAL.set_receiver(ext_rx);

        let mut app = App::new();
        app.add_plugins((
            bevy::MinimalPlugins,
            duplex,
            AppPlugin {
                server_url: format!("ws://{addr}/ws"),
                room: "global-room".into(),
            },
        ));
        for _ in 0..20 {
            app.update();
            std::thread::sleep(Duration::from_millis(20));
        }

        GLOBAL.send(AppCommand::SetTool(Tool::Create)).unwrap();
        GLOBAL
            .send(AppCommand::Tool(ToolCommand::Create(
                CreateCommand::SpawnNodeAt {
                    position: Pos2 { x: -10.0, y: 25.0 },
                    color: Rgb {
                        r: 0.0,
                        g: 1.0,
                        b: 0.0,
                    },
                },
            )))
            .unwrap();

        pump_until(
            &mut app,
            Duration::from_secs(2),
            "GLOBAL.send → ECS",
            |app| {
                let mut q = app.world_mut().query::<(&Frame, &Transform)>();
                q.iter(app.world()).any(|(_, t)| {
                    (t.translation.x - -10.0).abs() < 0.001
                        && (t.translation.y - 25.0).abs() < 0.001
                })
            },
        );

        // GLOBAL also exposes `try_receive` — outbound events should
        // flow back. Pump a few frames and look for at least one.
        let mut saw_event = false;
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && !saw_event {
            app.update();
            while let Ok(Some(_)) = GLOBAL.try_receive() {
                saw_event = true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            saw_event,
            "GLOBAL.try_receive should observe at least one AppEvent"
        );
    });
    join.await.expect("worker panic");
}
