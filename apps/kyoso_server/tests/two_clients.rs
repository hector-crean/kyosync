//! End-to-end: spin up the real axum server, connect two `tokio-tungstenite`
//! clients, and verify ops submitted by one peer arrive at the other with
//! a server-assigned `GlobalSeq`.
//!
//! This is the smoke test the architecture has to keep passing as the
//! CRDT, persistence, and protocol layers evolve.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kyoso_crdt::CrdtId;
use kyoso_graph_crdt::{OpKind, Snapshot};

type Op = kyoso_crdt::Op<OpKind>;
type ClientMsg = kyoso_crdt::ClientMsg<OpKind>;
type ServerMsg = kyoso_crdt::ServerMsg<OpKind, Snapshot>;
use kyoso_server::{AppState, OpStore, RoomManager, app};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

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

async fn send_client_msg<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    msg: ClientMsg,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let bytes = msg.encode().unwrap();
    ws.send(WsMessage::Binary(bytes.into())).await.unwrap();
}

async fn next_server_msg<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
) -> ServerMsg
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
            return ServerMsg::decode(&bytes).expect("decode");
        }
        // Ignore Ping/Pong/Close that aren't the data frame we want.
    }
}

#[tokio::test]
async fn two_clients_see_each_others_ops() {
    let addr = spawn_server().await;

    let mut alice = connect(addr).await;
    let mut bob = connect(addr).await;

    // Both join the same room from a fresh state.
    send_client_msg(
        &mut alice,
        ClientMsg::Hello {
            room: "demo".into(),
            since: 0,
        },
    )
    .await;
    send_client_msg(
        &mut bob,
        ClientMsg::Hello {
            room: "demo".into(),
            since: 0,
        },
    )
    .await;

    let alice_welcome = next_server_msg(&mut alice).await;
    let bob_welcome = next_server_msg(&mut bob).await;

    let (alice_peer, bob_peer) = match (alice_welcome, bob_welcome) {
        (
            ServerMsg::Welcome {
                peer: a,
                snapshot: sa,
                diff: da,
                ..
            },
            ServerMsg::Welcome {
                peer: b,
                snapshot: sb,
                diff: db,
                ..
            },
        ) => {
            assert!(sa.is_none() && sb.is_none(), "no snapshot in fresh room");
            assert!(da.is_empty(), "fresh room should have no ops");
            assert!(db.is_empty());
            assert_ne!(a, b, "server must mint distinct peer ids");
            (a, b)
        }
        other => panic!("expected Welcome from both, got {other:?}"),
    };

    // Alice submits an AddNode using her assigned peer id.
    let alice_op = Op::new(CrdtId::new(alice_peer, 0), OpKind::AddNode);
    send_client_msg(&mut alice, ClientMsg::Submit(alice_op.clone())).await;

    // Both Alice and Bob should receive the op stamped with seq=1.
    let alice_apply = next_server_msg(&mut alice).await;
    let bob_apply = next_server_msg(&mut bob).await;
    match (alice_apply, bob_apply) {
        (ServerMsg::Apply(a), ServerMsg::Apply(b)) => {
            assert_eq!(a.id, alice_op.id);
            assert_eq!(a.seq, Some(1));
            assert_eq!(b.id, alice_op.id);
            assert_eq!(b.seq, Some(1));
        }
        other => panic!("expected Apply, got {other:?}"),
    }

    // Bob now submits, observing seq=2.
    let bob_op = Op::new(CrdtId::new(bob_peer, 0), OpKind::AddNode);
    send_client_msg(&mut bob, ClientMsg::Submit(bob_op.clone())).await;

    let alice_apply2 = next_server_msg(&mut alice).await;
    let bob_apply2 = next_server_msg(&mut bob).await;
    match (alice_apply2, bob_apply2) {
        (ServerMsg::Apply(a), ServerMsg::Apply(b)) => {
            assert_eq!(a.id, bob_op.id);
            assert_eq!(a.seq, Some(2));
            assert_eq!(b.id, bob_op.id);
            assert_eq!(b.seq, Some(2));
        }
        other => panic!("expected Apply, got {other:?}"),
    }
}

#[tokio::test]
async fn late_joiner_catches_up_via_welcome() {
    let addr = spawn_server().await;

    // First client joins and submits two ops.
    let mut early = connect(addr).await;
    send_client_msg(
        &mut early,
        ClientMsg::Hello {
            room: "late".into(),
            since: 0,
        },
    )
    .await;
    let welcome = next_server_msg(&mut early).await;
    let early_peer = match welcome {
        ServerMsg::Welcome { peer, .. } => peer,
        other => panic!("expected Welcome, got {other:?}"),
    };
    for i in 0..2 {
        send_client_msg(
            &mut early,
            ClientMsg::Submit(Op::new(CrdtId::new(early_peer, i), OpKind::AddNode)),
        )
        .await;
        // Drain the echoed Apply so the test stays in sync.
        let _ = next_server_msg(&mut early).await;
    }

    // Second client joins late: Welcome should already include the two
    // ops in `diff`.
    let mut late_joiner = connect(addr).await;
    send_client_msg(
        &mut late_joiner,
        ClientMsg::Hello {
            room: "late".into(),
            since: 0,
        },
    )
    .await;
    let welcome = next_server_msg(&mut late_joiner).await;
    match welcome {
        ServerMsg::Welcome { diff, snapshot, .. } => {
            // No periodic snapshot has run yet, so the late joiner just
            // gets the full op tail.
            assert!(snapshot.is_none(), "no snapshot expected without scheduler");
            assert_eq!(diff.from_seq, 0);
            assert_eq!(diff.to_seq, 2);
            assert_eq!(diff.ops.len(), 2);
            assert_eq!(diff.ops[0].seq, Some(1));
            assert_eq!(diff.ops[1].seq, Some(2));
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
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
    send_client_msg(
        &mut early,
        ClientMsg::Hello {
            room: "snapshot-room".into(),
            since: 0,
        },
    )
    .await;
    let early_peer = match next_server_msg(&mut early).await {
        ServerMsg::Welcome { peer, .. } => peer,
        other => panic!("expected Welcome, got {other:?}"),
    };
    for i in 0..3 {
        send_client_msg(
            &mut early,
            ClientMsg::Submit(Op::new(CrdtId::new(early_peer, i), OpKind::AddNode)),
        )
        .await;
        let _ = next_server_msg(&mut early).await; // drain echoed Apply
    }

    // Force a snapshot via the room handle (would normally happen via
    // the periodic scheduler).
    let room = state
        .rooms
        .get_or_create("snapshot-room")
        .await
        .expect("room");
    let snap = room.take_snapshot().await.expect("snapshot");
    assert_eq!(snap.at_seq, 3);
    assert_eq!(snap.nodes.len(), 3);

    // A late joiner with `since: 0` should now receive the snapshot
    // (cheaper than replaying all 3 ops from scratch).
    let mut late = connect(addr).await;
    send_client_msg(
        &mut late,
        ClientMsg::Hello {
            room: "snapshot-room".into(),
            since: 0,
        },
    )
    .await;
    match next_server_msg(&mut late).await {
        ServerMsg::Welcome {
            snapshot, diff, ..
        } => {
            let s = snapshot.expect("snapshot delivered");
            assert_eq!(s.at_seq, 3);
            // Diff is empty — the snapshot already covers everything.
            assert!(diff.is_empty());
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
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
    send_client_msg(
        &mut alice,
        ClientMsg::Hello {
            room: "gc-room".into(),
            since: 0,
        },
    )
    .await;
    let alice_peer = match next_server_msg(&mut alice).await {
        ServerMsg::Welcome { peer, .. } => peer,
        other => panic!("expected Welcome, got {other:?}"),
    };
    for i in 0..4 {
        send_client_msg(
            &mut alice,
            ClientMsg::Submit(Op::new(CrdtId::new(alice_peer, i), OpKind::AddNode)),
        )
        .await;
        let _ = next_server_msg(&mut alice).await;
    }

    // Take a snapshot at seq=4 — without it, GC has nothing to compact
    // against.
    let room = rooms.get_or_create("gc-room").await.expect("room");
    room.take_snapshot().await.expect("snapshot");

    // Alice acks she's seen seq=4.
    send_client_msg(&mut alice, ClientMsg::Ping { applied_seq: 4 }).await;
    match next_server_msg(&mut alice).await {
        ServerMsg::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }

    // Run one GC cycle. min_ack=4, snapshot_seq=4, so upper=4.
    let dropped = room.run_gc().await.expect("gc");
    assert_eq!(dropped, 4, "all four ops should be compacted");

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
    send_client_msg(
        &mut alice,
        ClientMsg::Hello {
            room: "disc-room".into(),
            since: 0,
        },
    )
    .await;
    let _ = next_server_msg(&mut alice).await;

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
    send_client_msg(
        &mut ws,
        ClientMsg::Hello {
            room: "ping-test".into(),
            since: 0,
        },
    )
    .await;
    let _welcome = next_server_msg(&mut ws).await;

    send_client_msg(&mut ws, ClientMsg::Ping { applied_seq: 0 }).await;
    match next_server_msg(&mut ws).await {
        ServerMsg::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}
