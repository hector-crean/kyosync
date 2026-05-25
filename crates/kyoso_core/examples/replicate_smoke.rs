//! End-to-end smoke for kyoso_core CRDT replication.
//!
//! Boots an in-process `kyoso_server`, runs two Bevy apps both using
//! `KyosoCorePlugin`, has app A spawn a small scene (Frame containing
//! a Rectangle and a Text node), then pumps until app B converges.
//! Prints what each peer sees along the way.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p kyoso_core --example replicate_smoke
//! ```
//!
//! No external setup: the server starts on a random local port, both
//! peers connect to it, and the binary exits cleanly when convergence
//! is observed (or after a timeout).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use kyoso_core::{
    Frame, KyosoCorePlugin, LayoutMode, Paint, Rectangle, Size, Text, TypeStyle,
};
use kyoso_server::{app, AppState};
use kyoso_sync::SyncStatus;
use tokio::net::TcpListener;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    init_logging();

    // ---- 1. boot the server -------------------------------------------
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    println!("[server] listening on ws://{addr}/ws");
    let state = AppState::in_memory();
    let router = app(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // ---- 2. run the demo on a blocking task ---------------------------
    let join = tokio::task::spawn_blocking(move || run_demo(addr));
    if let Err(e) = join.await {
        eprintln!("worker panic: {e:?}");
        std::process::exit(1);
    }
}

fn run_demo(addr: SocketAddr) {
    let mut peer_a = build_app(addr, "demo-room");
    let mut peer_b = build_app(addr, "demo-room");

    println!("[demo] waiting for both peers to connect…");
    pump_until(
        &mut [&mut peer_a, &mut peer_b],
        Duration::from_secs(3),
        |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
    )
    .expect("connect timeout");
    println!("[demo] both peers connected.\n");

    // ---- 3. peer A spawns a small scene -------------------------------
    println!("[A] spawning Frame name='Header' clips_content=true layout=Horizontal");
    println!("[A]   spawning Rectangle corner_radius=12 fill=blue");
    println!("[A]   spawning Text content='kyoso live' family=Helvetica size=18\n");

    let _entity_a = peer_a.world_mut().spawn((
        kyoso_core::SceneNode,
        Frame {
            name: "Header".into(),
            clips_content: true,
            layout_mode: LayoutMode::Horizontal,
            fills: vec![Paint::Solid {
                color: [0.95, 0.95, 0.97, 1.0],
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

    let _rect = peer_a.world_mut().spawn((
        kyoso_core::SceneNode,
        Rectangle {
            corner_radius: 12.0,
            fills: vec![Paint::Solid {
                color: [0.0, 0.5, 1.0, 1.0],
            }],
            strokes: vec![],
            stroke_weight: 0.0,
        },
        Size {
            width: 200.0,
            height: 36.0,
        },
        Transform::default(),
    ));

    let _text = peer_a.world_mut().spawn((
        kyoso_core::SceneNode,
        Text {
            content: "kyoso live".into(),
            style: TypeStyle {
                font_family: "Helvetica".into(),
                font_size: 18.0,
                font_weight: 600,
                line_height: 1.2,
            },
            fills: vec![],
        },
        Transform::default(),
    ));

    // ---- 4. pump until B sees all three -------------------------------
    println!("[demo] pumping until peer B converges…");
    pump_until(
        &mut [&mut peer_a, &mut peer_b],
        Duration::from_secs(5),
        |apps| {
            let world = apps[1].world_mut();
            let frame_seen = world.query::<&Frame>().iter(world).any(|f| f.name == "Header");
            let rect_seen = world
                .query::<&Rectangle>()
                .iter(world)
                .any(|r| (r.corner_radius - 12.0).abs() < 0.001);
            let text_seen = world
                .query::<&Text>()
                .iter(world)
                .any(|t| t.content == "kyoso live");
            frame_seen && rect_seen && text_seen
        },
    )
    .expect("B did not converge within timeout");

    println!("\n[B] world state:");
    print_world_state(&mut peer_b);

    println!("\n[demo] ✓ replication confirmed end-to-end via real WebSocket transport.");
}

fn build_app(server: SocketAddr, room: &str) -> App {
    let mut app = App::new();
    app.add_plugins(KyosoCorePlugin {
        server_url: format!("ws://{server}/ws"),
        room: room.to_string(),
    });
    app
}

fn pump_until(
    apps: &mut [&mut App],
    timeout: Duration,
    mut pred: impl FnMut(&mut [&mut App]) -> bool,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    loop {
        for app in apps.iter_mut() {
            app.update();
        }
        if pred(apps) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sync_status(app: &mut App) -> SyncStatus {
    *app.world().get_resource::<SyncStatus>().unwrap()
}

fn print_world_state(app: &mut App) {
    let world = app.world_mut();

    let mut q_frame = world.query::<&Frame>();
    for f in q_frame.iter(world) {
        println!(
            "  Frame {{ name: {:?}, clips_content: {}, layout_mode: {:?}, fills: {} paint(s) }}",
            f.name,
            f.clips_content,
            f.layout_mode,
            f.fills.len(),
        );
    }
    let mut q_rect = world.query::<&Rectangle>();
    for r in q_rect.iter(world) {
        println!(
            "  Rectangle {{ corner_radius: {}, fills: {} paint(s), stroke_weight: {} }}",
            r.corner_radius,
            r.fills.len(),
            r.stroke_weight,
        );
    }
    let mut q_text = world.query::<&Text>();
    for t in q_text.iter(world) {
        println!(
            "  Text {{ content: {:?}, style: {{ family: {:?}, size: {}, weight: {} }} }}",
            t.content, t.style.font_family, t.style.font_size, t.style.font_weight,
        );
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Default: kyoso_* at info, everything else at warn.
        EnvFilter::new("warn,kyoso_core=info,kyoso_sync=info")
    });
    fmt().with_env_filter(filter).with_target(false).init();
}
