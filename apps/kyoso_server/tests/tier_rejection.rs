//! Tiered-write rejection: a `Tier::Read` connection that tries to
//! submit a graph op should get an `EnvelopeServerMsg::Error` back
//! without crashing the server, and the op must not broadcast to
//! other clients in the same room.

use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kyoso_crdt::{
    CrdtId, EnvelopeClientMsg, EnvelopeServerMsg, GlobalSeq, Tier,
};
use kyoso_graph_crdt::{graph_model, OpKind};
use kyoso_server::{AppState, app};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

type Op = kyoso_crdt::Op<OpKind>;

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

async fn connect(
    addr: SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
{
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

async fn hello<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    room: &str,
    tier: Tier,
    since: GlobalSeq,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_envelope(
        ws,
        EnvelopeClientMsg::Hello {
            room: room.to_string(),
            tier,
            models: vec![(graph_model(), since)],
        },
    )
    .await;
}

async fn submit_graph<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, op: &Op)
where
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

/// A `Read`-tier client sends `Submit` for the graph model. The
/// server must respond with `EnvelopeServerMsg::Error` and must NOT
/// broadcast the op to a writer-tier observer in the same room.
#[tokio::test]
async fn read_tier_submit_is_rejected() {
    let addr = spawn_server().await;
    let mut reader = connect(addr).await;
    let mut writer = connect(addr).await;

    // Writer joins first so we have an observer for the broadcast.
    hello(&mut writer, "tier-room", Tier::ReadWrite, 0).await;
    let _ = next_envelope(&mut writer).await; // Welcome
    // Reader joins with Tier::Read.
    hello(&mut reader, "tier-room", Tier::Read, 0).await;
    let _ = next_envelope(&mut reader).await; // Welcome

    // Reader tries to submit an AddNode op.
    let op = Op::new(CrdtId::new(99, 0), OpKind::AddNode);
    submit_graph(&mut reader, &op).await;

    // Reader should receive an Error frame, not an Apply.
    match next_envelope(&mut reader).await {
        EnvelopeServerMsg::Error { message } => {
            assert!(
                message.contains("tier") || message.contains("submit"),
                "error message should mention tier rejection, got `{message}`"
            );
        }
        other => panic!("expected Error frame on read-tier submit, got {other:?}"),
    }

    // Writer must not have received an Apply broadcast for this op
    // (the submit was rejected before reaching the broadcast channel).
    // Give the server a beat to settle and then verify the next frame
    // arriving on writer's stream is NOT an Apply within a short window.
    let probe = tokio::time::timeout(Duration::from_millis(300), next_envelope(&mut writer)).await;
    match probe {
        Err(_) => { /* no frame within 300ms — correct */ }
        Ok(EnvelopeServerMsg::Apply { .. }) => {
            panic!("read-tier submit leaked to writer-tier observer");
        }
        Ok(other) => {
            // Pong / Welcome echoes are fine, just not Apply.
            eprintln!("writer received non-Apply frame after read-tier reject: {other:?}");
        }
    }
}
