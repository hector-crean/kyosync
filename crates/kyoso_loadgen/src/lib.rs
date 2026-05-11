//! Bench-harness library — two complementary tools:
//!
//! - **[`run`]** (load generator): spawns N concurrent WS clients
//!   speaking the [`kyoso_crdt::EnvelopeClientMsg`] protocol and
//!   records per-op submit-to-echo latency in an [`hdrhistogram`].
//!   Drives Layer 3 of the harness — throughput / latency under real
//!   tokio + axum + tokio-tungstenite.
//! - **[`sim`]** (chaos simulator): pure in-process simulator that
//!   drives any [`CrdtModel`] under seeded random network conditions
//!   (drops, reorders, delays) and asserts convergence. Drives Layer
//!   4a of the harness — CRDT correctness under adversarial conditions.
//!
//! Each tool is usable as a library (call [`run`] / [`sim::run_chaos_sim`]
//! from a test) or a binary (`kyoso_loadgen --help`, `kyoso_chaos --help`).

pub mod findings;
pub mod sim;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use hdrhistogram::Histogram;
use kyoso_comments_crdt::{CommentOpKind, comments_model};
use kyoso_crdt::{CrdtId, EnvelopeClientMsg, EnvelopeServerMsg, ModelId, Op, PeerId, Tier};
use kyoso_graph_crdt::{OpKind, graph_model};
use kyoso_server::{AppState, app};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LoadModel {
    Graph,
    Comments,
    /// 50/50 graph + comments alternating per client.
    Mixed,
}

impl std::str::FromStr for LoadModel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "graph" => Ok(Self::Graph),
            "comments" => Ok(Self::Comments),
            "mixed" => Ok(Self::Mixed),
            other => Err(format!("unknown model: {other}")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LoadConfig {
    /// Where to connect. Either an explicit `ws://...` URL or `None`
    /// (spawn an in-process server on a random port).
    pub url: Option<String>,
    pub room: String,
    pub clients: usize,
    /// Ops/sec per client. Total offered load = `clients * rate_per_client`.
    pub rate_per_client: u32,
    pub duration: Duration,
    pub model: LoadModel,
}

#[derive(Debug, Serialize)]
pub struct LoadReport {
    pub config: LoadConfigSer,
    pub ops_submitted: u64,
    pub ops_echoed: u64,
    pub errors: u64,
    pub elapsed_s: f64,
    pub throughput_ops_per_sec: f64,
    /// All-percentiles in microseconds.
    pub latency_us: LatencyPercentiles,
}

#[derive(Debug, Serialize)]
pub struct LoadConfigSer {
    pub url: Option<String>,
    pub room: String,
    pub clients: usize,
    pub rate_per_client: u32,
    pub duration_s: u64,
    pub model: LoadModel,
}

#[derive(Debug, Serialize)]
pub struct LatencyPercentiles {
    pub p50: u64,
    pub p90: u64,
    pub p95: u64,
    pub p99: u64,
    pub p999: u64,
    pub max: u64,
    pub mean: f64,
    pub stddev: f64,
}

/// Run the load test against `url` (or an in-process server if
/// `cfg.url` is None) and return the aggregated report.
pub async fn run(cfg: LoadConfig) -> std::io::Result<LoadReport> {
    let (url, _server_handle) = match cfg.url.clone() {
        Some(url) => (url, None),
        None => {
            let (addr, h) = spawn_in_process_server().await?;
            (format!("ws://{addr}/ws"), Some(h))
        }
    };

    let started = Instant::now();
    let mut tasks = Vec::with_capacity(cfg.clients);
    let model_for = |i: usize| match cfg.model {
        LoadModel::Graph => graph_model(),
        LoadModel::Comments => comments_model(),
        LoadModel::Mixed => {
            if i % 2 == 0 {
                graph_model()
            } else {
                comments_model()
            }
        }
    };
    for i in 0..cfg.clients {
        let url = url.clone();
        let room = cfg.room.clone();
        let rate = cfg.rate_per_client;
        let dur = cfg.duration;
        let model = model_for(i);
        tasks.push(tokio::spawn(async move {
            client_loop(url, room, model, rate, dur, i).await
        }));
    }

    let mut hist: Histogram<u64> = Histogram::new(3).unwrap();
    let mut total_submitted = 0u64;
    let mut total_echoed = 0u64;
    let mut total_errors = 0u64;
    for handle in tasks {
        match handle.await {
            Ok(Ok(stats)) => {
                total_submitted += stats.submitted;
                total_echoed += stats.echoed;
                total_errors += stats.errors;
                hist.add(&stats.latencies).unwrap();
            }
            Ok(Err(e)) => {
                tracing::error!(error = ?e, "client task failed");
                total_errors += 1;
            }
            Err(e) => {
                tracing::error!(error = ?e, "join failed");
                total_errors += 1;
            }
        }
    }

    let elapsed = started.elapsed();
    let throughput = total_echoed as f64 / elapsed.as_secs_f64();

    Ok(LoadReport {
        config: LoadConfigSer {
            url: cfg.url.clone(),
            room: cfg.room,
            clients: cfg.clients,
            rate_per_client: cfg.rate_per_client,
            duration_s: cfg.duration.as_secs(),
            model: cfg.model,
        },
        ops_submitted: total_submitted,
        ops_echoed: total_echoed,
        errors: total_errors,
        elapsed_s: elapsed.as_secs_f64(),
        throughput_ops_per_sec: throughput,
        latency_us: LatencyPercentiles {
            p50: hist.value_at_quantile(0.50),
            p90: hist.value_at_quantile(0.90),
            p95: hist.value_at_quantile(0.95),
            p99: hist.value_at_quantile(0.99),
            p999: hist.value_at_quantile(0.999),
            max: hist.max(),
            mean: hist.mean(),
            stddev: hist.stdev(),
        },
    })
}

#[derive(Debug)]
struct ClientStats {
    submitted: u64,
    echoed: u64,
    errors: u64,
    latencies: Histogram<u64>,
}

impl Default for ClientStats {
    fn default() -> Self {
        Self {
            submitted: 0,
            echoed: 0,
            errors: 0,
            latencies: Histogram::new(3).unwrap(),
        }
    }
}

async fn spawn_in_process_server() -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = app(AppState::in_memory());
    let h = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = ?e, "server task ended with error");
        }
    });
    Ok((addr, h))
}

async fn client_loop(
    url: String,
    room: String,
    model: ModelId,
    rate_per_client: u32,
    duration: Duration,
    client_idx: usize,
) -> std::io::Result<ClientStats> {
    let mut stats = ClientStats {
        latencies: Histogram::new(3).unwrap(),
        ..Default::default()
    };
    let (ws, _resp) = match tokio_tungstenite::connect_async(&url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = ?e, client = client_idx, "ws connect");
            stats.errors += 1;
            return Ok(stats);
        }
    };
    let (mut sink, mut stream) = ws.split();

    // Hello listing the model we'll send.
    let hello = EnvelopeClientMsg::Hello {
        room: room.clone(),
        tier: Tier::ReadWrite,
        models: vec![(model.clone(), 0)],
    };
    if let Err(e) = send_envelope(&mut sink, hello).await {
        tracing::error!(error = ?e, client = client_idx, "send Hello");
        stats.errors += 1;
        return Ok(stats);
    }

    // Read Welcome to get our peer id.
    let peer = match read_until_welcome(&mut stream).await {
        Some(p) => p,
        None => {
            tracing::error!(client = client_idx, "no Welcome before stream end");
            stats.errors += 1;
            return Ok(stats);
        }
    };

    // pending_at: per-CrdtId timestamp the client recorded at submit.
    // The reader looks up echoes here to compute round-trip latency.
    let pending_at = Arc::new(Mutex::new(HashMap::<CrdtId, Instant>::new()));
    let pending_clone = pending_at.clone();
    let mut reader_latencies: Histogram<u64> = Histogram::new(3).unwrap();
    let model_for_reader = model.clone();
    let stop_at = Instant::now() + duration + Duration::from_millis(500);

    // Reader task: pulls Apply echoes, computes latency.
    let reader = tokio::spawn(async move {
        let mut echoed = 0u64;
        loop {
            tokio::select! {
                frame = stream.next() => {
                    let Some(frame) = frame else { break; };
                    let Ok(WsMessage::Binary(bytes)) = frame else { continue; };
                    let env = match EnvelopeServerMsg::decode(&bytes) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    if let EnvelopeServerMsg::Apply { model, payload } = env {
                        if model != model_for_reader { continue; }
                        // Decode just enough to extract the op id —
                        // both OpKind and CommentOpKind share the
                        // `Op<K> { id, seq, kind: K }` envelope.
                        // We try graph first, then comments.
                        let id_opt = decode_op_id_for_model(&model_for_reader, &payload);
                        if let Some(id) = id_opt {
                            let removed = pending_clone.lock().await.remove(&id);
                            if let Some(t0) = removed {
                                let elapsed_us = t0.elapsed().as_micros() as u64;
                                reader_latencies.record(elapsed_us.max(1)).ok();
                                echoed += 1;
                            }
                        }
                    }
                }
                _ = tokio::time::sleep_until(stop_at.into()) => break,
            }
        }
        (echoed, reader_latencies)
    });

    // Writer loop: at the target rate, mint and submit ops.
    let interval_us = 1_000_000u64 / rate_per_client.max(1) as u64;
    let mut next_at = Instant::now();
    let end_at = Instant::now() + duration;
    let mut local_seq: u64 = 0;
    while Instant::now() < end_at {
        let now = Instant::now();
        if now < next_at {
            tokio::time::sleep_until(next_at.into()).await;
        }
        next_at += Duration::from_micros(interval_us);
        local_seq += 1;
        let op_id = CrdtId::new(peer, local_seq);
        let payload = match build_op_payload(&model, op_id, peer) {
            Some(p) => p,
            None => {
                stats.errors += 1;
                continue;
            }
        };
        pending_at.lock().await.insert(op_id, Instant::now());
        let env = EnvelopeClientMsg::Submit {
            model: model.clone(),
            payload,
        };
        if let Err(e) = send_envelope(&mut sink, env).await {
            tracing::error!(error = ?e, client = client_idx, "send Submit");
            stats.errors += 1;
            break;
        }
        stats.submitted += 1;
    }

    // Drain echoes for a brief tail window.
    let (echoed, lat) = reader.await.unwrap_or((0, Histogram::new(3).unwrap()));
    stats.echoed = echoed;
    stats.latencies = lat;
    Ok(stats)
}

fn build_op_payload(model: &ModelId, op_id: CrdtId, peer: PeerId) -> Option<Vec<u8>> {
    if *model == graph_model() {
        let op: Op<OpKind> = Op::new(op_id, OpKind::AddNode);
        postcard::to_allocvec(&op).ok()
    } else if *model == comments_model() {
        let op: Op<CommentOpKind> = Op::new(
            op_id,
            CommentOpKind::AddComment {
                anchor: CrdtId::new(peer, 0),
                parent: None,
                body: "ld".into(),
            },
        );
        postcard::to_allocvec(&op).ok()
    } else {
        None
    }
}

fn decode_op_id_for_model(model: &ModelId, payload: &[u8]) -> Option<CrdtId> {
    if *model == graph_model() {
        let op: Op<OpKind> = postcard::from_bytes(payload).ok()?;
        Some(op.id)
    } else if *model == comments_model() {
        let op: Op<CommentOpKind> = postcard::from_bytes(payload).ok()?;
        Some(op.id)
    } else {
        None
    }
}

async fn send_envelope<S>(
    sink: &mut SplitSink<WebSocketStream<S>, WsMessage>,
    msg: EnvelopeClientMsg,
) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let bytes = msg.encode().map_err(|e| std::io::Error::other(format!("encode: {e}")))?;
    sink.send(WsMessage::Binary(bytes.into()))
        .await
        .map_err(|e| std::io::Error::other(format!("ws send: {e}")))?;
    Ok(())
}

async fn read_until_welcome<S>(
    stream: &mut SplitStream<WebSocketStream<S>>,
) -> Option<PeerId>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(frame) = stream.next().await {
        let Ok(WsMessage::Binary(bytes)) = frame else { continue };
        let Ok(env) = EnvelopeServerMsg::decode(&bytes) else { continue };
        if let EnvelopeServerMsg::Welcome { peer, .. } = env {
            return Some(peer);
        }
    }
    None
}
