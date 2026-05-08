//! Async WebSocket client for [`kyoso_server`](kyoso_server) talking the
//! [`kyoso_crdt`] protocol.
//!
//! Owns its own multi-threaded tokio runtime so the rest of the host
//! process — typically a Bevy app on a non-tokio thread — doesn't have
//! to deal with async/await. Communicates with the host via two
//! mpsc-style channels:
//!
//! - **Outbound** (host → io task) `tokio::sync::mpsc<ClientMsg>` — the
//!   host pushes any [`ClientMsg`] (ops, acks, presence updates).
//! - **Inbound** (io task → host) `crossbeam_channel<Inbound>` —
//!   `try_recv` is sync and runtime-free, which is exactly what a Bevy
//!   system needs.

use futures::{SinkExt, StreamExt};
use kyoso_crdt::{GlobalSeq, PeerId};
use kyoso_graph_crdt::{OpKind, Snapshot};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// Bind framework generics to the graph model. Stage 2 will replace this
// with a multi-model envelope routed by ModelId.
type Op = kyoso_crdt::Op<OpKind>;
type Diff = kyoso_crdt::Diff<OpKind>;
type ClientMsg = kyoso_crdt::ClientMsg<OpKind>;
type ServerMsg = kyoso_crdt::ServerMsg<OpKind, Snapshot>;

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("codec: {0}")]
    Codec(String),
}

impl From<postcard::Error> for ConnectError {
    fn from(e: postcard::Error) -> Self {
        Self::Codec(format!("{e}"))
    }
}

/// A single inbound event from the network as the host should observe it.
#[derive(Debug)]
pub enum Inbound {
    Welcome {
        peer: PeerId,
        snapshot: Option<Snapshot>,
        diff: Diff,
        /// Current room-wide presence snapshot. Each entry is a peer's
        /// most recent [`ClientMsg::Presence`] payload, opaque bytes the
        /// consumer decodes.
        presence: Vec<(PeerId, Vec<u8>)>,
    },
    Apply(Op),
    Catchup(Diff),
    /// Peer `peer` updated their presence. State is opaque bytes —
    /// decode with whatever scheme the consumer used to encode.
    PresenceUpdate {
        peer: PeerId,
        state: Vec<u8>,
    },
    /// Peer `peer` cleared their presence (explicit leave or disconnect).
    PresenceLeft {
        peer: PeerId,
    },
    /// Server sent an error frame. Connection may still be open.
    ServerError(String),
    /// The transport has closed (server-side close, network drop, …).
    /// No further `Inbound` will arrive on this client.
    Disconnected,
}

pub struct WsClient {
    /// Held to keep the io task alive — dropping the runtime aborts the
    /// task and closes the channels.
    _runtime: tokio::runtime::Runtime,
    outbound_tx: mpsc::UnboundedSender<ClientMsg>,
    inbound_rx: crossbeam_channel::Receiver<Inbound>,
}

impl WsClient {
    /// Connect to `url`, send the initial `Hello { room, since: 0 }`,
    /// then spawn the bidirectional io task. Blocks the calling thread
    /// until the WebSocket handshake + Hello complete.
    pub fn connect(url: &str, room: &str) -> Result<Self, ConnectError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("kyoso-sync-ws")
            .build()?;

        let url = url.to_string();
        let room = room.to_string();
        let (sink, stream) = runtime.block_on(async move {
            let (ws, _resp) = tokio_tungstenite::connect_async(url).await?;
            let (mut sink, stream) = ws.split();
            let hello = ClientMsg::Hello { room, since: 0 }.encode()?;
            sink.send(WsMessage::Binary(hello.into())).await?;
            Ok::<_, ConnectError>((sink, stream))
        })?;

        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (inbound_tx, inbound_rx) = crossbeam_channel::unbounded();

        runtime.spawn(io_loop(sink, stream, outbound_rx, inbound_tx));

        Ok(Self {
            _runtime: runtime,
            outbound_tx,
            inbound_rx,
        })
    }

    /// Queue any [`ClientMsg`] for transmission. Returns `false` if the io
    /// task has shut down (transport gone) — the host should treat this
    /// as disconnected.
    pub fn send(&self, msg: ClientMsg) -> bool {
        self.outbound_tx.send(msg).is_ok()
    }

    /// Convenience: wrap `op` in [`ClientMsg::Submit`] and queue it.
    pub fn send_op(&self, op: Op) -> bool {
        self.send(ClientMsg::Submit(op))
    }

    /// Convenience: send a [`ClientMsg::Ping`] carrying the host's
    /// current applied-seq (used by the server for compaction GC).
    pub fn send_ack(&self, applied_seq: GlobalSeq) -> bool {
        self.send(ClientMsg::Ping { applied_seq })
    }

    /// Convenience: replace this peer's presence state with `state`.
    pub fn send_presence(&self, state: Vec<u8>) -> bool {
        self.send(ClientMsg::Presence(state))
    }

    /// Convenience: clear this peer's presence (server also clears on disconnect).
    pub fn leave_presence(&self) -> bool {
        self.send(ClientMsg::LeavePresence)
    }

    /// Non-blocking poll for a next inbound event. Bevy systems call
    /// this in a loop until `None`.
    pub fn try_recv(&self) -> Option<Inbound> {
        self.inbound_rx.try_recv().ok()
    }
}

// `tokio::runtime::Runtime` aborts pending tasks and joins worker
// threads when dropped, so no explicit shutdown is needed. We don't
// bother sending a Close frame; the server's reader handles abrupt
// disconnect just as well.

async fn io_loop(
    mut sink: futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        WsMessage,
    >,
    mut stream: futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    mut outbound_rx: mpsc::UnboundedReceiver<ClientMsg>,
    inbound_tx: crossbeam_channel::Sender<Inbound>,
) {
    loop {
        tokio::select! {
            msg = outbound_rx.recv() => {
                let Some(msg) = msg else { break; };
                let bytes = match msg.encode() {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = ?e, "encode ClientMsg");
                        continue;
                    }
                };
                if sink.send(WsMessage::Binary(bytes.into())).await.is_err() {
                    break;
                }
            }
            frame = stream.next() => {
                let Some(frame) = frame else { break; };
                let frame = match frame {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = ?e, "ws recv");
                        break;
                    }
                };
                match frame {
                    WsMessage::Binary(bytes) => match ServerMsg::decode(&bytes) {
                        Ok(ServerMsg::Welcome { peer, snapshot, diff, presence }) => {
                            if inbound_tx
                                .send(Inbound::Welcome { peer, snapshot, diff, presence })
                                .is_err() { break; }
                        }
                        Ok(ServerMsg::Apply(op)) => {
                            if inbound_tx.send(Inbound::Apply(op)).is_err() { break; }
                        }
                        Ok(ServerMsg::Catchup(diff)) => {
                            if inbound_tx.send(Inbound::Catchup(diff)).is_err() { break; }
                        }
                        Ok(ServerMsg::Pong) => {}
                        Ok(ServerMsg::Error { message }) => {
                            let _ = inbound_tx.send(Inbound::ServerError(message));
                        }
                        Ok(ServerMsg::PresenceUpdate { peer, state }) => {
                            if inbound_tx.send(Inbound::PresenceUpdate { peer, state }).is_err() { break; }
                        }
                        Ok(ServerMsg::PresenceLeft { peer }) => {
                            if inbound_tx.send(Inbound::PresenceLeft { peer }).is_err() { break; }
                        }
                        Err(e) => {
                            let _ = inbound_tx.send(Inbound::ServerError(format!("decode: {e}")));
                        }
                    },
                    WsMessage::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    let _ = inbound_tx.send(Inbound::Disconnected);
}
