//! `kyoso_topology_probe` — measure shape-dependent CRDT costs +
//! verify topology invariants on large reference graphs.
//!
//! Builds four reference graphs in a single canonical replica
//! (deep tree, wide tree, dense ref-edge graph, worst-case cycle
//! walk), each isolated in its own backend instance so timings don't
//! contaminate each other. For each:
//!
//! - records build-time per-op apply rate;
//! - times one full `snapshot()` + `restore()` round trip;
//! - times a representative `would_create_cycle` query;
//! - runs the invariants module against the built topology and
//!   includes any violations in the report.
//!
//! Output: `target/harness-reports/topology-probe.json`. Findings:
//! a `topology-probe-shapes` entry per shape with timings + invariant
//! violations.

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use kyoso_crdt::EmptySchema;
use kyoso_graph_crdt::{
    check_topology, cross_check_cycle_detection, GraphBackend, InvariantViolation, OpKind,
};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(version, about = "Topology-shape stress + invariants harness")]
struct Args {
    /// Build a deep tree of this depth (depth=1 == one node).
    #[arg(long, default_value_t = 2000)]
    deep_tree_depth: usize,
    /// Build a wide tree under one root with this many children.
    #[arg(long, default_value_t = 4000)]
    wide_tree_width: usize,
    /// Build a dense ref-edge graph: N nodes, every pair gets an
    /// edge in one direction (so `dense_nodes * (dense_nodes - 1)`
    /// edges). Use small numbers — 50 is already 2,450 edges.
    #[arg(long, default_value_t = 40)]
    dense_nodes: usize,
    /// How many random `would_create_cycle` queries to sample per
    /// shape; capped at the live-node-count squared.
    #[arg(long, default_value_t = 256)]
    cycle_check_sample: usize,
    /// Where to write the JSON report.
    #[arg(long, default_value = "target/harness-reports/topology-probe.json")]
    output: PathBuf,
}

type StructuralBackend = GraphBackend<EmptySchema>;

#[derive(Debug, Serialize)]
struct ShapeReport {
    name: String,
    node_count: usize,
    edge_count: usize,
    build_ops: usize,
    build_us: u128,
    build_ops_per_sec: f64,
    snapshot_us: u128,
    restore_us: u128,
    cycle_check_us: u128,
    cycle_check_queries: usize,
    invariant_violations: Vec<InvariantViolation>,
}

#[derive(Debug, Serialize)]
struct TopologyProbeReport {
    shapes: Vec<ShapeReport>,
    total_invariant_violations: usize,
}

fn main() {
    let args = Args::parse();

    let mut shapes = Vec::new();
    shapes.push(measure_deep_tree(args.deep_tree_depth, args.cycle_check_sample));
    shapes.push(measure_wide_tree(args.wide_tree_width, args.cycle_check_sample));
    shapes.push(measure_dense_edges(args.dense_nodes, args.cycle_check_sample));
    shapes.push(measure_worst_case_cycle_walk(
        args.deep_tree_depth,
        args.cycle_check_sample,
    ));

    let total_invariant_violations: usize =
        shapes.iter().map(|s| s.invariant_violations.len()).sum();

    let report = TopologyProbeReport {
        shapes,
        total_invariant_violations,
    };

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent).expect("mkdir reports dir");
    }
    let json = serde_json::to_string_pretty(&report).expect("serialize");
    std::fs::write(&args.output, json).expect("write report");
    eprintln!("wrote {}", args.output.display());

    // Print a one-line summary for the agent / human.
    for shape in &report.shapes {
        eprintln!(
            "{}: {} nodes, {} edges — build {:.0} ops/s, snap {} µs, restore {} µs, cycle-check {} µs ({} q), violations {}",
            shape.name,
            shape.node_count,
            shape.edge_count,
            shape.build_ops_per_sec,
            shape.snapshot_us,
            shape.restore_us,
            shape.cycle_check_us,
            shape.cycle_check_queries,
            shape.invariant_violations.len(),
        );
    }

    if report.total_invariant_violations > 0 {
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Shape builders — each constructs ops, replays them through a fresh
// canonical replica, then measures snapshot/restore/cycle-check costs.
// ---------------------------------------------------------------------------

fn measure_deep_tree(depth: usize, cycle_sample: usize) -> ShapeReport {
    let mut backend = StructuralBackend::with_peer(99);
    let ops = build_deep_tree_ops(&mut backend, depth);
    finalize_shape(backend, "deep_tree", ops, cycle_sample)
}

fn measure_wide_tree(width: usize, cycle_sample: usize) -> ShapeReport {
    let mut backend = StructuralBackend::with_peer(99);
    let ops = build_wide_tree_ops(&mut backend, width);
    finalize_shape(backend, "wide_tree", ops, cycle_sample)
}

fn measure_dense_edges(n: usize, cycle_sample: usize) -> ShapeReport {
    let mut backend = StructuralBackend::with_peer(99);
    let ops = build_dense_edges_ops(&mut backend, n);
    finalize_shape(backend, "dense_edges", ops, cycle_sample)
}

fn measure_worst_case_cycle_walk(depth: usize, cycle_sample: usize) -> ShapeReport {
    // Same shape as deep_tree but specifically labelled so the
    // cycle-check timing has a meaningful name in the report.
    let mut backend = StructuralBackend::with_peer(99);
    let ops = build_deep_tree_ops(&mut backend, depth);
    finalize_shape(backend, "worst_case_cycle_walk", ops, cycle_sample.max(64))
}

// ---------------------------------------------------------------------------
// Ops generation — each builder enqueues ops via the canonical replica's
// own mutation API, then drains and stamps them so they look exactly
// like what the server would broadcast.
// ---------------------------------------------------------------------------

fn build_deep_tree_ops(backend: &mut StructuralBackend, depth: usize) -> Vec<kyoso_crdt::Op<OpKind>> {
    let mut ids = Vec::with_capacity(depth);
    for _ in 0..depth {
        let id = backend.add_node();
        ids.push(id);
    }
    for window in ids.windows(2) {
        let parent = window[0];
        let child = window[1];
        backend.move_node(child, Some(parent), String::from("a"));
    }
    drain_and_apply(backend)
}

fn build_wide_tree_ops(backend: &mut StructuralBackend, width: usize) -> Vec<kyoso_crdt::Op<OpKind>> {
    let root = backend.add_node();
    let mut children = Vec::with_capacity(width);
    for _ in 0..width {
        children.push(backend.add_node());
    }
    for (i, child) in children.iter().enumerate() {
        // Spread the order keys so the tree isn't degenerate by accident.
        backend.move_node(*child, Some(root), format!("{i:08}"));
    }
    drain_and_apply(backend)
}

fn build_dense_edges_ops(backend: &mut StructuralBackend, n: usize) -> Vec<kyoso_crdt::Op<OpKind>> {
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        ids.push(backend.add_node());
    }
    for (i, from) in ids.iter().enumerate() {
        for (j, to) in ids.iter().enumerate() {
            if i == j {
                continue;
            }
            backend.add_edge(*from, *to);
        }
    }
    drain_and_apply(backend)
}

/// Drain `backend.pending`, stamp seqs, and apply through `apply_remote`
/// — the same path a server-mediated broadcast would take. Returns
/// the stamped ops for further reuse if the caller wants them.
fn drain_and_apply(backend: &mut StructuralBackend) -> Vec<kyoso_crdt::Op<OpKind>> {
    let pending = backend.drain_pending();
    let mut applied_seq = backend.applied_seq();
    let mut out = Vec::with_capacity(pending.len());
    for mut op in pending {
        applied_seq += 1;
        op.seq = Some(applied_seq);
        // The pending ops were produced locally on the same backend —
        // applying them via `apply_remote` simulates the echo path.
        let _ = backend.apply_remote(&op);
        out.push(op);
    }
    out
}

// ---------------------------------------------------------------------------
// Finalize — measure timings on the already-built canonical replica.
// ---------------------------------------------------------------------------

fn finalize_shape(
    backend: StructuralBackend,
    name: &str,
    build_ops: Vec<kyoso_crdt::Op<OpKind>>,
    cycle_sample: usize,
) -> ShapeReport {
    let node_count = backend.backend().topology().node_count();
    let edge_count = backend.backend().topology().edge_count();

    // Re-build a fresh copy to time the apply phase isolated from
    // the topology snapshots above.
    let mut timed = StructuralBackend::with_peer(99);
    let build_start = Instant::now();
    let mut applied_seq = 0u64;
    let build_ops_n = build_ops.len();
    for mut op in build_ops {
        applied_seq += 1;
        op.seq = Some(applied_seq);
        let _ = timed.apply_remote(&op);
    }
    let build_us = build_start.elapsed().as_micros();
    let build_ops_per_sec = if build_us == 0 {
        0.0
    } else {
        build_ops_n as f64 / (build_us as f64 / 1_000_000.0)
    };

    // Snapshot + restore round trip on the already-replayed backend
    // (timed independent of the build phase).
    let snap_start = Instant::now();
    let snap = timed.snapshot();
    let snapshot_us = snap_start.elapsed().as_micros();

    let mut restore_target = StructuralBackend::with_peer(99);
    let restore_start = Instant::now();
    restore_target.restore(snap);
    let restore_us = restore_start.elapsed().as_micros();

    // Cycle-check timing: sample `cycle_sample` random
    // `would_create_cycle` queries against the original backend.
    let topology = timed.backend().topology();
    let live_nodes: Vec<_> = topology.live_node_ids().collect();
    let mut cycle_check_queries = 0usize;
    let cycle_start = Instant::now();
    if !live_nodes.is_empty() {
        // Pseudorandom but deterministic — `wrapping_mul` keeps it
        // dependency-light vs. pulling in `rand` here.
        let mut x: usize = 0x9E37_79B1;
        while cycle_check_queries < cycle_sample {
            let i = x % live_nodes.len();
            x = x.wrapping_mul(2654435761).wrapping_add(1);
            let j = x % live_nodes.len();
            x = x.wrapping_mul(2654435761).wrapping_add(1);
            if i != j {
                let _ = topology.would_create_cycle(live_nodes[i], live_nodes[j]);
                cycle_check_queries += 1;
            } else {
                break; // single-node case
            }
        }
    }
    let cycle_check_us = cycle_start.elapsed().as_micros();

    let mut invariant_violations = check_topology(topology);
    invariant_violations.extend(cross_check_cycle_detection(topology, cycle_sample.min(64)));

    ShapeReport {
        name: name.to_string(),
        node_count,
        edge_count,
        build_ops: build_ops_n,
        build_us,
        build_ops_per_sec,
        snapshot_us,
        restore_us,
        cycle_check_us,
        cycle_check_queries,
        invariant_violations,
    }
}
