//! End-to-end: two Bevy apps each running the multi-model transport
//! plus both graph and comments plugins, converging on one room over
//! one WebSocket per app.
//!
//! Proves the full architectural goal: a single
//! [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) hosts
//! multiple per-model plugins
//! ([`GraphSyncPlugin`](kyoso_graph_sync::GraphSyncPlugin),
//! [`CommentsSyncPlugin`](kyoso_comments_sync::CommentsSyncPlugin)),
//! they share the peer-level [`PeerIdGen`](kyoso_sync::PeerIdGen) so
//! cross-model `CrdtId` references stay collision-free, and a comment
//! anchored to a graph node round-trips intact between two peers.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use kyoso_comments_sync::{CommentsClient, CommentsSyncPlugin};
use kyoso_crdt::CrdtId;
use kyoso_graph_sync::{ClientSyncEngine, GraphSyncPlugin};
use kyoso_server::{AppState, app};
use kyoso_sync::{SyncStatus, SyncTransportPlugin};
use tokio::net::TcpListener;

#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
struct N;
#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
struct E;

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
    app.add_plugins((
        SyncTransportPlugin::new(format!("ws://{server}/ws"), room),
        GraphSyncPlugin::<N, E>::default(),
        CommentsSyncPlugin::default(),
    ));
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

fn graph_node_count(app: &mut App) -> usize {
    app.world()
        .get_resource::<ClientSyncEngine>()
        .map_or(0, |g| g.node_count())
}

fn comment_count(app: &mut App) -> usize {
    app.world()
        .get_resource::<CommentsClient>()
        .map_or(0, |c| c.comment_count())
}

fn first_node_id(app: &mut App) -> Option<CrdtId> {
    let index = app.world().resource::<kyoso_graph_sync::EntityCrdtIndex>();
    index.entity_of_node.keys().next().copied()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_bevy_apps_converge_on_graph_and_comments() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        let mut a = build_app(addr, "multi-bevy");
        let mut b = build_app(addr, "multi-bevy");

        // Wait for both apps to connect (Welcome arrives + status flips).
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(5),
            "both apps connected",
            |apps| {
                apps.iter_mut().all(|app| sync_status(app).is_connected())
            },
        );

        // App A spawns a graph node — detection emits AddNode op,
        // outbound submits it.
        let _entity = a.world_mut().spawn(N).id();
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(5),
            "graph node visible on B",
            |apps| graph_node_count(apps[1]) == 1 && graph_node_count(apps[0]) == 1,
        );

        // Find the node's CrdtId on app A. App B should know about the
        // same id (it came from server-stamped Apply).
        let node_id_a = first_node_id(&mut a).expect("A should have the node id");
        let node_id_b = first_node_id(&mut b).expect("B should have the node id");
        assert_eq!(
            node_id_a, node_id_b,
            "both apps should agree on the graph node id"
        );

        // App B anchors a comment to the graph node. The comment's
        // CrdtId is minted from B's peer, the anchor references the
        // graph node id (cross-model reference safe because they share
        // the same PeerIdGen-backed IdGen).
        let comment_id = {
            let mut client = b
                .world_mut()
                .resource_mut::<CommentsClient>();
            client.add_comment(node_id_b, None, "looks great".into())
        };
        // Comment_id's peer should match B's peer id.
        let b_peer = b.world().resource::<CommentsClient>().peer();
        assert_eq!(comment_id.peer, b_peer, "comment minted under B's peer");

        // Anchor reference: the comment's anchor on B should equal the
        // graph node id we just minted on B.
        assert_eq!(
            b.world()
                .resource::<CommentsClient>()
                .anchor(comment_id),
            Some(node_id_b),
        );

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(5),
            "comment visible on A",
            |apps| comment_count(apps[0]) == 1 && comment_count(apps[1]) == 1,
        );

        // App A should now see the comment with the same anchor — proves
        // the cross-model CrdtId reference round-tripped intact.
        let comments_a = a.world().resource::<CommentsClient>();
        assert_eq!(comments_a.body(comment_id), Some("looks great"));
        assert_eq!(
            comments_a.anchor(comment_id),
            Some(node_id_a),
            "A's view of the comment must point at the same graph node id"
        );

        // App A edits the body. App B should converge.
        {
            let mut client = a
                .world_mut()
                .resource_mut::<CommentsClient>();
            client.edit_body(comment_id, "looks great!!".into());
        }
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(5),
            "edit visible on B",
            |apps| {
                let body_a = apps[0]
                    .world()
                    .resource::<CommentsClient>()
                    .body(comment_id)
                    .map(str::to_string);
                let body_b = apps[1]
                    .world()
                    .resource::<CommentsClient>()
                    .body(comment_id)
                    .map(str::to_string);
                body_a == Some("looks great!!".into())
                    && body_b == Some("looks great!!".into())
            },
        );
    });
    join.await.unwrap();
}

/// A graph-only client (no CommentsSyncPlugin) is unaffected by the
/// existence of comments traffic — the server still includes comments
/// greetings if we asked, but a client that doesn't subscribe never
/// sees comments envelopes. Smoke-test that a graph-only app with the
/// same room as a graph+comments app still converges on graph state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_only_app_unaffected_by_comments_traffic() {
    let addr = spawn_server().await;
    let join = tokio::task::spawn_blocking(move || {
        // Graph-only app
        let mut a = {
            let mut app = App::new();
            app.add_plugins((
                SyncTransportPlugin::new(format!("ws://{addr}/ws"), "mixed"),
                GraphSyncPlugin::<N, E>::default(),
            ));
            app
        };
        // Graph + comments app
        let mut b = build_app(addr, "mixed");

        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(5),
            "both connected",
            |apps| apps.iter_mut().all(|app| sync_status(app).is_connected()),
        );

        // B adds a graph node + a comment.
        let _entity = b.world_mut().spawn(N).id();
        pump_until(
            &mut [&mut a, &mut b],
            Duration::from_secs(5),
            "graph node on A",
            |apps| graph_node_count(apps[0]) == 1 && graph_node_count(apps[1]) == 1,
        );
        let node_id = first_node_id(&mut b).unwrap();
        b.world_mut()
            .resource_mut::<CommentsClient>()
            .add_comment(node_id, None, "from b".into());

        // Drive both for a bit. A doesn't have CommentsSyncPlugin so it
        // doesn't track comments, but it shouldn't crash either.
        for _ in 0..30 {
            a.update();
            b.update();
            std::thread::sleep(Duration::from_millis(10));
        }

        // B sees the comment; A doesn't (no CommentsClient resource).
        assert_eq!(comment_count(&mut b), 1);
        assert!(a.world().get_resource::<CommentsClient>().is_none());
        assert_eq!(graph_node_count(&mut a), 1);
    });
    join.await.unwrap();
}
