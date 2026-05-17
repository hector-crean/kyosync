//! `kyoso_soak` — long-duration loadgen with windowed reporting.
//!
//! Runs the same load profile in N back-to-back intervals against a
//! single in-process server. Emits a `SoakReport` JSON containing the
//! per-interval `LoadReport`s so an agent or human can spot:
//!
//! - **Latency drift**: p99 trending up across intervals → memory
//!   pressure, queue growth, or unbounded log replay.
//! - **Throughput cliff**: ops/s falling halfway through → broadcast
//!   channel backing up, or per-room state bloat.
//! - **Error accumulation**: errors growing past interval 1 →
//!   transient state leaking across reconnects.
//!
//! No RSS / heap-profile dimension yet — that's worth adding via dhat
//! or similar but is platform-specific. Defer until a soak run shows
//! a real symptom that needs memory drilldown.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use kyoso_loadgen::{run, LoadConfig, LoadModel, LoadReport};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(version, about = "Long-duration loadgen soak with windowed reporting")]
struct Args {
    /// Total duration in seconds. Each interval is `duration / intervals`.
    #[arg(long, default_value_t = 60)]
    duration: u64,
    /// How many windowed intervals to record. Default = 6 → one
    /// sample every 10s for the default 60s run.
    #[arg(long, default_value_t = 6)]
    intervals: u64,
    /// Concurrent clients (same shape as `kyoso_loadgen`).
    #[arg(long, default_value_t = 8)]
    clients: usize,
    /// Per-client target rate (ops/s).
    #[arg(long, default_value_t = 50)]
    rate: u32,
    /// Workload: `graph`.
    #[arg(long, default_value = "graph")]
    model: String,
    /// Room name (clients share the same room across intervals so
    /// per-room state grows realistically).
    #[arg(long, default_value = "soak")]
    room: String,
    /// Output path.
    #[arg(long, default_value = "target/harness-reports/soak.json")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct IntervalReport {
    interval: u64,
    /// Wall-clock at the start of this interval (seconds since soak start).
    started_at_s: f64,
    report: LoadReport,
}

#[derive(Debug, Serialize)]
struct SoakReport {
    total_duration_s: f64,
    intervals: Vec<IntervalReport>,
    /// Crude drift metric: max(p99) / min(p99) across intervals. >2.0
    /// is suspicious; the agent should investigate.
    p99_drift_ratio: f64,
    /// Same shape for throughput.
    throughput_drift_ratio: f64,
    /// Total errors across all intervals — should be 0 for a clean run.
    total_errors: u64,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,kyoso_soak=info")),
        )
        .init();
    let args = Args::parse();
    let model = match args.model.as_str() {
        "graph" => LoadModel::Graph,
        other => {
            eprintln!("unknown --model `{other}` (use graph)");
            std::process::exit(2);
        }
    };
    let interval_duration = (args.duration / args.intervals).max(1);

    let soak_started = Instant::now();
    let mut intervals = Vec::with_capacity(args.intervals as usize);
    for i in 0..args.intervals {
        let cfg = LoadConfig {
            // None → spawn a fresh in-process server. We *want* the
            // server to persist across intervals so per-room state
            // accumulates — but kyoso_loadgen::run currently spawns
            // a new server per call. The next refactor of this binary
            // should add a long-lived server option; for now each
            // interval is its own server which still catches client-
            // side leaks and protocol drift.
            url: None,
            room: args.room.clone(),
            clients: args.clients,
            rate_per_client: args.rate,
            duration: Duration::from_secs(interval_duration),
            model: model.clone(),
        };
        let started_at_s = soak_started.elapsed().as_secs_f64();
        eprintln!(
            "interval {} of {} — t={started_at_s:.1}s, duration={interval_duration}s",
            i + 1,
            args.intervals
        );
        match run(cfg).await {
            Ok(report) => {
                eprintln!(
                    "  ✓ {} ops/s, p99 {}µs, errors={}",
                    report.throughput_ops_per_sec as u64,
                    report.latency_us.p99,
                    report.errors
                );
                intervals.push(IntervalReport {
                    interval: i + 1,
                    started_at_s,
                    report,
                });
            }
            Err(e) => {
                eprintln!("  ✘ interval failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let p99s: Vec<u64> = intervals.iter().map(|i| i.report.latency_us.p99).collect();
    let ths: Vec<f64> = intervals
        .iter()
        .map(|i| i.report.throughput_ops_per_sec)
        .collect();
    let p99_drift_ratio = ratio(&p99s.iter().map(|&v| v as f64).collect::<Vec<_>>());
    let throughput_drift_ratio = ratio(&ths);
    let total_errors: u64 = intervals.iter().map(|i| i.report.errors).sum();
    let total_duration_s = soak_started.elapsed().as_secs_f64();

    let soak = SoakReport {
        total_duration_s,
        intervals,
        p99_drift_ratio,
        throughput_drift_ratio,
        total_errors,
    };

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent).expect("mkdir reports dir");
    }
    let json = serde_json::to_string_pretty(&soak).expect("serialize");
    std::fs::write(&args.output, json).expect("write report");
    eprintln!(
        "wrote {} — p99 drift {:.2}×, throughput drift {:.2}×, errors {}",
        args.output.display(),
        soak.p99_drift_ratio,
        soak.throughput_drift_ratio,
        soak.total_errors,
    );
    if soak.total_errors > 0 || soak.p99_drift_ratio > 3.0 {
        std::process::exit(1);
    }
}

fn ratio(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mn = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let mx = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if mn <= 0.0 {
        return 0.0;
    }
    mx / mn
}
