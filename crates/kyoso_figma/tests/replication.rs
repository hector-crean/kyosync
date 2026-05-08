//! End-to-end replication tests for the kyoso_figma node types.
//!
//! Two real `kyoso_server`-backed apps using `KyosoFigmaPlugin`. For
//! each node type (Frame, Rectangle, Text), App A spawns an entity
//! with non-default values and App B converges. Validates the full
//! `derive(SchemaSync)` → CRDT → wire → schema-apply → write_back path
//! against a real WebSocket transport.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use kyoso_figma::{
    Frame, KyosoFigmaPlugin, LayoutMode, Paint, Rectangle, Size, Text, TypeStyle,
};
use kyoso_server::{AppState, app};
use kyoso_sync::SyncStatus;
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

fn build_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins(KyosoFigmaPlugin {
        server_url: format!("ws://{server}/ws"),
        room: room.to_string(),
    });
    app
}

fn pump_until(
    apps: &mut [&mut App],
    timeout: Duration,
    label: &str,
    mut pred: impl FnMut(&mut [&mut App]) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        for app in apps.iter_mut() {
            app.update();
        }
        if pred(apps) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for: {label}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sync_status(app: &mut App) -> SyncStatus {
    *app.world()
        .get_resource::<SyncStatus>()
        .expect("SyncStatus resource present")
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frame_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, "frame-room");
        let mut b = build_app(addr, "frame-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        // App A spawns a Frame with non-default fields.
        a.world_mut().spawn((
            kyoso_figma::FigmaNode,
            Frame {
                name: "header".into(),
                clips_content: true,
                layout_mode: LayoutMode::Horizontal,
                fills: vec![Paint::Solid {
                    color: [0.9, 0.2, 0.1, 1.0],
                }],
                strokes: vec![],
                stroke_weight: 0.0,
            },
            Size {
                width: 800.0,
                height: 60.0,
            },
            Transform::default(),
        ));

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees the Frame",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Frame>();
                q.iter(apps[1].world()).any(|f| {
                    f.name == "header"
                        && f.clips_content
                        && f.layout_mode == LayoutMode::Horizontal
                        && matches!(
                            f.fills.first(),
                            Some(Paint::Solid { color }) if (color[0] - 0.9).abs() < 0.001
                        )
                })
            },
        );
    });
    join.await.expect("worker panic");
}

// ---------------------------------------------------------------------------
// Rectangle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rectangle_replicates_end_to_end() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, "rect-room");
        let mut b = build_app(addr, "rect-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        a.world_mut().spawn((
            kyoso_figma::FigmaNode,
            Rectangle {
                corner_radius: 12.0,
                fills: vec![Paint::Solid {
                    color: [0.0, 0.5, 1.0, 1.0],
                }],
                strokes: vec![],
                stroke_weight: 2.0,
            },
            Size {
                width: 200.0,
                height: 100.0,
            },
            Transform::default(),
        ));

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees the Rectangle",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Rectangle>();
                q.iter(apps[1].world()).any(|r| {
                    (r.corner_radius - 12.0).abs() < 0.001
                        && (r.stroke_weight - 2.0).abs() < 0.001
                })
            },
        );
    });
    join.await.expect("worker panic");
}

// ---------------------------------------------------------------------------
// Text (incl. nested TypeStyle)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn text_replicates_end_to_end_with_nested_style() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, "text-room");
        let mut b = build_app(addr, "text-room");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(2),
            "welcome",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        a.world_mut().spawn((
            kyoso_figma::FigmaNode,
            Text {
                content: "hello".into(),
                style: TypeStyle {
                    font_family: "Helvetica".into(),
                    font_size: 24.0,
                    font_weight: 700,
                    line_height: 1.2,
                },
                fills: vec![],
            },
            Transform::default(),
        ));

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(3),
            "B sees the Text + nested TypeStyle fields",
            |apps| {
                let mut q = apps[1].world_mut().query::<&Text>();
                q.iter(apps[1].world()).any(|t| {
                    t.content == "hello"
                        && t.style.font_family == "Helvetica"
                        && (t.style.font_size - 24.0).abs() < 0.001
                        && t.style.font_weight == 700
                })
            },
        );
    });
    join.await.expect("worker panic");
}
