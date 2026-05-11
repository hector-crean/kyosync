//! Async WebSocket transport for [`kyoso_server`](kyoso_server) speaking
//! the [`kyoso_crdt`] multi-model envelope protocol.
//!
//! [`WsClient`] is fully model-agnostic: it owns the WebSocket connection
//! and shuttles [`EnvelopeClientMsg`] / [`EnvelopeServerMsg`] frames in
//! both directions. It does **not** know about graphs, comments, or any
//! specific data model — those live in per-model Bevy plugins
//! (`kyoso_graph_sync`, `kyoso_comments_sync`) that share this transport.
//!
//! Per-model traffic is tagged with a [`ModelId`]. Per-model plugins
//! filter the inbound [`Inbound::ModelApply`] / [`Inbound::ModelCatchup`]
//! events for their own model, decode the byte payload as their typed
//! `Op<K>` / `Diff<K>`, and apply locally.
//!
//! Owns its own multi-threaded tokio runtime so the rest of the host
//! process — typically a Bevy app on a non-tokio thread — doesn't have
//! to deal with async/await.

use futures::{SinkExt, StreamExt};
use kyoso_crdt::{
    EnvelopeClientMsg, EnvelopeServerMsg, GlobalSeq, ModelGreeting, ModelId, PeerId, RoomId, Tier,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

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
///
/// Generic over the model — the per-model decode happens downstream in
/// each model plugin's inbound system.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// Reply to the initial Hello. Carries the assigned [`PeerId`], a
    /// per-model greeting list (snapshot + diff for each subscribed
    /// model), and the room-wide presence snapshot.
    ///
    /// Per-model plugins find their model's [`ModelGreeting`] by
    /// [`ModelId`] and decode the byte payloads as their typed
    /// `S` / `Diff<K>`.
    Welcome {
        peer: PeerId,
        tier_granted: Tier,
        models: Vec<ModelGreeting>,
        presence: Vec<(PeerId, Vec<u8>)>,
    },
    /// Server-confirmed op for one model. Payload is postcard-encoded
    /// `kyoso_crdt::Op<K_M>` for that model. Live (uncoalesced) path —
    /// `ReadWrite`-tier connections receive every op as one of these.
    ModelApply { model: ModelId, payload: Vec<u8> },
    /// Server-confirmed batch of ops for one model on the coalesced
    /// reader path. Each `payload` is the same shape as
    /// [`Inbound::ModelApply::payload`]; apply in order.
    ModelApplyBatch { model: ModelId, payloads: Vec<Vec<u8>> },
    /// Catchup reply for one model. Payload is postcard-encoded
    /// `kyoso_crdt::Diff<K_M>`.
    ModelCatchup { model: ModelId, payload: Vec<u8> },
    /// Peer `peer` updated their presence. State is opaque bytes.
    PresenceUpdate { peer: PeerId, state: Vec<u8> },
    /// Peer `peer` cleared their presence (explicit leave or disconnect).
    PresenceLeft { peer: PeerId },
    /// Server sent an error frame. Connection may still be open.
    ServerError(String),
    /// The transport has closed (server-side close, network drop, …).
    /// No further `Inbound` will arrive on this client.
    Disconnected,
}

/// Per-peer transport handle. Holds the tokio runtime that owns the io
/// task; dropping the client aborts the task and closes the channels.
pub struct WsClient {
    /// Held to keep the io task alive — dropping the runtime aborts the
    /// task and closes the channels.
    _runtime: tokio::runtime::Runtime,
    outbound_tx: mpsc::UnboundedSender<EnvelopeClientMsg>,
    inbound_rx: crossbeam_channel::Receiver<Inbound>,
}

impl WsClient {
    /// Open the WebSocket and send the initial `Hello` envelope listing
    /// every model the peer wants to subscribe to. Blocks the caller
    /// until the WS handshake + Hello complete.
    ///
    /// `models` is `(model_id, since)` pairs: the per-model `since`
    /// cursor is the highest applied seq for that model on the peer
    /// (`0` for fresh joiners).
    pub fn connect(
        url: &str,
        room: &RoomId,
        tier: Tier,
        models: Vec<(ModelId, GlobalSeq)>,
    ) -> Result<Self, ConnectError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("kyoso-sync-ws")
            .build()?;

        let url = url.to_string();
        let room = room.clone();
        let (sink, stream) = runtime.block_on(async move {
            let (ws, _resp) = tokio_tungstenite::connect_async(url).await?;
            let (mut sink, stream) = ws.split();
            let hello = EnvelopeClientMsg::Hello { room, tier, models }.encode()?;
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

    /// Queue any [`EnvelopeClientMsg`] for transmission. Returns `false`
    /// if the io task has shut down (transport gone) — the host should
    /// treat this as disconnected.
    pub fn send_envelope(&self, msg: EnvelopeClientMsg) -> bool {
        self.outbound_tx.send(msg).is_ok()
    }

    /// Wrap a per-model op payload in [`EnvelopeClientMsg::Submit`] and
    /// queue it. Per-model plugins encode their typed `Op<K>` to bytes
    /// and call this.
    pub fn submit(&self, model: ModelId, payload: Vec<u8>) -> bool {
        self.send_envelope(EnvelopeClientMsg::Submit { model, payload })
    }

    /// Send a per-model catchup request.
    pub fn catchup(&self, model: ModelId, since: GlobalSeq) -> bool {
        self.send_envelope(EnvelopeClientMsg::Catchup { model, since })
    }

    /// Send a per-model ack ([`EnvelopeClientMsg::Ping`]).
    pub fn ack(&self, model: ModelId, applied_seq: GlobalSeq) -> bool {
        self.send_envelope(EnvelopeClientMsg::Ping { model, applied_seq })
    }

    /// Replace this peer's presence state.
    pub fn send_presence(&self, state: Vec<u8>) -> bool {
        self.send_envelope(EnvelopeClientMsg::Presence(state))
    }

    /// Clear this peer's presence (server also clears on disconnect).
    pub fn leave_presence(&self) -> bool {
        self.send_envelope(EnvelopeClientMsg::LeavePresence)
    }

    /// Non-blocking poll for a next inbound event. Bevy systems call
    /// this in a loop until `None`. Single-consumer — the
    /// [`crate::SyncTransportPlugin`] owns the drain and re-emits as
    /// Bevy events for per-model plugins to filter.
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
    mut outbound_rx: mpsc::UnboundedReceiver<EnvelopeClientMsg>,
    inbound_tx: crossbeam_channel::Sender<Inbound>,
) {
    loop {
        tokio::select! {
            msg = outbound_rx.recv() => {
                let Some(msg) = msg else { break; };
                let bytes = match msg.encode() {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = ?e, "encode EnvelopeClientMsg");
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
                    WsMessage::Binary(bytes) => match EnvelopeServerMsg::decode(&bytes) {
                        Ok(env) => {
                            let inbound = match env {
                                EnvelopeServerMsg::Welcome { peer, tier_granted, models, presence } => {
                                    Inbound::Welcome { peer, tier_granted, models, presence }
                                }
                                EnvelopeServerMsg::Apply { model, payload } => {
                                    Inbound::ModelApply { model, payload }
                                }
                                EnvelopeServerMsg::ApplyBatch { model, payloads } => {
                                    Inbound::ModelApplyBatch { model, payloads }
                                }
                                EnvelopeServerMsg::Catchup { model, payload } => {
                                    Inbound::ModelCatchup { model, payload }
                                }
                                EnvelopeServerMsg::Pong => continue,
                                EnvelopeServerMsg::Error { message } => {
                                    Inbound::ServerError(message)
                                }
                                EnvelopeServerMsg::PresenceUpdate { peer, state } => {
                                    Inbound::PresenceUpdate { peer, state }
                                }
                                EnvelopeServerMsg::PresenceLeft { peer } => {
                                    Inbound::PresenceLeft { peer }
                                }
                            };
                            if inbound_tx.send(inbound).is_err() { break; }
                        }
                        Err(e) => {
                            let _ = inbound_tx.send(
                                Inbound::ServerError(format!("decode envelope: {e}")),
                            );
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
