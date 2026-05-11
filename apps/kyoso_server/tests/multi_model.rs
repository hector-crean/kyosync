//! End-to-end: graph + comments models multiplexed onto a single
//! WebSocket connection through the multi-model envelope.
//!
//! The test bypasses [`kyoso_sync::WsClient`] (which is graph-only
//! today) and speaks the wire protocol directly so we can exercise both
//! models from the same client. Two peers each subscribe to both
//! models in their `Hello`; one creates a graph node and a comment
//! anchored to it; the other observes both round-trips and converges.

use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kyoso_comments_crdt::{CommentOpKind, comments_model};
use kyoso_crdt::{
    CrdtId, EnvelopeClientMsg, EnvelopeServerMsg, GlobalSeq, ModelGreeting, ModelId, PeerId, Tier,
};
use kyoso_graph_crdt::{OpKind, graph_model};
use kyoso_server::{AppState, app};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

type GraphOp = kyoso_crdt::Op<OpKind>;
type CommentOp = kyoso_crdt::Op<CommentOpKind>;
type CommentDiff = kyoso_crdt::Diff<CommentOpKind>;

async fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(AppState::in_memory());
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
    }
}

/// Read frames until one matches `predicate`, returning the matched
/// frame. Useful for tolerating interleaved Pongs / presence frames.
async fn next_envelope_matching<S, P>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    predicate: P,
) -> EnvelopeServerMsg
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    P: Fn(&EnvelopeServerMsg) -> bool,
{
    loop {
        let env = next_envelope(ws).await;
        if predicate(&env) {
            return env;
        }
    }
}

/// Send `Hello { room, models: [(graph, 0), (comments, 0)] }`.
async fn hello_both<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, room: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_envelope(
        ws,
        EnvelopeClientMsg::Hello {
            room: room.to_string(),
            tier: Tier::ReadWrite,
            models: vec![(graph_model(), 0), (comments_model(), 0)],
        },
    )
    .await;
}

async fn submit<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    model: ModelId,
    payload: Vec<u8>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_envelope(ws, EnvelopeClientMsg::Submit { model, payload }).await;
}

async fn submit_graph<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, op: &GraphOp)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    submit(ws, graph_model(), postcard::to_allocvec(op).unwrap()).await;
}

async fn submit_comment<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, op: &CommentOp)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    submit(ws, comments_model(), postcard::to_allocvec(op).unwrap()).await;
}

struct WelcomeBoth {
    peer: PeerId,
    graph_diff: kyoso_crdt::Diff<OpKind>,
    comments_diff: CommentDiff,
}

fn decode_welcome(env: EnvelopeServerMsg) -> WelcomeBoth {
    let EnvelopeServerMsg::Welcome { peer, models, .. } = env else {
        panic!("expected Welcome, got {env:?}");
    };
    let graph_id = graph_model();
    let comments_id = comments_model();
    let graph_greeting = find_greeting(&models, &graph_id);
    let comments_greeting = find_greeting(&models, &comments_id);
    let graph_diff: kyoso_crdt::Diff<OpKind> =
        postcard::from_bytes(&graph_greeting.diff_payload).expect("decode graph diff");
    let comments_diff: CommentDiff =
        postcard::from_bytes(&comments_greeting.diff_payload).expect("decode comments diff");
    WelcomeBoth {
        peer,
        graph_diff,
        comments_diff,
    }
}

fn find_greeting<'a>(models: &'a [ModelGreeting], target: &ModelId) -> &'a ModelGreeting {
    models
        .iter()
        .find(|g| &g.model == target)
        .unwrap_or_else(|| panic!("Welcome missing greeting for {target}"))
}

/// Decode an Apply envelope as a graph op. Panics if the model is
/// not graph or the payload doesn't decode.
fn decode_graph_apply(env: EnvelopeServerMsg) -> GraphOp {
    let EnvelopeServerMsg::Apply { model, payload } = env else {
        panic!("expected Apply, got {env:?}");
    };
    assert_eq!(model, graph_model());
    postcard::from_bytes(&payload).expect("decode graph op")
}

fn decode_comment_apply(env: EnvelopeServerMsg) -> CommentOp {
    let EnvelopeServerMsg::Apply { model, payload } = env else {
        panic!("expected Apply, got {env:?}");
    };
    assert_eq!(model, comments_model());
    postcard::from_bytes(&payload).expect("decode comment op")
}

#[tokio::test]
async fn graph_and_comments_multiplex_on_one_connection() {
    let addr = spawn_server().await;

    let mut alice = connect(addr).await;
    let mut bob = connect(addr).await;

    hello_both(&mut alice, "multi").await;
    hello_both(&mut bob, "multi").await;

    let alice_welcome = decode_welcome(next_envelope(&mut alice).await);
    let bob_welcome = decode_welcome(next_envelope(&mut bob).await);

    assert!(alice_welcome.graph_diff.is_empty());
    assert!(alice_welcome.comments_diff.is_empty());
    assert!(bob_welcome.graph_diff.is_empty());
    assert!(bob_welcome.comments_diff.is_empty());
    assert_ne!(alice_welcome.peer, bob_welcome.peer);

    // Alice creates a graph node.
    let node_op = GraphOp::new(CrdtId::new(alice_welcome.peer, 0), OpKind::AddNode);
    submit_graph(&mut alice, &node_op).await;

    let alice_node_apply = decode_graph_apply(
        next_envelope_matching(&mut alice, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    let bob_node_apply = decode_graph_apply(
        next_envelope_matching(&mut bob, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    assert_eq!(alice_node_apply.id, node_op.id);
    assert_eq!(alice_node_apply.seq, Some(1));
    assert_eq!(bob_node_apply.id, node_op.id);
    assert_eq!(bob_node_apply.seq, Some(1));
    let node_id = node_op.id;

    // Bob anchors a comment to Alice's graph node — cross-model
    // reference. The comment's id is minted from Bob's peer namespace.
    let comment_op = CommentOp::new(
        CrdtId::new(bob_welcome.peer, 0),
        CommentOpKind::AddComment {
            anchor: node_id,
            parent: None,
            body: "looks great".into(),
        },
    );
    submit_comment(&mut bob, &comment_op).await;

    let alice_comment_apply = decode_comment_apply(
        next_envelope_matching(&mut alice, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    let bob_comment_apply = decode_comment_apply(
        next_envelope_matching(&mut bob, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    // Comments has its own per-model seq starting at 1 (independent
    // of graph's seq).
    assert_eq!(alice_comment_apply.seq, Some(1));
    assert_eq!(bob_comment_apply.seq, Some(1));
    // Anchor must match the graph node id Alice minted — proves the
    // cross-model reference round-tripped intact.
    if let CommentOpKind::AddComment { anchor, body, .. } = &alice_comment_apply.kind {
        assert_eq!(*anchor, node_id);
        assert_eq!(body, "looks great");
    } else {
        panic!("expected AddComment, got {:?}", alice_comment_apply.kind);
    }

    // Bob edits the body.
    let edit_op = CommentOp::new(
        CrdtId::new(bob_welcome.peer, 1),
        CommentOpKind::EditBody {
            target: comment_op.id,
            body: "looks great!!".into(),
        },
    );
    submit_comment(&mut bob, &edit_op).await;

    let alice_edit = decode_comment_apply(
        next_envelope_matching(&mut alice, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    let bob_edit = decode_comment_apply(
        next_envelope_matching(&mut bob, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    assert_eq!(alice_edit.seq, Some(2));
    assert_eq!(bob_edit.seq, Some(2));

    // Sanity: graph submissions still get a graph seq, independent of
    // comments. Submit another graph op and verify it stamps as graph
    // seq=2 even though comments is also at seq=2.
    let node2_op = GraphOp::new(CrdtId::new(alice_welcome.peer, 1), OpKind::AddNode);
    submit_graph(&mut alice, &node2_op).await;
    let alice_node2_apply = decode_graph_apply(
        next_envelope_matching(&mut alice, |e| matches!(e, EnvelopeServerMsg::Apply { .. })).await,
    );
    assert_eq!(
        alice_node2_apply.seq,
        Some(2),
        "graph seq must advance independently of comments seq"
    );
}

#[tokio::test]
async fn comments_optional_in_hello_keeps_legacy_clients_working() {
    // A client that subscribes to graph only (no comments) gets a
    // welcome with one greeting and the comments path is silently
    // unused on its connection. Mirrors the old single-model flow.
    let addr = spawn_server().await;
    let mut ws = connect(addr).await;
    send_envelope(
        &mut ws,
        EnvelopeClientMsg::Hello {
            room: "graph-only".into(),
            tier: Tier::ReadWrite,
            models: vec![(graph_model(), 0)],
        },
    )
    .await;
    let env = next_envelope(&mut ws).await;
    let EnvelopeServerMsg::Welcome { models, .. } = env else {
        panic!("expected Welcome, got {env:?}");
    };
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model, graph_model());
}

#[tokio::test]
async fn late_joiner_receives_comments_history_in_welcome() {
    let addr = spawn_server().await;

    // First peer subscribes to both, posts a comment, then disconnects.
    let mut early = connect(addr).await;
    hello_both(&mut early, "late-comments").await;
    let early_welcome = decode_welcome(next_envelope(&mut early).await);

    let comment_op = CommentOp::new(
        CrdtId::new(early_welcome.peer, 0),
        CommentOpKind::AddComment {
            anchor: CrdtId::new(99, 42),
            parent: None,
            body: "first!".into(),
        },
    );
    submit_comment(&mut early, &comment_op).await;
    // Drain echo so the test stays in sync.
    let _ = next_envelope_matching(&mut early, |e| matches!(e, EnvelopeServerMsg::Apply { .. }))
        .await;

    // Second peer joins and should see the comment in its welcome diff.
    let mut late_joiner = connect(addr).await;
    hello_both(&mut late_joiner, "late-comments").await;
    let welcome = decode_welcome(next_envelope(&mut late_joiner).await);
    assert!(welcome.graph_diff.is_empty());
    assert_eq!(welcome.comments_diff.ops.len(), 1);
    assert_eq!(welcome.comments_diff.ops[0].id, comment_op.id);
    assert_eq!(welcome.comments_diff.ops[0].seq, Some(1));
}

/// `Tier::Read` connection: server rejects graph submits (writer-tier
/// only) but accepts comment submits (handler is tier-permissive).
/// Confirms phase 2's `allows_submit` policy is wired into the server.
#[tokio::test]
async fn read_tier_rejects_graph_submit_allows_comment_submit() {
    let addr = spawn_server().await;
    let mut ws = connect(addr).await;
    send_envelope(
        &mut ws,
        EnvelopeClientMsg::Hello {
            room: "tier-test".into(),
            tier: Tier::Read,
            models: vec![(graph_model(), 0), (comments_model(), 0)],
        },
    )
    .await;

    // Welcome echoes the granted tier.
    let env = next_envelope(&mut ws).await;
    let EnvelopeServerMsg::Welcome { peer, tier_granted, .. } = env else {
        panic!("expected Welcome, got {env:?}");
    };
    assert_eq!(tier_granted, Tier::Read);

    // Reader cannot submit a graph op — server replies Error.
    let graph_op: GraphOp = kyoso_crdt::Op::new(CrdtId::new(peer, 1), OpKind::AddNode);
    submit_graph(&mut ws, &graph_op).await;
    let env = next_envelope_matching(&mut ws, |e| matches!(e, EnvelopeServerMsg::Error { .. }))
        .await;
    let EnvelopeServerMsg::Error { message } = env else {
        unreachable!();
    };
    assert!(
        message.contains("permission denied") || message.contains("Read"),
        "expected permission-denied error, got: {message}"
    );

    // Reader CAN submit a comment — comments handler is tier-permissive.
    let comment_op: CommentOp = kyoso_crdt::Op::new(
        CrdtId::new(peer, 2),
        CommentOpKind::AddComment {
            anchor: CrdtId::new(peer, 99),
            parent: None,
            body: "from a reader".into(),
        },
    );
    submit_comment(&mut ws, &comment_op).await;
    // Reader connections receive Apply ops as a coalesced ApplyBatch
    // on the next tick (default 250 ms).
    let env = next_envelope_matching(&mut ws, |e| {
        matches!(e, EnvelopeServerMsg::ApplyBatch { model, .. } if model == &comments_model())
    })
    .await;
    let EnvelopeServerMsg::ApplyBatch { payloads, .. } = env else {
        unreachable!();
    };
    assert_eq!(payloads.len(), 1, "exactly one comment should be batched");
    let echoed: CommentOp = postcard::from_bytes(&payloads[0]).unwrap();
    assert_eq!(echoed.id, comment_op.id);
    assert!(echoed.seq.is_some(), "server should stamp seq");
}

#[allow(dead_code)]
fn _silence_unused_imports(_: GlobalSeq) {}
