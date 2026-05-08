//! Per-connection WebSocket lifecycle.
//!
//! Two halves over one socket:
//!
//! - **Reader** (this task) — decodes `ClientMsg` frames and dispatches:
//!   `Submit` → `room.submit`, `Catchup` → reply via outbox, `Ping` →
//!   record ack + reply, second `Hello` → error.
//! - **Writer** (spawned task) — `tokio::select!`s over the
//!   per-connection mpsc and the room broadcast, encodes `ServerMsg`
//!   frames, writes them out.
//!
//! On disconnect the reader drops the outbox sender (writer drains and
//! exits) and clears the peer's row from the ack table so it stops
//! holding back compaction.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use kyoso_crdt::{GlobalSeq, PeerId};
use tokio::sync::mpsc;

use crate::AppState;
use crate::model::{ClientMsg, ServerMsg};
use crate::services::Room;

const OUTBOX_CAPACITY: usize = 32;

pub async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| serve(socket, state))
}

#[tracing::instrument(skip_all)]
async fn serve(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (outbox_tx, mut outbox_rx) = mpsc::channel::<ServerMsg>(OUTBOX_CAPACITY);

    let Some((room, peer)) = handshake(&mut stream, &outbox_tx, &state).await else {
        return;
    };

    let mut broadcast_rx = room.subscribe();
    tracing::info!(room = %room.id, peer, "client joined");

    let writer = tokio::spawn(async move {
        loop {
            // `else => break` in select! only fires when *all* branches
            // are disabled; here we explicitly detect outbox-closed
            // (the reader's drop signal) and broadcast-closed (room
            // dropped) so the task always wakes from idle.
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
            let bytes = match msg.encode() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = ?e, "outbound encode failed");
                    continue;
                }
            };
            if sink.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Binary(bytes) => match ClientMsg::decode(&bytes) {
                Ok(ClientMsg::Hello { .. }) => {
                    let _ = outbox_tx
                        .send(ServerMsg::Error {
                            message: "already joined".into(),
                        })
                        .await;
                }
                Ok(ClientMsg::Submit(op)) => {
                    if let Err(e) = room.submit(op).await {
                        tracing::warn!(room = %room.id, peer, error = %e, "submit failed");
                        let _ = outbox_tx
                            .send(ServerMsg::Error {
                                message: format!("submit: {e}"),
                            })
                            .await;
                    }
                }
                Ok(ClientMsg::Catchup { since }) => match room.catchup(since).await {
                    Ok(diff) => {
                        let _ = outbox_tx.send(ServerMsg::Catchup(diff)).await;
                    }
                    Err(e) => {
                        let _ = outbox_tx
                            .send(ServerMsg::Error {
                                message: format!("catchup: {e}"),
                            })
                            .await;
                    }
                },
                Ok(ClientMsg::Ping { applied_seq }) => {
                    if let Err(e) = record_ack_silently(&room, peer, applied_seq).await {
                        tracing::warn!(error = %e, "record_ack failed");
                    }
                    let _ = outbox_tx.send(ServerMsg::Pong).await;
                }
                Ok(ClientMsg::Presence(state)) => {
                    room.update_presence(peer, state).await;
                }
                Ok(ClientMsg::LeavePresence) => {
                    room.clear_presence(peer).await;
                }
                Err(e) => {
                    let _ = outbox_tx
                        .send(ServerMsg::Error {
                            message: format!("decode: {e}"),
                        })
                        .await;
                }
            },
            Message::Text(t) => {
                let _ = outbox_tx
                    .send(ServerMsg::Error {
                        message: format!("expected binary frames, got text: {}", text_preview(&t)),
                    })
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
    outbox: &mpsc::Sender<ServerMsg>,
    state: &AppState,
) -> Option<(Arc<Room>, PeerId)> {
    let frame = stream.next().await?.ok()?;
    let bytes = match frame {
        Message::Binary(b) => b,
        Message::Text(t) => {
            let _ = outbox
                .send(ServerMsg::Error {
                    message: format!("expected Hello (binary), got text: {}", text_preview(&t)),
                })
                .await;
            return None;
        }
        _ => return None,
    };
    let hello: ClientMsg = ClientMsg::decode(&bytes).ok()?;
    let (room_id, since) = match hello {
        ClientMsg::Hello { room, since } => (room, since),
        other => {
            let _ = outbox
                .send(ServerMsg::Error {
                    message: format!("first frame must be Hello, got {other:?}"),
                })
                .await;
            return None;
        }
    };

    let room = match state.rooms.get_or_create(&room_id).await {
        Ok(r) => r,
        Err(e) => {
            let _ = outbox
                .send(ServerMsg::Error {
                    message: format!("room init: {e}"),
                })
                .await;
            return None;
        }
    };
    let peer = room.assign_peer();
    let (snapshot, diff) = match room.welcome_for(since).await {
        Ok(p) => p,
        Err(e) => {
            let _ = outbox
                .send(ServerMsg::Error {
                    message: format!("welcome: {e}"),
                })
                .await;
            return None;
        }
    };
    // Seed the ack table with what the client claims to already have so
    // GC can observe an accurate min from the moment of join.
    let starting_ack = snapshot.as_ref().map_or(since, |s| s.at_seq);
    let _ = room.record_ack(peer, starting_ack).await;

    let presence = room.presence_snapshot().await;
    tracing::debug!(
        room = %room.id,
        peer,
        since,
        snapshot_at = snapshot.as_ref().map(|s| s.at_seq),
        diff_ops = diff.ops.len(),
        diff_to = diff.to_seq,
        presence_peers = presence.len(),
        "welcome sent"
    );
    let _ = outbox
        .send(ServerMsg::Welcome {
            peer,
            snapshot,
            diff,
            presence,
        })
        .await;
    Some((room, peer))
}

async fn record_ack_silently(
    room: &Room,
    peer: PeerId,
    applied: GlobalSeq,
) -> crate::Result<()> {
    room.record_ack(peer, applied).await
}

fn text_preview(t: &Utf8Bytes) -> String {
    let s: &str = t.as_str();
    if s.len() > 60 {
        format!("{}…", &s[..60])
    } else {
        s.to_string()
    }
}
