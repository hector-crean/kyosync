//! End-to-end: spin up the real axum server, connect two `tokio-tungstenite`
//! clients, and verify ops submitted by one peer arrive at the other with
//! a server-assigned `GlobalSeq`.
//!
//! These tests speak the multi-model envelope protocol directly. Helper
//! functions wrap/unwrap envelopes for the graph model so each test
//! reads at the level of "send a Submit, expect an Apply" rather than
//! "encode an Op<OpKind> into bytes, build an Envelope::Submit, …".

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kyoso_crdt::{
    CrdtId, EnvelopeClientMsg, EnvelopeServerMsg, GlobalSeq, ModelGreeting, PeerId, Tier,
};
use kyoso_graph_crdt::{OpKind, graph_model};
use kyoso_server::model::Snapshot;
use kyoso_server::{AppState, OpStore, RoomManager, app};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

type Op = kyoso_crdt::Op<OpKind>;
type Diff = kyoso_crdt::Diff<OpKind>;

async fn spawn_server() -> SocketAddr {
    spawn_server_with(AppState::in_memory()).await
}

async fn spawn_server_with(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

async fn connect(
    addr: SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/ws");
    let (ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws
}

async fn send_envelope<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    msg: EnvelopeClientMsg,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let bytes = msg.encode().unwrap();
    ws.send(WsMessage::Binary(bytes.into())).await.unwrap();
}

async fn next_envelope<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
) -> EnvelopeServerMsg
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("frame timeout")
            .expect("stream ended")
            .expect("ws error");
        if let WsMessage::Binary(bytes) = frame {
            return EnvelopeServerMsg::decode(&bytes).expect("decode");
        }
        // Ignore Ping/Pong/Close that aren't the data frame we want.
    }
}

/// Send `Hello { room, models: [(graph, since)] }`.
async fn hello_graph<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    room: &str,
    since: GlobalSeq,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_envelope(
        ws,
        EnvelopeClientMsg::Hello {
            room: room.to_string(),
            tier: Tier::ReadWrite,
            models: vec![(graph_model(), since)],
        },
    )
    .await;
}

/// Wrap and send an `Op` for the graph model.
async fn submit_graph_op<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    op: &Op,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let payload = postcard::to_allocvec(op).unwrap();
    send_envelope(
        ws,
        EnvelopeClientMsg::Submit {
            model: graph_model(),
            payload,
        },
    )
    .await;
}

/// Send `Ping { model: graph, applied_seq }`.
async fn ping_graph<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    applied_seq: GlobalSeq,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_envelope(
        ws,
        EnvelopeClientMsg::Ping {
            model: graph_model(),
            applied_seq,
        },
    )
    .await;
}

/// Decoded form of a graph welcome — the per-model snapshot + diff
/// extracted from the envelope's [`ModelGreeting`].
struct GraphWelcome {
    peer: PeerId,
    snapshot: Option<Snapshot>,
    diff: Diff,
}

fn decode_graph_welcome(envelope: EnvelopeServerMsg) -> GraphWelcome {
    let EnvelopeServerMsg::Welcome { peer, models, .. } = envelope else {
        panic!("expected Welcome, got {envelope:?}");
    };
    let greeting = find_graph_greeting(&models);
    let snapshot = greeting
        .snapshot_payload
        .as_deref()
        .map(|b| postcard::from_bytes::<Snapshot>(b).expect("decode snapshot"));
    let diff: Diff = postcard::from_bytes(&greeting.diff_payload).expect("decode diff");
    GraphWelcome {
        peer,
        snapshot,
        diff,
    }
}

fn find_graph_greeting(models: &[ModelGreeting]) -> &ModelGreeting {
    let graph = graph_model();
    models
        .iter()
        .find(|g| g.model == graph)
        .expect("Welcome must include graph greeting")
}

fn decode_graph_apply(envelope: EnvelopeServerMsg) -> Op {
    let EnvelopeServerMsg::Apply { model, payload } = envelope else {
        panic!("expected Apply, got {envelope:?}");
    };
    assert_eq!(model, graph_model());
    postcard::from_bytes(&payload).expect("decode op")
}

#[tokio::test]
async fn two_clients_see_each_others_ops() {
    let addr = spawn_server().await;

    let mut alice = connect(addr).await;
    let mut bob = connect(addr).await;

    // Both join the same room from a fresh state.
    hello_graph(&mut alice, "demo", 0).await;
    hello_graph(&mut bob, "demo", 0).await;

    let alice_welcome = decode_graph_welcome(next_envelope(&mut alice).await);
    let bob_welcome = decode_graph_welcome(next_envelope(&mut bob).await);

    assert!(
        alice_welcome.snapshot.is_none() && bob_welcome.snapshot.is_none(),
        "no snapshot in fresh room"
    );
    assert!(alice_welcome.diff.is_empty(), "fresh room should have no ops");
    assert!(bob_welcome.diff.is_empty());
    assert_ne!(
        alice_welcome.peer, bob_welcome.peer,
        "server must mint distinct peer ids"
    );

    // Alice submits an AddNode using her assigned peer id.
    let alice_op = Op::new(CrdtId::new(alice_welcome.peer, 0), OpKind::AddNode);
    submit_graph_op(&mut alice, &alice_op).await;

    // Both Alice and Bob should receive the op stamped with seq=1.
    let alice_apply = decode_graph_apply(next_envelope(&mut alice).await);
    let bob_apply = decode_graph_apply(next_envelope(&mut bob).await);
    assert_eq!(alice_apply.id, alice_op.id);
    assert_eq!(alice_apply.seq, Some(1));
    assert_eq!(bob_apply.id, alice_op.id);
    assert_eq!(bob_apply.seq, Some(1));

    // Bob now submits, observing seq=2.
    let bob_op = Op::new(CrdtId::new(bob_welcome.peer, 0), OpKind::AddNode);
    submit_graph_op(&mut bob, &bob_op).await;

    let alice_apply2 = decode_graph_apply(next_envelope(&mut alice).await);
    let bob_apply2 = decode_graph_apply(next_envelope(&mut bob).await);
    assert_eq!(alice_apply2.id, bob_op.id);
    assert_eq!(alice_apply2.seq, Some(2));
    assert_eq!(bob_apply2.id, bob_op.id);
    assert_eq!(bob_apply2.seq, Some(2));
}

#[tokio::test]
async fn late_joiner_catches_up_via_welcome() {
    let addr = spawn_server().await;

    // First client joins and submits two ops.
    let mut early = connect(addr).await;
    hello_graph(&mut early, "late", 0).await;
    let early_welcome = decode_graph_welcome(next_envelope(&mut early).await);
    let early_peer = early_welcome.peer;
    for i in 0..2 {
        submit_graph_op(
            &mut early,
            &Op::new(CrdtId::new(early_peer, i), OpKind::AddNode),
        )
        .await;
        // Drain the echoed Apply so the test stays in sync.
        let _ = next_envelope(&mut early).await;
    }

    // Second client joins late: Welcome should already include the two
    // ops in `diff`.
    let mut late_joiner = connect(addr).await;
    hello_graph(&mut late_joiner, "late", 0).await;
    let welcome = decode_graph_welcome(next_envelope(&mut late_joiner).await);
    // No periodic snapshot has run yet, so the late joiner just gets the
    // full op tail.
    assert!(welcome.snapshot.is_none(), "no snapshot expected without scheduler");
    assert_eq!(welcome.diff.from_seq, 0);
    assert_eq!(welcome.diff.to_seq, 2);
    assert_eq!(welcome.diff.ops.len(), 2);
    assert_eq!(welcome.diff.ops[0].seq, Some(1));
    assert_eq!(welcome.diff.ops[1].seq, Some(2));
}

#[tokio::test]
async fn welcome_uses_snapshot_after_periodic_take() {
    // Sharing one OpStore between the server and the test code lets us
    // pre-populate ops + take a snapshot, then have the late joiner pick
    // it up via the normal handshake.
    let store = OpStore::in_memory();
    let state = AppState::from_store(store.clone());
    let addr = spawn_server_with(state.clone()).await;

    // First client connects and submits some ops.
    let mut early = connect(addr).await;
    hello_graph(&mut early, "snapshot-room", 0).await;
    let early_peer = decode_graph_welcome(next_envelope(&mut early).await).peer;
    for i in 0..3 {
        submit_graph_op(
            &mut early,
            &Op::new(CrdtId::new(early_peer, i), OpKind::AddNode),
        )
        .await;
        let _ = next_envelope(&mut early).await; // drain echoed Apply
    }

    // Force a snapshot via the room handle (would normally happen via
    // the periodic scheduler).
    let room = state
        .rooms
        .get_or_create("snapshot-room")
        .await
        .expect("room");
    // Per-handler dispatch — fans out across every model. Comments has
    // no snapshots (default no-op), graph persists into `store`.
    room.take_snapshot_all().await;
    let snap = store
        .latest_snapshot(&"snapshot-room".into())
        .await
        .expect("store ok")
        .expect("graph snapshot persisted");
    assert_eq!(snap.at_seq, 3);
    assert_eq!(snap.topology.nodes.len(), 3);

    // A late joiner with `since: 0` should now receive the snapshot
    // (cheaper than replaying all 3 ops from scratch).
    let mut late = connect(addr).await;
    hello_graph(&mut late, "snapshot-room", 0).await;
    let welcome = decode_graph_welcome(next_envelope(&mut late).await);
    let s = welcome.snapshot.expect("snapshot delivered");
    assert_eq!(s.at_seq, 3);
    // Diff is empty — the snapshot already covers everything.
    assert!(welcome.diff.is_empty());
}

#[tokio::test]
async fn ping_ack_drives_compaction_threshold() {
    // Bring up a server, submit some ops, take a snapshot, advance the
    // peer's ack via Ping, then run GC manually and verify ops below the
    // threshold are gone.
    let store = OpStore::in_memory();
    let state = AppState::from_store(store.clone());
    let addr = spawn_server_with(state.clone()).await;
    let rooms: Arc<RoomManager> = state.rooms.clone();

    let mut alice = connect(addr).await;
    hello_graph(&mut alice, "gc-room", 0).await;
    let alice_peer = decode_graph_welcome(next_envelope(&mut alice).await).peer;
    for i in 0..4 {
        submit_graph_op(
            &mut alice,
            &Op::new(CrdtId::new(alice_peer, i), OpKind::AddNode),
        )
        .await;
        let _ = next_envelope(&mut alice).await;
    }

    // Take a snapshot at seq=4 — without it, GC has nothing to compact
    // against.
    let room = rooms.get_or_create("gc-room").await.expect("room");
    room.take_snapshot_all().await;

    // Alice acks she's seen seq=4.
    ping_graph(&mut alice, 4).await;
    match next_envelope(&mut alice).await {
        EnvelopeServerMsg::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }

    // Run one GC cycle. min_ack=4, snapshot_seq=4, so upper=4. The
    // total covers every handler — comments contributes 0 (no GC),
    // graph drops 4.
    let dropped = room.run_gc_all().await;
    assert_eq!(dropped, 4, "all four graph ops should be compacted");

    // Verify the underlying store no longer has those ops below 4.
    let head = store.head(&"gc-room".into()).await.unwrap();
    assert_eq!(head, 4);
    let slice = store.slice(&"gc-room".into(), 0, head).await.unwrap();
    assert!(slice.is_empty(), "ops table is empty after compaction");
    assert_eq!(store.compacted_below(&"gc-room".into()).await.unwrap(), 4);
}

#[tokio::test]
async fn disconnect_clears_peer_ack() {
    // After a peer disconnects, their ack row must be cleared so a slow
    // ack doesn't permanently block GC.
    let store = OpStore::in_memory();
    let state = AppState::from_store(store.clone());
    let addr = spawn_server_with(state.clone()).await;

    let mut alice = connect(addr).await;
    hello_graph(&mut alice, "disc-room", 0).await;
    let _ = next_envelope(&mut alice).await;

    // Confirm an ack row exists.
    assert_eq!(
        store.min_ack(&"disc-room".into()).await.unwrap(),
        Some(0),
        "joining seeds an ack at the client's `since`"
    );

    // Drop the connection.
    drop(alice);

    // Wait briefly for the server's release_peer call to propagate.
    for _ in 0..50 {
        if store.min_ack(&"disc-room".into()).await.unwrap().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("ack row never cleared after disconnect");
}

#[tokio::test]
async fn ping_replies_pong() {
    let addr = spawn_server().await;
    let mut ws = connect(addr).await;
    hello_graph(&mut ws, "ping-test", 0).await;
    let _welcome = next_envelope(&mut ws).await;

    ping_graph(&mut ws, 0).await;
    match next_envelope(&mut ws).await {
        EnvelopeServerMsg::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}
