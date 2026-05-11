//! `kyoso_peer_sweep` — vary writer + reader peer counts against one
//! in-process server and chart writer latency, throughput, and per-tier
//! ingress bandwidth.
//!
//! Each step shares one server; clients all join the same room.
//! - **Writers** (`Tier::ReadWrite`) submit `AddNode` ops at the
//!   configured rate and measure submit-to-echo latency on the live
//!   broadcast path.
//! - **Readers** (`Tier::Read`) submit nothing; they consume the
//!   coalesced `ApplyBatch` stream and measure per-reader ingress
//!   bandwidth + delivered op count.
//!
//! Holding writers fixed and varying readers across the sweep is the
//! direct test of the Phase 3 reader-coalescing win: if it works, we
//! can grow readers ~10× past the writer-only N=256 ceiling without
//! the broadcast channel saturating.
//!
//! ```text
//! kyoso_peer_sweep \
//!     --writers 4 \
//!     --readers 0,16,64,256,1024 \
//!     --rate 10 \
//!     --duration 5 \
//!     --output target/harness-reports/peer-sweep.json
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use hdrhistogram::Histogram;
use kyoso_crdt::{
    CrdtId, EnvelopeClientMsg, EnvelopeServerMsg, ModelId, Op, PeerId, Tier,
};
use kyoso_graph_crdt::{OpKind, graph_model};
use kyoso_server::{AppState, app};
use serde::Serialize;
use std::collections::HashMap;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Parser, Debug)]
#[command(version, about = "Mixed-tier peer-count sweep for kyoso_server")]
struct Args {
    /// Number of writer-tier (`Tier::ReadWrite`) clients per step.
    /// Held fixed across the sweep so the live broadcast load is
    /// constant; the swept axis is reader count.
    #[arg(long, default_value_t = 4)]
    writers: usize,

    /// Comma-separated reader-tier (`Tier::Read`) counts to sweep.
    /// `0` is the writer-only baseline.
    #[arg(long, default_value = "0,16,64,256,1024", value_delimiter = ',')]
    readers: Vec<usize>,

    /// Per-writer target rate (ops/sec). Readers never submit.
    #[arg(long, default_value_t = 10)]
    rate: u32,

    /// Duration of each step in seconds.
    #[arg(long, default_value_t = 5)]
    duration: u64,

    /// Path to write the JSON report to. Defaults to stdout if omitted.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Room id every client joins.
    #[arg(long, default_value = "sweep")]
    room: String,
}

#[derive(Debug, Serialize)]
struct SweepReport {
    writers: usize,
    rate_per_writer_ops_per_sec: u32,
    duration_per_step_s: u64,
    steps: Vec<StepReport>,
}

#[derive(Debug, Serialize)]
struct StepReport {
    n_readers: usize,
    n_writers: usize,
    /// Writers: ops submitted across all writer clients.
    writer_ops_submitted: u64,
    /// Writers: live-path echoes received.
    writer_ops_echoed: u64,
    /// Aggregate readers: total payloads delivered (sum of payloads
    /// across every `ApplyBatch` frame).
    reader_ops_received: u64,
    errors: u64,
    elapsed_s: f64,
    writer_throughput_ops_per_sec: f64,
    /// Avg per-writer ingress bytes/sec (writers receive everything
    /// on the live path, including their own echoes).
    avg_per_writer_ingress_bytes_per_sec: f64,
    /// Avg per-reader ingress bytes/sec (readers receive only
    /// `ApplyBatch` frames flushed every coalesce-interval).
    avg_per_reader_ingress_bytes_per_sec: f64,
    /// Total server egress (writers + readers).
    server_egress_bytes_per_sec: f64,
    /// Submit-to-echo latency on the writer live path.
    writer_latency_us: LatencyPercentiles,
}

#[derive(Debug, Serialize)]
struct LatencyPercentiles {
    p50: u64,
    p90: u64,
    p95: u64,
    p99: u64,
    p999: u64,
    max: u64,
    mean: f64,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut steps = Vec::with_capacity(args.readers.len());
    for &n_readers in &args.readers {
        eprintln!(
            "→ step writers={} readers={n_readers} rate={} duration={}s",
            args.writers, args.rate, args.duration
        );
        let step = run_step(
            args.writers,
            n_readers,
            args.rate,
            Duration::from_secs(args.duration),
            &args.room,
        )
        .await?;
        eprintln!(
            "  writer thr={:.0} ops/s, p50={}us p99={}us, writer_in={:.1} KB/s, reader_in={:.1} KB/s, server_egress={:.1} KB/s, errors={}",
            step.writer_throughput_ops_per_sec,
            step.writer_latency_us.p50,
            step.writer_latency_us.p99,
            step.avg_per_writer_ingress_bytes_per_sec / 1024.0,
            step.avg_per_reader_ingress_bytes_per_sec / 1024.0,
            step.server_egress_bytes_per_sec / 1024.0,
            step.errors,
        );
        steps.push(step);
    }

    let report = SweepReport {
        writers: args.writers,
        rate_per_writer_ops_per_sec: args.rate,
        duration_per_step_s: args.duration,
        steps,
    };
    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    match args.output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, json)?;
            eprintln!("wrote report → {}", path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}

async fn run_step(
    n_writers: usize,
    n_readers: usize,
    rate: u32,
    duration: Duration,
    room: &str,
) -> std::io::Result<StepReport> {
    let (addr, _server) = spawn_in_process_server().await?;
    let url = format!("ws://{addr}/ws");
    let started = Instant::now();
    let model = graph_model();

    let mut writer_tasks = Vec::with_capacity(n_writers);
    for idx in 0..n_writers {
        let url = url.clone();
        let room = room.to_string();
        let model = model.clone();
        writer_tasks.push(tokio::spawn(async move {
            writer_loop(url, room, model, rate, duration, idx).await
        }));
    }
    let mut reader_tasks = Vec::with_capacity(n_readers);
    for idx in 0..n_readers {
        let url = url.clone();
        let room = room.to_string();
        let model = model.clone();
        reader_tasks.push(tokio::spawn(async move {
            reader_loop(url, room, model, duration, idx).await
        }));
    }

    let mut hist: Histogram<u64> = Histogram::new(3).unwrap();
    let mut total_submitted = 0u64;
    let mut total_echoed = 0u64;
    let mut total_errors = 0u64;
    let mut writer_ingress_bytes = 0u64;
    let mut counted_writers = 0u64;
    for handle in writer_tasks {
        match handle.await {
            Ok(Ok(stats)) => {
                total_submitted += stats.submitted;
                total_echoed += stats.echoed;
                total_errors += stats.errors;
                writer_ingress_bytes += stats.ingress_bytes;
                counted_writers += 1;
                hist.add(&stats.latencies).ok();
            }
            Ok(Err(e)) => {
                tracing::error!(error = ?e, "writer task failed");
                total_errors += 1;
            }
            Err(e) => {
                tracing::error!(error = ?e, "writer join failed");
                total_errors += 1;
            }
        }
    }

    let mut reader_ingress_bytes = 0u64;
    let mut counted_readers = 0u64;
    let mut reader_ops_received = 0u64;
    for handle in reader_tasks {
        match handle.await {
            Ok(Ok(stats)) => {
                reader_ingress_bytes += stats.ingress_bytes;
                reader_ops_received += stats.ops_received;
                counted_readers += 1;
                total_errors += stats.errors;
            }
            Ok(Err(e)) => {
                tracing::error!(error = ?e, "reader task failed");
                total_errors += 1;
            }
            Err(e) => {
                tracing::error!(error = ?e, "reader join failed");
                total_errors += 1;
            }
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    let writer_throughput = total_echoed as f64 / elapsed;
    let avg_writer_in = if counted_writers > 0 {
        writer_ingress_bytes as f64 / counted_writers as f64 / elapsed
    } else {
        0.0
    };
    let avg_reader_in = if counted_readers > 0 {
        reader_ingress_bytes as f64 / counted_readers as f64 / elapsed
    } else {
        0.0
    };
    let server_egress =
        avg_writer_in * counted_writers as f64 + avg_reader_in * counted_readers as f64;
    Ok(StepReport {
        n_readers,
        n_writers,
        writer_ops_submitted: total_submitted,
        writer_ops_echoed: total_echoed,
        reader_ops_received,
        errors: total_errors,
        elapsed_s: elapsed,
        writer_throughput_ops_per_sec: writer_throughput,
        avg_per_writer_ingress_bytes_per_sec: avg_writer_in,
        avg_per_reader_ingress_bytes_per_sec: avg_reader_in,
        server_egress_bytes_per_sec: server_egress,
        writer_latency_us: LatencyPercentiles {
            p50: hist.value_at_quantile(0.50),
            p90: hist.value_at_quantile(0.90),
            p95: hist.value_at_quantile(0.95),
            p99: hist.value_at_quantile(0.99),
            p999: hist.value_at_quantile(0.999),
            max: hist.max(),
            mean: hist.mean(),
        },
    })
}

async fn spawn_in_process_server() -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = app(AppState::in_memory());
    let h = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = ?e, "server task ended");
        }
    });
    Ok((addr, h))
}

#[derive(Debug)]
struct WriterStats {
    submitted: u64,
    echoed: u64,
    errors: u64,
    ingress_bytes: u64,
    latencies: Histogram<u64>,
}

#[derive(Debug)]
struct ReaderStats {
    /// Number of payloads delivered across all `ApplyBatch` frames.
    ops_received: u64,
    /// Total bytes received on the WS reader.
    ingress_bytes: u64,
    errors: u64,
}

async fn writer_loop(
    url: String,
    room: String,
    model: ModelId,
    rate: u32,
    duration: Duration,
    client_idx: usize,
) -> std::io::Result<WriterStats> {
    let (ws, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = ?e, client = client_idx, "ws connect");
            return Ok(WriterStats {
                submitted: 0,
                echoed: 0,
                errors: 1,
                ingress_bytes: 0,
                latencies: Histogram::new(3).unwrap(),
            });
        }
    };
    let (mut sink, mut stream) = ws.split();
    let hello = EnvelopeClientMsg::Hello {
        room: room.clone(),
        tier: Tier::ReadWrite,
        models: vec![(model.clone(), 0)],
    };
    if send_envelope(&mut sink, hello).await.is_err() {
        return Ok(WriterStats {
            submitted: 0,
            echoed: 0,
            errors: 1,
            ingress_bytes: 0,
            latencies: Histogram::new(3).unwrap(),
        });
    }
    let peer = match read_until_welcome(&mut stream).await {
        Some(p) => p,
        None => {
            return Ok(WriterStats {
                submitted: 0,
                echoed: 0,
                errors: 1,
                ingress_bytes: 0,
                latencies: Histogram::new(3).unwrap(),
            });
        }
    };

    let pending: Arc<Mutex<HashMap<CrdtId, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let pending_reader = pending.clone();
    let model_reader = model.clone();
    let stop_at = Instant::now() + duration + Duration::from_millis(500);
    let ingress_bytes = Arc::new(AtomicU64::new(0));
    let ingress_reader = ingress_bytes.clone();

    let reader = tokio::spawn(async move {
        let mut hist: Histogram<u64> = Histogram::new(3).unwrap();
        let mut echoed = 0u64;
        loop {
            tokio::select! {
                frame = stream.next() => {
                    let Some(frame) = frame else { break; };
                    let Ok(WsMessage::Binary(bytes)) = frame else { continue; };
                    ingress_reader.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    let env = match EnvelopeServerMsg::decode(&bytes) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    if let EnvelopeServerMsg::Apply { model, payload } = env {
                        if model != model_reader { continue; }
                        if let Ok(op) = postcard::from_bytes::<Op<OpKind>>(&payload) {
                            if let Some(t0) = pending_reader.lock().await.remove(&op.id) {
                                let us = t0.elapsed().as_micros() as u64;
                                hist.record(us.max(1)).ok();
                                echoed += 1;
                            }
                        }
                    }
                }
                _ = tokio::time::sleep_until(stop_at.into()) => break,
            }
        }
        (echoed, hist)
    });

    let interval_us = 1_000_000u64 / rate.max(1) as u64;
    let mut next_at = Instant::now();
    let end_at = Instant::now() + duration;
    let mut local_seq: u64 = 0;
    let mut submitted = 0u64;
    let mut errors = 0u64;
    while Instant::now() < end_at {
        let now = Instant::now();
        if now < next_at {
            tokio::time::sleep_until(next_at.into()).await;
        }
        next_at += Duration::from_micros(interval_us);
        local_seq += 1;
        let op_id = CrdtId::new(peer, local_seq);
        let payload = match build_payload(&model, op_id) {
            Some(p) => p,
            None => {
                errors += 1;
                continue;
            }
        };
        pending.lock().await.insert(op_id, Instant::now());
        let env = EnvelopeClientMsg::Submit {
            model: model.clone(),
            payload,
        };
        if send_envelope(&mut sink, env).await.is_err() {
            errors += 1;
            break;
        }
        submitted += 1;
    }

    let (echoed, latencies) = reader.await.unwrap_or((0, Histogram::new(3).unwrap()));
    Ok(WriterStats {
        submitted,
        echoed,
        errors,
        ingress_bytes: ingress_bytes.load(Ordering::Relaxed),
        latencies,
    })
}

async fn reader_loop(
    url: String,
    room: String,
    model: ModelId,
    duration: Duration,
    client_idx: usize,
) -> std::io::Result<ReaderStats> {
    let (ws, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = ?e, client = client_idx, "ws connect (reader)");
            return Ok(ReaderStats {
                ops_received: 0,
                ingress_bytes: 0,
                errors: 1,
            });
        }
    };
    let (mut sink, mut stream) = ws.split();
    let hello = EnvelopeClientMsg::Hello {
        room: room.clone(),
        tier: Tier::Read,
        models: vec![(model.clone(), 0)],
    };
    if send_envelope(&mut sink, hello).await.is_err() {
        return Ok(ReaderStats {
            ops_received: 0,
            ingress_bytes: 0,
            errors: 1,
        });
    }
    if read_until_welcome(&mut stream).await.is_none() {
        return Ok(ReaderStats {
            ops_received: 0,
            ingress_bytes: 0,
            errors: 1,
        });
    }

    // Drain frames until duration elapses + a small tail to catch the
    // final coalesce flush.
    let stop_at = Instant::now() + duration + Duration::from_millis(500);
    let mut ingress_bytes = 0u64;
    let mut ops_received = 0u64;
    loop {
        tokio::select! {
            frame = stream.next() => {
                let Some(frame) = frame else { break; };
                let Ok(WsMessage::Binary(bytes)) = frame else { continue; };
                ingress_bytes += bytes.len() as u64;
                if let Ok(env) = EnvelopeServerMsg::decode(&bytes) {
                    match env {
                        EnvelopeServerMsg::ApplyBatch { model: m, payloads } if m == model => {
                            ops_received += payloads.len() as u64;
                        }
                        EnvelopeServerMsg::Apply { model: m, .. } if m == model => {
                            // A reader receiving an `Apply` (non-batched)
                            // means the dual-fanout is broken. Count it
                            // as a delivered op so the assertion is
                            // covered in `reader_ops_received` but log
                            // a warning.
                            tracing::warn!(client = client_idx, "reader got non-batched Apply");
                            ops_received += 1;
                        }
                        _ => {}
                    }
                }
            }
            _ = tokio::time::sleep_until(stop_at.into()) => break,
        }
    }
    Ok(ReaderStats {
        ops_received,
        ingress_bytes,
        errors: 0,
    })
}

fn build_payload(model: &ModelId, op_id: CrdtId) -> Option<Vec<u8>> {
    if *model == graph_model() {
        let op: Op<OpKind> = Op::new(op_id, OpKind::AddNode);
        postcard::to_allocvec(&op).ok()
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
