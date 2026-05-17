//! `kyoso_chaos` — Layer 4a chaos-simulator runner.
//!
//! Sweeps N seeds against a chosen model with configurable drop +
//! reorder + delay parameters, and writes a JSON report. Default seeds
//! cover quick regression detection (10 seeds, ~seconds). Bump
//! `--seeds` for a more thorough property-test-style run.
//!
//! ```text
//! kyoso_chaos \
//!     --model graph \
//!     --peers 5 \
//!     --rounds 200 \
//!     --drop-prob 0.1 \
//!     --max-delay 5 \
//!     --seeds 25 \
//!     --output target/harness-reports/chaos-graph.json
//! ```

use std::path::PathBuf;

use clap::Parser;
use kyoso_crdt::EmptySchema;
use kyoso_graph_crdt::{GraphBackend, check_topology};
use kyoso_loadgen::sim::{ChaosConfig, SweepReport, sweep_seeds};
use rand::Rng;

/// Invariants closure for graph backends — runs the structural
/// topology checker and serialises any violations as plain strings.
fn graph_invariants(canonical: &GraphBackend<EmptySchema>) -> Vec<String> {
    check_topology(canonical.backend().topology())
        .into_iter()
        .map(|v| format!("{:?}: {}", v.kind, v.detail))
        .collect()
}

#[derive(clap::ValueEnum, Clone, Debug)]
enum SimModel {
    Graph,
}

/// Named graph workloads — distinct op-mix profiles that stress
/// specific code paths. `Default` keeps the historical mix.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Workload {
    /// Original mix (~30% AddNode, ~10% AddEdge, ~6% Move, ~3% RemoveEdge, ~4% RemoveNode).
    Default,
    /// Heavy Move + reparent activity — stresses Kleppmann tree
    /// concurrent-swap and `would_create_cycle` against fast-changing
    /// state.
    TreeRestructure,
    /// Heavy RemoveNode on nodes with many incident edges — stresses
    /// the cascade-tombstone path that has historically broken under
    /// concurrent AddEdge.
    CascadeHeavy,
    /// Heavy interleaving of structural and property ops — stresses
    /// the SchemaApply path against nodes that may not exist yet on
    /// every peer. Uses `EmptySchema` so the property apply is a
    /// no-op; what we're testing is the dispatch + applied_seq
    /// monotonicity, not the schema state itself.
    PropertyInterleave,
}

#[derive(Parser, Debug)]
#[command(version, about = "CRDT chaos simulator (Layer 4a)")]
struct Args {
    #[arg(long, value_enum, default_value_t = SimModel::Graph)]
    model: SimModel,

    /// Op-mix workload to drive. Only meaningful for `--model graph`.
    #[arg(long, value_enum, default_value_t = Workload::Default)]
    workload: Workload,

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
        SimModel::Graph => match args.workload {
            Workload::Default => sweep_seeds::<GraphBackend<EmptySchema>, _, _>(
                base_cfg,
                seeds,
                graph_mutate,
                graph_invariants,
            ),
            Workload::TreeRestructure => sweep_seeds::<GraphBackend<EmptySchema>, _, _>(
                base_cfg,
                seeds,
                graph_mutate_tree_restructure,
                graph_invariants,
            ),
            Workload::CascadeHeavy => sweep_seeds::<GraphBackend<EmptySchema>, _, _>(
                base_cfg,
                seeds,
                graph_mutate_cascade_heavy,
                graph_invariants,
            ),
            Workload::PropertyInterleave => sweep_seeds::<GraphBackend<EmptySchema>, _, _>(
                base_cfg,
                seeds,
                graph_mutate_property_interleave,
                graph_invariants,
            ),
        },
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
/// mutating method on `GraphBackend`:
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
    backend: &mut GraphBackend<EmptySchema>,
    rng: &mut rand::rngs::StdRng,
    _round: usize,
    _peer: kyoso_crdt::PeerId,
) {
    if rng.gen_bool(0.3) {
        backend.add_node();
    }
    if rng.gen_bool(0.10) {
        let snap = backend.snapshot();
        if snap.topology.nodes.len() >= 2 {
            let a_idx = rng.gen_range(0..snap.topology.nodes.len());
            let b_idx = rng.gen_range(0..snap.topology.nodes.len());
            if a_idx != b_idx {
                backend.add_edge(snap.topology.nodes[a_idx].id, snap.topology.nodes[b_idx].id);
            }
        }
    }
    if rng.gen_bool(0.06) {
        let snap = backend.snapshot();
        if snap.topology.nodes.len() >= 2 {
            let target_idx = rng.gen_range(0..snap.topology.nodes.len());
            // 25% chance of detaching to root, otherwise reparent
            // under a random other node.
            let new_parent = if rng.gen_bool(0.25) {
                None
            } else {
                let mut p_idx = rng.gen_range(0..snap.topology.nodes.len());
                while p_idx == target_idx {
                    p_idx = rng.gen_range(0..snap.topology.nodes.len());
                }
                Some(snap.topology.nodes[p_idx].id)
            };
            backend.move_node(
                snap.topology.nodes[target_idx].id,
                new_parent,
                format!("p{:x}", rng.gen_range(0..u32::MAX)),
            );
        }
    }
    if rng.gen_bool(0.03) {
        let snap = backend.snapshot();
        if !snap.topology.edges.is_empty() {
            let idx = rng.gen_range(0..snap.topology.edges.len());
            backend.remove_edge(snap.topology.edges[idx].id);
        }
    }
    if rng.gen_bool(0.04) {
        let snap = backend.snapshot();
        if !snap.topology.nodes.is_empty() {
            let idx = rng.gen_range(0..snap.topology.nodes.len());
            backend.remove_node(snap.topology.nodes[idx].id);
        }
    }
}

// ---------------------------------------------------------------------------
// Graph workloads
// ---------------------------------------------------------------------------

/// Tree-restructure workload: lots of Moves with concurrent reparents.
/// Exercises Kleppmann tree concurrent-swap and `would_create_cycle`
/// against fast-changing state. Add some AddNode early on to give
/// the workload material; once enough nodes exist, the mix shifts to
/// move-heavy.
fn graph_mutate_tree_restructure(
    backend: &mut GraphBackend<EmptySchema>,
    rng: &mut rand::rngs::StdRng,
    round: usize,
    _peer: kyoso_crdt::PeerId,
) {
    // Front-load AddNode so the early rounds have material.
    let warmup = round < 30;
    if warmup && rng.gen_bool(0.6) {
        backend.add_node();
        return;
    }
    if !warmup && rng.gen_bool(0.1) {
        backend.add_node();
    }
    // Move: 40% in steady state.
    if rng.gen_bool(0.40) {
        let snap = backend.snapshot();
        if snap.topology.nodes.len() >= 2 {
            let t_idx = rng.gen_range(0..snap.topology.nodes.len());
            let p_idx = if rng.gen_bool(0.25) {
                usize::MAX // sentinel for "detach to root"
            } else {
                let mut p = rng.gen_range(0..snap.topology.nodes.len());
                while p == t_idx {
                    p = rng.gen_range(0..snap.topology.nodes.len());
                }
                p
            };
            let new_parent = if p_idx == usize::MAX {
                None
            } else {
                Some(snap.topology.nodes[p_idx].id)
            };
            backend.move_node(
                snap.topology.nodes[t_idx].id,
                new_parent,
                format!("k{:x}", rng.gen_range(0..u32::MAX)),
            );
        }
    }
}

/// Cascade-heavy workload: build out edge-dense nodes, then RemoveNode
/// at a high rate so each removal cascades through many incident edges.
/// Exercises the path the historical `or_insert` cascade-tombstone bug
/// lived on.
fn graph_mutate_cascade_heavy(
    backend: &mut GraphBackend<EmptySchema>,
    rng: &mut rand::rngs::StdRng,
    round: usize,
    _peer: kyoso_crdt::PeerId,
) {
    // First third: build N nodes + many edges.
    let warmup = round < 40;
    if warmup {
        if rng.gen_bool(0.5) {
            backend.add_node();
        } else {
            let snap = backend.snapshot();
            if snap.topology.nodes.len() >= 2 {
                let i = rng.gen_range(0..snap.topology.nodes.len());
                let j = rng.gen_range(0..snap.topology.nodes.len());
                if i != j {
                    backend.add_edge(snap.topology.nodes[i].id, snap.topology.nodes[j].id);
                }
            }
        }
        return;
    }
    // Steady state: 30% RemoveNode, 20% add new node + edge to keep
    // graph from collapsing.
    if rng.gen_bool(0.30) {
        let snap = backend.snapshot();
        if !snap.topology.nodes.is_empty() {
            let idx = rng.gen_range(0..snap.topology.nodes.len());
            backend.remove_node(snap.topology.nodes[idx].id);
        }
    }
    if rng.gen_bool(0.20) {
        let new_node = backend.add_node();
        let snap = backend.snapshot();
        if !snap.topology.nodes.is_empty() {
            let idx = rng.gen_range(0..snap.topology.nodes.len());
            backend.add_edge(new_node, snap.topology.nodes[idx].id);
        }
    }
}

/// Property-interleave workload: structural and property ops mixed at
/// roughly equal rates. Property ops here are `set_node_property`
/// calls against nodes that may not exist on every peer yet — the
/// SchemaApply dispatch should tolerate that without breaking
/// `applied_seq` monotonicity.
fn graph_mutate_property_interleave(
    backend: &mut GraphBackend<EmptySchema>,
    rng: &mut rand::rngs::StdRng,
    _round: usize,
    _peer: kyoso_crdt::PeerId,
) {
    if rng.gen_bool(0.30) {
        backend.add_node();
    }
    // Property ops targeting an arbitrary live node (or a likely-not-
    // present id) — the SchemaApply on `()` is a no-op but the
    // SetNodeProperty op still flows through `apply_remote`.
    if rng.gen_bool(0.40) {
        let snap = backend.snapshot();
        if !snap.topology.nodes.is_empty() {
            let idx = rng.gen_range(0..snap.topology.nodes.len());
            let target = snap.topology.nodes[idx].id;
            backend.set_node_property(
                target,
                format!("f{}", rng.gen_range(0..4)),
                rng.gen_range(0..u64::MAX).to_le_bytes().to_vec(),
            );
        }
    }
    if rng.gen_bool(0.06) {
        let snap = backend.snapshot();
        if snap.topology.nodes.len() >= 2 {
            let i = rng.gen_range(0..snap.topology.nodes.len());
            let j = rng.gen_range(0..snap.topology.nodes.len());
            if i != j {
                backend.add_edge(snap.topology.nodes[i].id, snap.topology.nodes[j].id);
            }
        }
    }
    if rng.gen_bool(0.04) {
        let snap = backend.snapshot();
        if !snap.topology.nodes.is_empty() {
            let idx = rng.gen_range(0..snap.topology.nodes.len());
            backend.remove_node(snap.topology.nodes[idx].id);
        }
    }
}
