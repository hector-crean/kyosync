//! Server + headless-client orchestration primitives. Lifted from the
//! pattern in `apps/kyoso_circuit_client/tests/two_clients.rs` and
//! generalised so scenarios can spawn N peers, hold handles to drive
//! them via the duplex bus, and request graceful disconnect /
//! reconnect.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use kyoso_circuit_client::msg::{AppCommand, AppEvent, create_duplex_plugin};
use kyoso_circuit_client::AppPlugin;
use kyoso_server::{AppState, app, services::room::RoomManager};
use tokio::net::TcpListener;

/// Result of spinning up the harness server. Holds the bind address
/// (clients connect to `ws://{addr}/ws`) and a clone of the
/// `RoomManager` so scenarios can trigger snapshot / GC out of band
/// (mirroring what the scheduler does in production).
pub struct ScenarioHarness {
    pub addr: SocketAddr,
    pub rooms: Arc<RoomManager>,
}

impl ScenarioHarness {
    /// Spawn an in-process `kyoso_server` on an ephemeral port. The
    /// `tokio::spawn` returns a detached task — the server runs until
    /// the runtime is dropped (i.e. the scenario binary exits).
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState::in_memory();
        let rooms = state.rooms.clone();
        let router = app(state);
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { addr, rooms }
    }

    /// Force snapshot + GC on a given room. Equivalent to letting the
    /// scheduler tick once at maximum speed. Returns the number of ops
    /// the GC compacted (handy for assertions like "ops were actually
    /// compacted before the late joiner arrived").
    pub async fn snapshot_and_gc(&self, room: &str) -> u64 {
        let room = self.rooms.get_or_create(room).await.expect("room");
        room.take_snapshot_all().await;
        room.run_gc_all().await
    }
}

/// One scripted peer: a headless Bevy `App` plus crossbeam channels
/// for driving it through the duplex bus.
pub struct ScenarioApp {
    pub app: App,
    pub tx: crossbeam_channel::Sender<AppCommand>,
    pub rx: crossbeam_channel::Receiver<AppEvent>,
}

/// Build a headless circuit-client `App` connected to `addr` / `room`.
/// Mirrors `apps/kyoso_circuit_client/tests/two_clients.rs::build_app`
/// — just enough Bevy plugins to make `AppPlugin`'s systems happy
/// without bringing in a renderer or window.
pub fn build_app(addr: SocketAddr, room: &str) -> ScenarioApp {
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
        // Op timeline capture — every peer logs each `RemoteOpApplied`
        // event with a monotonic timestamp so the scenario can dump a
        // merged JSONL alongside its state report.
        crate::timeline::TimelinePlugin,
    ));
    ScenarioApp {
        app,
        tx: ext_tx,
        rx: ext_rx,
    }
}

/// Pump N apps in lockstep until `pred` returns true on any of them or
/// the deadline elapses. Returns `true` on success, `false` on timeout
/// — scenarios decide how to react (record divergence vs. panic).
pub fn pump_apps_until(
    apps: &mut [&mut App],
    timeout: Duration,
    mut pred: impl FnMut(&mut [&mut App]) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        for app in apps.iter_mut() {
            app.update();
        }
        if pred(apps) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pump a single app for a fixed wall-clock duration without a
/// predicate. Useful for "let the network settle" gaps between
/// scenario phases.
pub fn pump_for(apps: &mut [&mut App], duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        for app in apps.iter_mut() {
            app.update();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
