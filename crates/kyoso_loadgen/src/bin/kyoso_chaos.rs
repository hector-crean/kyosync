//! `kyoso_chaos` — Layer 4a chaos-simulator runner.
//!
//! Sweeps N seeds against a chosen model with configurable drop +
//! reorder + delay parameters, and writes a JSON report. Default seeds
//! cover quick regression detection (10 seeds, ~seconds). Bump
//! `--seeds` for a more thorough property-test-style run.
//!
//! ```text
//! kyoso_chaos \
//!     --model {graph|comments} \
//!     --peers 5 \
//!     --rounds 200 \
//!     --drop-prob 0.1 \
//!     --max-delay 5 \
//!     --seeds 25 \
//!     --output target/harness-reports/chaos-graph.json
//! ```

use std::path::PathBuf;

use clap::Parser;
use kyoso_comments_crdt::CommentsBackend;
use kyoso_crdt::{CrdtId, CrdtModel};
use kyoso_graph_crdt::CrdtBackend;
use kyoso_loadgen::sim::{ChaosConfig, SweepReport, sweep_seeds};
use rand::Rng;

#[derive(clap::ValueEnum, Clone, Debug)]
enum SimModel {
    Graph,
    Comments,
}

#[derive(Parser, Debug)]
#[command(version, about = "CRDT chaos simulator (Layer 4a)")]
struct Args {
    #[arg(long, value_enum, default_value_t = SimModel::Graph)]
    model: SimModel,

    /// Number of replicas in each run.
    #[arg(long, default_value_t = 5)]
    peers: usize,

    /// Number of mutation rounds per run.
    #[arg(long, default_value_t = 200)]
    rounds: usize,

    /// Probability `[0.0, 1.0)` of any individual op delivery being
    /// initially dropped (re-delivered on final flush).
    #[arg(long, default_value_t = 0.1)]
    drop_prob: f64,

    /// Max extra rounds a delivery can be delayed by.
    #[arg(long, default_value_t = 5)]
    max_delay: usize,

    /// Number of distinct seeds to sweep.
    #[arg(long, default_value_t = 10)]
    seeds: usize,

    /// First seed; subsequent seeds are `first + 1`, `first + 2`, …
    #[arg(long, default_value_t = 0xCAFE_F00D)]
    first_seed: u64,

    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let base_cfg = ChaosConfig {
        peers: args.peers,
        op_rounds: args.rounds,
        drop_probability: args.drop_prob,
        max_delay_rounds: args.max_delay,
        seed: args.first_seed,
    };
    let seeds: Vec<u64> = (0..args.seeds as u64)
        .map(|i| args.first_seed.wrapping_add(i))
        .collect();

    let report: SweepReport = match args.model {
        SimModel::Graph => sweep_seeds::<CrdtBackend<(), ()>, _>(base_cfg, seeds, graph_mutate),
        SimModel::Comments => sweep_seeds::<CommentsBackend, _>(base_cfg, seeds, comments_mutate),
    };

    eprintln!("{report}");
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
    if !report.all_converged {
        std::process::exit(1);
    }
    Ok(())
}

/// Per-round mutation for the graph backend. Exercises every
/// mutating method on `CrdtBackend`:
///
/// - `add_node` (~30%)
/// - `add_edge` between two random nodes (~10%) — exercises the
///   `or_insert`/`and_modify` path that used to drop the
///   cascade-tombstone fix
/// - `move_node` reparenting a random node under another (~6%) —
///   exercises `apply_remote`'s cycle check against a state that
///   may not match canonical's because of local pre-apply
/// - `remove_edge` (~3%)
/// - `remove_node` (~4%) — cascades through incident edges
///
/// Each path is a different CRDT invariant under chaos. Findings
/// from runs include divergent seeds with one-line repros so a
/// fresh divergence can be replayed bit-for-bit.
fn graph_mutate(
    backend: &mut CrdtBackend<(), ()>,
    rng: &mut rand::rngs::StdRng,
    _round: usize,
    _peer: kyoso_crdt::PeerId,
) {
    if rng.gen_bool(0.3) {
        backend.add_node();
    }
    if rng.gen_bool(0.10) {
        let snap = backend.snapshot();
        if snap.nodes.len() >= 2 {
            let a_idx = rng.gen_range(0..snap.nodes.len());
            let b_idx = rng.gen_range(0..snap.nodes.len());
            if a_idx != b_idx {
                backend.add_edge(snap.nodes[a_idx].id, snap.nodes[b_idx].id);
            }
        }
    }
    if rng.gen_bool(0.06) {
        let snap = backend.snapshot();
        if snap.nodes.len() >= 2 {
            let target_idx = rng.gen_range(0..snap.nodes.len());
            // 25% chance of detaching to root, otherwise reparent
            // under a random other node.
            let new_parent = if rng.gen_bool(0.25) {
                None
            } else {
                let mut p_idx = rng.gen_range(0..snap.nodes.len());
                while p_idx == target_idx {
                    p_idx = rng.gen_range(0..snap.nodes.len());
                }
                Some(snap.nodes[p_idx].id)
            };
            backend.move_node(
                snap.nodes[target_idx].id,
                new_parent,
                format!("p{:x}", rng.gen_range(0..u32::MAX)),
            );
        }
    }
    if rng.gen_bool(0.03) {
        let snap = backend.snapshot();
        if !snap.edges.is_empty() {
            let idx = rng.gen_range(0..snap.edges.len());
            backend.remove_edge(snap.edges[idx].id);
        }
    }
    if rng.gen_bool(0.04) {
        let snap = backend.snapshot();
        if !snap.nodes.is_empty() {
            let idx = rng.gen_range(0..snap.nodes.len());
            backend.remove_node(snap.nodes[idx].id);
        }
    }
}

/// Per-round mutation for the comments backend. Each peer adds a
/// comment ~30% of the time and edits a random existing comment ~10%
/// of the time.
fn comments_mutate(
    backend: &mut CommentsBackend,
    rng: &mut rand::rngs::StdRng,
    _round: usize,
    _peer: kyoso_crdt::PeerId,
) {
    if rng.gen_bool(0.3) {
        backend.add_comment(
            CrdtId::new(99, 42),
            None,
            format!("body-{}", rng.gen_range(0..10_000)),
        );
    }
    if rng.gen_bool(0.10) {
        let snap = backend.snapshot();
        if !snap.comments.is_empty() {
            let idx = rng.gen_range(0..snap.comments.len());
            backend.edit_body(
                snap.comments[idx].id,
                format!("v-{}", rng.gen_range(0..10_000)),
            );
        }
    }
    if rng.gen_bool(0.03) {
        let snap = backend.snapshot();
        if !snap.comments.is_empty() {
            let idx = rng.gen_range(0..snap.comments.len());
            backend.delete_comment(snap.comments[idx].id);
        }
    }
}
