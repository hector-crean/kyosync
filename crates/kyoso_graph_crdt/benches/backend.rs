//! Micro-benches for the graph CRDT backend.
//!
//! Run: `cargo bench -p kyoso_graph_crdt`.
//!
//! What these catch:
//! - `apply_remote` regressions per op kind. The `RemoveNode` cascade
//!   walks every edge, so this is the one that gets accidentally
//!   quadratic when refactoring.
//! - `snapshot` cost growth with backend size. Used by the snapshot
//!   scheduler — if it gets slow, the scheduler blocks live submits
//!   on the mirror's mutex.
//! - `would_create_cycle` cost on tall trees.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use kyoso_crdt::{EmptySchema, InMemoryOpLog, OpLogRead, OpLogWrite};
use kyoso_graph_crdt::{GraphBackend, OpKind};

type Backend = GraphBackend<EmptySchema>;
type Log = InMemoryOpLog<OpKind>;

fn populate(n: usize) -> (Backend, Log) {
    // Build a mirror with `n` nodes + edges into both a backend and a
    // log. Each call to `populate` is its own seed — so a benchmark
    // can clone-then-apply or replay-from-log as needed.
    let mut backend = Backend::with_peer(1);
    let mut log = Log::new();
    let mut node_ids = Vec::with_capacity(n);
    for _ in 0..n {
        node_ids.push(backend.add_node());
    }
    // Half the nodes get an outbound edge to the next.
    for w in node_ids.windows(2).step_by(2) {
        let _ = backend.add_edge(w[0], w[1]);
    }
    for op in backend.drain_pending() {
        let stamped = log.append(op);
        backend.apply_remote(&stamped).unwrap();
    }
    (backend, log)
}

fn apply_remote(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_remote");
    for &n in &[100usize, 1_000, 10_000] {
        group.bench_function(format!("replay_{n}_ops_into_fresh_backend"), |b| {
            // Build the log once; bench iterates a fresh backend
            // applying every op.
            let (_, log) = populate(n);
            let head = log.head();
            let ops = log.slice(0, head);
            b.iter(|| {
                let mut backend = Backend::with_peer(2);
                for op in &ops {
                    backend.apply_remote(black_box(op)).unwrap();
                }
                black_box(backend.node_count());
            });
        });
    }
    group.finish();
}

fn snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot");
    for &n in &[100usize, 1_000, 10_000] {
        group.bench_function(format!("snapshot_{n}_nodes"), |b| {
            let (backend, _) = populate(n);
            b.iter(|| {
                let snap = black_box(&backend).snapshot();
                black_box(snap);
            });
        });
        group.bench_function(format!("restore_{n}_nodes"), |b| {
            let (backend, _) = populate(n);
            let snap = backend.snapshot();
            b.iter(|| {
                let mut fresh = Backend::with_peer(99);
                fresh.restore(black_box(snap.clone()));
                black_box(fresh.node_count());
            });
        });
    }
    group.finish();
}

fn move_op_cycle_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_cycle_check");
    // Build a deep chain n1→n2→...→nN, then attempt a Move that would
    // form a cycle. The cycle check walks the whole chain.
    for &depth in &[10usize, 100, 1_000] {
        group.bench_function(format!("cycle_check_depth_{depth}"), |b| {
            let mut backend = Backend::with_peer(1);
            let mut nodes = Vec::with_capacity(depth);
            for _ in 0..depth {
                nodes.push(backend.add_node());
            }
            for i in 1..depth {
                backend.move_node(nodes[i], Some(nodes[i - 1]), format!("p{i}"));
            }
            // Drain to apply locally so tree_parent reads are valid.
            for op in backend.drain_pending() {
                let stamped = op.with_seq(1);
                let _ = backend.apply_remote(&stamped);
            }
            b.iter(|| {
                // Trying to make root a child of leaf would cycle —
                // returns false without mutating.
                let attempted = backend.move_node(
                    nodes[0],
                    Some(*black_box(&nodes[depth - 1])),
                    "x".into(),
                );
                black_box(attempted);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, apply_remote, snapshot, move_op_cycle_check);
criterion_main!(benches);
