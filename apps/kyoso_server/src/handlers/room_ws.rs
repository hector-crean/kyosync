//! Per-connection WebSocket lifecycle.
//!
//! Two halves over one socket:
//!
//! - **Reader** — decodes [`EnvelopeClientMsg`] frames and forwards
//!   them to [`Room`]'s router (`submit` / `catchup` / `record_ack`).
//!   The router looks the [`ModelId`] up in its handler table; this
//!   file no longer knows the difference between graph traffic and
//!   comments traffic.
//! - **Writer** — `tokio::select!`s over the per-connection
//!   [`EnvelopeServerMsg`] outbox and the room broadcast, encodes each
//!   envelope frame, writes it out.
//!
//! On disconnect the reader drops the outbox sender (writer drains and
//! exits) and asks every handler to release the peer's ack row + clears
//! room-wide presence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use kyoso_crdt::{EnvelopeClientMsg, EnvelopeServerMsg, GlobalSeq, ModelId, PeerId, Tier};
use tokio::sync::mpsc;

use crate::AppState;
use crate::services::Room;

const OUTBOX_CAPACITY: usize = 32;

/// Reader-tier broadcast flush cadence. 250 ms is the v1 default —
/// trades ~quarter-second freshness for an order-of-magnitude room
/// scaling improvement (see HARNESS.md Layer 4c). Override via
/// `KYOSO_READER_COALESCE_MS` env var for benchmarking.
fn reader_coalesce_interval() -> Duration {
    std::env::var("KYOSO_READER_COALESCE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(250))
}

pub async fn upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| serve(socket, state))
}

#[tracing::instrument(skip_all)]
async fn serve(socket: WebSocket, state: AppState) {
    let (sink, mut stream) = socket.split();
    let (outbox_tx, outbox_rx) = mpsc::channel::<EnvelopeServerMsg>(OUTBOX_CAPACITY);

    let Some((room, peer, tier)) = handshake(&mut stream, &outbox_tx, &state).await else {
        return;
    };

    let broadcast_rx = room.subscribe();
    tracing::info!(room = %room.id, peer, ?tier, "client joined");

    let writer = match tier {
        Tier::ReadWrite => tokio::spawn(writer_live(sink, outbox_rx, broadcast_rx)),
        Tier::Read => {
            let interval = reader_coalesce_interval();
            tokio::spawn(writer_coalesced(sink, outbox_rx, broadcast_rx, interval))
        }
    };

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Binary(bytes) => match EnvelopeClientMsg::decode(&bytes) {
                Ok(EnvelopeClientMsg::Hello { .. }) => {
                    send_error(&outbox_tx, "already joined".into()).await;
                }
                Ok(EnvelopeClientMsg::Submit { model, payload }) => {
                    if !room.has_model(&model) {
                        send_error(&outbox_tx, format!("unknown model: {model}")).await;
                        continue;
                    }
                    if let Err(e) = room.submit(&model, tier, payload).await {
                        tracing::warn!(room = %room.id, peer, %model, error = %e, "submit failed");
                        send_error(&outbox_tx, format!("submit ({model}): {e}")).await;
                    }
                }
                Ok(EnvelopeClientMsg::Catchup { model, since }) => {
                    if !room.has_model(&model) {
                        send_error(&outbox_tx, format!("unknown model: {model}")).await;
                        continue;
                    }
                    match room.catchup(&model, since).await {
                        Ok(payload) => {
                            let _ = outbox_tx
                                .send(EnvelopeServerMsg::Catchup {
                                    model: model.clone(),
                                    payload,
                                })
                                .await;
                        }
                        Err(e) => {
                            send_error(&outbox_tx, format!("catchup ({model}): {e}")).await;
                        }
                    }
                }
                Ok(EnvelopeClientMsg::Ping { model, applied_seq }) => {
                    if !room.has_model(&model) {
                        send_error(&outbox_tx, format!("unknown model: {model}")).await;
                        continue;
                    }
                    if let Err(e) = room.record_ack(&model, peer, applied_seq).await {
                        tracing::warn!(error = %e, "record_ack failed");
                    }
                    let _ = outbox_tx.send(EnvelopeServerMsg::Pong).await;
                }
                Ok(EnvelopeClientMsg::Presence(state)) => {
                    room.update_presence(peer, state).await;
                }
                Ok(EnvelopeClientMsg::LeavePresence) => {
                    room.clear_presence(peer).await;
                }
                Err(e) => {
                    send_error(&outbox_tx, format!("decode envelope: {e}")).await;
                }
            },
            Message::Text(t) => {
                send_error(
                    &outbox_tx,
                    format!("expected binary frames, got text: {}", text_preview(&t)),
                )
                .await;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    drop(outbox_tx);
    let _ = writer.await;

    if let Err(e) = room.release_peer(peer).await {
        tracing::warn!(room = %room.id, peer, error = %e, "release_peer failed");
    }
    tracing::info!(room = %room.id, peer, "client disconnected");
}

async fn handshake(
    stream: &mut futures::stream::SplitStream<WebSocket>,
    outbox: &mpsc::Sender<EnvelopeServerMsg>,
    state: &AppState,
) -> Option<(Arc<Room>, PeerId, Tier)> {
    let frame = stream.next().await?.ok()?;
    let bytes = match frame {
        Message::Binary(b) => b,
        Message::Text(t) => {
            send_error(
                outbox,
                format!("expected Hello (binary), got text: {}", text_preview(&t)),
            )
            .await;
            return None;
        }
        _ => return None,
    };
    let hello = match EnvelopeClientMsg::decode(&bytes) {
        Ok(h) => h,
        Err(e) => {
            send_error(outbox, format!("decode Hello envelope: {e}")).await;
            return None;
        }
    };
    let (room_id, requested_tier, requested_models) = match hello {
        EnvelopeClientMsg::Hello { room, tier, models } => (room, tier, models),
        other => {
            send_error(outbox, format!("first frame must be Hello, got {other:?}")).await;
            return None;
        }
    };
    // Phase 1: trust the requested tier verbatim. A real authz hook
    // (consult policy store, downgrade if peer lacks write permission)
    // is a follow-up — this is the seam.
    let tier_granted = requested_tier;

    let room = match state.rooms.get_or_create(&room_id).await {
        Ok(r) => r,
        Err(e) => {
            send_error(outbox, format!("room init: {e}")).await;
            return None;
        }
    };

    // Reject the join if the client asked for any model this room
    // doesn't host. Catches typos and accidental cross-deployment
    // joins (graph-only server, comments-subscribing client).
    for (model, _) in &requested_models {
        if !room.has_model(model) {
            send_error(
                outbox,
                format!("room {} does not host model {model}", &room_id),
            )
            .await;
            return None;
        }
    }

    let peer = room.assign_peer();
    let greetings = match room.welcome_for(&requested_models).await {
        Ok(g) => g,
        Err(e) => {
            send_error(outbox, format!("welcome: {e}")).await;
            return None;
        }
    };

    // Seed each requested model's ack table with the client's claimed
    // `since`. Models that don't track acks (default trait impl) are
    // no-ops. This preserves the existing GC behaviour where joining
    // immediately surfaces the peer in `min_ack`.
    for (model, since) in &requested_models {
        let _ = room.record_ack(model, peer, *since).await;
    }

    let presence = room.presence_snapshot().await;
    tracing::debug!(
        room = %room.id,
        peer,
        models = ?requested_models.iter().map(|(m, s)| (m.as_str().to_string(), *s)).collect::<Vec<_>>(),
        presence_peers = presence.len(),
        "welcome sent"
    );
    let _ = outbox
        .send(EnvelopeServerMsg::Welcome {
            peer,
            tier_granted,
            models: greetings,
            presence,
        })
        .await;
    Some((room, peer, tier_granted))
}

async fn send_error(outbox: &mpsc::Sender<EnvelopeServerMsg>, message: String) {
    let _ = outbox.send(EnvelopeServerMsg::Error { message }).await;
}

fn text_preview(t: &Utf8Bytes) -> String {
    let s: &str = t.as_str();
    if s.len() > 60 {
        format!("{}…", &s[..60])
    } else {
        s.to_string()
    }
}

// Suppress dead-code warning — `GlobalSeq` is part of the public
// signature of helper functions in this module's tests.
#[allow(dead_code)]
fn _suppress(_: GlobalSeq, _: ModelId) {}

// ---------------------------------------------------------------------
// Writer tasks — one per tier.
//
// Both consume the same per-connection outbox (Welcome, Catchup, Pong,
// Error frames are sent directly regardless of tier) and the same
// shared room broadcast (every Apply landed by the room). They differ
// only in how they handle Apply frames:
//
// - `writer_live` forwards each Apply individually with no buffering —
//   `ReadWrite` peers see ops as soon as the room broadcast emits them.
// - `writer_coalesced` buffers Apply payloads per-model in a local
//   HashMap and flushes them as `ApplyBatch` frames on a fixed tick.
//   Trades ~quarter-second freshness for an order-of-magnitude
//   reduction in per-connection wakeups → larger room ceiling.
// ---------------------------------------------------------------------

type WsSink = futures::stream::SplitSink<WebSocket, Message>;

async fn writer_live(
    mut sink: WsSink,
    mut outbox_rx: mpsc::Receiver<EnvelopeServerMsg>,
    mut broadcast_rx: tokio::sync::broadcast::Receiver<EnvelopeServerMsg>,
) {
    loop {
        let msg = tokio::select! {
            m = outbox_rx.recv() => match m {
                Some(m) => m,
                None => break,
            },
            m = broadcast_rx.recv() => match m {
                Ok(m) => m,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "broadcast lag; dropping client");
                    break;
                }
            },
        };
        if !send_envelope(&mut sink, msg).await {
            break;
        }
    }
}

async fn writer_coalesced(
    mut sink: WsSink,
    mut outbox_rx: mpsc::Receiver<EnvelopeServerMsg>,
    mut broadcast_rx: tokio::sync::broadcast::Receiver<EnvelopeServerMsg>,
    coalesce_interval: Duration,
) {
    let mut buffer: HashMap<ModelId, Vec<Vec<u8>>> = HashMap::new();
    let mut tick = tokio::time::interval(coalesce_interval);
    // Skip the immediate tick that `interval` fires so we don't flush
    // an empty buffer in the first millisecond.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            m = outbox_rx.recv() => match m {
                Some(m) => {
                    // Direct frames (Welcome / Catchup / Pong / Error /
                    // PresenceUpdate / PresenceLeft) bypass the buffer
                    // — they're not part of the high-frequency
                    // broadcast and the reader needs them promptly.
                    if !send_envelope(&mut sink, m).await {
                        break;
                    }
                }
                None => break,
            },
            m = broadcast_rx.recv() => match m {
                Ok(EnvelopeServerMsg::Apply { model, payload }) => {
                    buffer.entry(model).or_default().push(payload);
                }
                Ok(other) => {
                    // Presence broadcasts and any future non-Apply
                    // broadcast frames forward immediately — same
                    // rationale as outbox direct frames.
                    if !send_envelope(&mut sink, other).await {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "broadcast lag; dropping reader");
                    break;
                }
            },
            _ = tick.tick() => {
                if buffer.is_empty() {
                    continue;
                }
                for (model, payloads) in buffer.drain() {
                    if payloads.is_empty() {
                        continue;
                    }
                    let frame = EnvelopeServerMsg::ApplyBatch { model, payloads };
                    if !send_envelope(&mut sink, frame).await {
                        return;
                    }
                }
            }
        }
    }
}

/// Encode + send one envelope. Returns `false` on encode failure or
/// closed sink — caller should stop the writer loop.
async fn send_envelope(sink: &mut WsSink, msg: EnvelopeServerMsg) -> bool {
    let bytes = match msg.encode() {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = ?e, "outbound encode failed");
            return true; // skip this frame, keep the loop alive
        }
    };
    sink.send(Message::Binary(bytes.into())).await.is_ok()
}
