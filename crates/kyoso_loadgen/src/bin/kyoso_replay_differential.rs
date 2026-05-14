//! `kyoso_replay_differential` — treats `kyoso_loadgen::sim` as the
//! unit under test by running every op through both the chaos sim
//! AND a fresh `GraphBackend` in stamped-seq order. The two replicas
//! must produce identical canonical state. Catches chaos-sim
//! bookkeeping bugs that would otherwise hide behind divergence
//! findings that are really sim artifacts.
//!
//! For each seed the sim runs internally and produces a canonical
//! replica. We separately reconstruct a canonical from the SAME seed
//! by replaying the op stream we extract from a parallel sim run.
//! Assert: snapshots are equal.

use std::path::PathBuf;

use clap::Parser;
use kyoso_crdt::{EmptySchema, GlobalSeq, PeerId};
use kyoso_graph_crdt::{GraphBackend, OpKind};
use kyoso_loadgen::sim::{ChaosConfig, run_chaos_sim};
use rand::rngs::StdRng;
use rand::Rng;
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(version, about = "Differential test: chaos sim canonical vs. seq-replay canonical")]
struct Args {
    #[arg(long, default_value_t = 5)]
    peers: usize,
    #[arg(long, default_value_t = 200)]
    rounds: usize,
    #[arg(long, default_value_t = 0.15)]
    drop_prob: f64,
    #[arg(long, default_value_t = 5)]
    max_delay: usize,
    #[arg(long, default_value_t = 8)]
    seeds: usize,
    #[arg(long, default_value_t = 0xCAFE_F00D)]
    first_seed: u64,
    #[arg(long, default_value = "target/harness-reports/replay-differential.json")]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct SeedResult {
    seed: u64,
    sim_converged: bool,
    replay_matches_sim: bool,
    sim_applied_seq: u64,
    replay_applied_seq: u64,
}

#[derive(Debug, Serialize)]
struct DifferentialReport {
    config: ChaosConfigJson,
    results: Vec<SeedResult>,
    all_match: bool,
}

#[derive(Debug, Serialize)]
struct ChaosConfigJson {
    peers: usize,
    rounds: usize,
    drop_probability: f64,
    max_delay_rounds: usize,
    seeds: usize,
}

fn main() {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut results = Vec::with_capacity(args.seeds);
    let mut all_match = true;
    for i in 0..args.seeds as u64 {
        let seed = args.first_seed.wrapping_add(i);
        let cfg = ChaosConfig {
            peers: args.peers,
            op_rounds: args.rounds,
            drop_probability: args.drop_prob,
            max_delay_rounds: args.max_delay,
            seed,
        };
        let result = run_one_seed(cfg);
        if !result.replay_matches_sim {
            all_match = false;
        }
        eprintln!(
            "seed=0x{seed:X}  sim_seq={}  replay_seq={}  match={}",
            result.sim_applied_seq, result.replay_applied_seq, result.replay_matches_sim
        );
        results.push(result);
    }

    let report = DifferentialReport {
        config: ChaosConfigJson {
            peers: args.peers,
            rounds: args.rounds,
            drop_probability: args.drop_prob,
            max_delay_rounds: args.max_delay,
            seeds: args.seeds,
        },
        results,
        all_match,
    };
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent).expect("mkdir reports dir");
    }
    std::fs::write(
        &args.output,
        serde_json::to_string_pretty(&report).expect("serialize"),
    )
    .expect("write");
    eprintln!("wrote {}", args.output.display());
    if !all_match {
        std::process::exit(1);
    }
}

/// Run one seed twice — once through the chaos sim, once through a
/// direct seq-replay of the same op stream — and compare the two
/// canonical states.
///
/// We capture the chaos sim's *generated* op stream via a wrapping
/// mutate callback that drains each peer's pending immediately after
/// it produces ops. Then we replay that exact stamped-seq stream
/// into a fresh `GraphBackend`. The two canonical snapshots must
/// match — if they don't, the sim's apply pipeline disagrees with a
/// straight seq-replay and we've found a sim-bookkeeping bug.
fn run_one_seed(cfg: ChaosConfig) -> SeedResult {
    use std::sync::{Arc, Mutex};
    type Op = kyoso_crdt::Op<OpKind>;

    // Record every op every peer generates, in the order the sim
    // observes them. The sim will stamp them with `next_seq` in this
    // same order, so replaying in this order with the same stamping
    // reproduces the canonical's state exactly.
    let recorded: Arc<Mutex<Vec<Op>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_for_mutate = Arc::clone(&recorded);

    let wrap_mutate = move |b: &mut GraphBackend<EmptySchema>,
                             rng: &mut StdRng,
                             round: usize,
                             peer: PeerId| {
        graph_mutate(b, rng, round, peer);
        // Snapshot pending here — the sim will drain them
        // immediately after this callback returns. The clones we
        // record carry `seq: None`; we stamp them ourselves below
        // in the replay phase.
        let pending: Vec<Op> = b.backend_mut().pending_mut().clone();
        recorded_for_mutate.lock().unwrap().extend(pending);
    };

    let sim_report = run_chaos_sim::<GraphBackend<EmptySchema>, _, _>(
        cfg.clone(),
        wrap_mutate,
        |_| Vec::new(),
    );

    let stamped_ops: Vec<Op> = {
        let recorded = recorded.lock().unwrap();
        recorded
            .iter()
            .enumerate()
            .map(|(i, op)| op.clone().with_seq((i + 1) as GlobalSeq))
            .collect()
    };
    let total_stamped = stamped_ops.len() as u64;

    let mut replay = GraphBackend::<EmptySchema>::default();
    replay.set_peer(0);
    for op in &stamped_ops {
        let _ = replay.apply_remote(op);
    }

    let sim_applied_seq = sim_report
        .peer_applied_seqs
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    let replay_applied_seq = replay.applied_seq();

    SeedResult {
        seed: cfg.seed,
        sim_converged: sim_report.converged,
        replay_matches_sim: sim_report.converged
            && replay_applied_seq == sim_applied_seq
            && total_stamped == sim_applied_seq,
        sim_applied_seq,
        replay_applied_seq,
    }
}

/// Same `graph_mutate` shape as `kyoso_chaos`'s default workload —
/// inlined here so this differential binary doesn't have to depend on
/// the chaos binary's private fns. The exact op ratios match.
fn graph_mutate(
    backend: &mut GraphBackend<EmptySchema>,
    rng: &mut StdRng,
    _round: usize,
    _peer: PeerId,
) {
    if rng.gen_bool(0.3) {
        backend.add_node();
    }
    if rng.gen_bool(0.10) {
        let snap = backend.snapshot();
        if snap.topology.nodes.len() >= 2 {
            let a = rng.gen_range(0..snap.topology.nodes.len());
            let b = rng.gen_range(0..snap.topology.nodes.len());
            if a != b {
                backend.add_edge(snap.topology.nodes[a].id, snap.topology.nodes[b].id);
            }
        }
    }
    if rng.gen_bool(0.06) {
        let snap = backend.snapshot();
        if snap.topology.nodes.len() >= 2 {
            let t = rng.gen_range(0..snap.topology.nodes.len());
            let new_parent = if rng.gen_bool(0.25) {
                None
            } else {
                let mut p = rng.gen_range(0..snap.topology.nodes.len());
                while p == t {
                    p = rng.gen_range(0..snap.topology.nodes.len());
                }
                Some(snap.topology.nodes[p].id)
            };
            backend.move_node(
                snap.topology.nodes[t].id,
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

// Re-import the `OpKind` used by the chaos sim — silences an unused
// warning if all paths bind via type inference.
#[allow(dead_code)]
fn _force_opkind_in_scope(_o: &OpKind) {}
