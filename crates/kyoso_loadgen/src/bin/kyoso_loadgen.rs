//! `kyoso_loadgen` binary — drives a configurable load test against
//! `kyoso_server` and writes a JSON report.
//!
//! ```text
//! kyoso_loadgen \
//!     [--url ws://localhost:7878/ws | --spawn-server] \
//!     --room bench \
//!     --model {graph|comments|mixed} \
//!     --clients 10 \
//!     --rate 100 \
//!     --duration 30 \
//!     --output target/harness-reports/loadgen-graph.json
//! ```
//!
//! Output JSON shape — see [`kyoso_loadgen::LoadReport`].

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use kyoso_loadgen::{LoadConfig, LoadModel, run};

#[derive(Parser, Debug)]
#[command(version, about = "WS load generator for kyoso_server")]
struct Args {
    /// Connect to an existing server. Mutually exclusive with --spawn-server.
    #[arg(long)]
    url: Option<String>,

    /// Spawn an in-process kyoso_server (in-memory) on a random port.
    #[arg(long, conflicts_with = "url")]
    spawn_server: bool,

    /// Room id every client joins.
    #[arg(long, default_value = "loadgen")]
    room: String,

    /// Number of concurrent clients.
    #[arg(long, default_value_t = 8)]
    clients: usize,

    /// Per-client target rate (ops/sec). Total offered load = clients * rate.
    #[arg(long, default_value_t = 100)]
    rate: u32,

    /// Test duration in seconds.
    #[arg(long, default_value_t = 10)]
    duration: u64,

    /// Which model to drive: graph | comments | mixed (50/50 alternating per client).
    #[arg(long, default_value = "graph")]
    model: LoadModel,

    /// Path to write the JSON report to. Defaults to stdout if omitted.
    #[arg(long)]
    output: Option<PathBuf>,
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

    if args.url.is_none() && !args.spawn_server {
        eprintln!("error: pass either --url <ws://...> or --spawn-server");
        std::process::exit(2);
    }

    let cfg = LoadConfig {
        url: args.url,
        room: args.room,
        clients: args.clients,
        rate_per_client: args.rate,
        duration: Duration::from_secs(args.duration),
        model: args.model,
    };

    let report = run(cfg).await?;
    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| std::io::Error::other(format!("serialize report: {e}")))?;
    match args.output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, json)?;
            eprintln!("wrote report → {}", path.display());
        }
        None => {
            println!("{json}");
        }
    }
    Ok(())
}
