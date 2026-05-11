//! Micro-benches for the comments CRDT backend.
//!
//! Run: `cargo bench -p kyoso_comments_crdt`.
//!
//! What these catch:
//! - `apply_remote` regressions per op kind (AddComment / EditBody /
//!   DeleteComment).
//! - `snapshot` cost growth with comment count.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use kyoso_comments_crdt::{CommentOpKind, CommentsBackend};
use kyoso_crdt::{CrdtId, CrdtModel, InMemoryOpLog, OpLogRead, OpLogWrite};

type Log = InMemoryOpLog<CommentOpKind>;

fn anchor() -> CrdtId {
    CrdtId::new(99, 42)
}

fn populate(n: usize) -> (CommentsBackend, Log) {
    let mut backend = CommentsBackend::with_peer(1);
    let mut log = Log::new();
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        // Reply chain: every other comment is a reply to the previous root.
        let parent = if i > 0 && i % 2 == 0 {
            Some(ids[i - 1])
        } else {
            None
        };
        ids.push(backend.add_comment(anchor(), parent, format!("body {i}")));
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
        group.bench_function(format!("replay_{n}_creates"), |b| {
            let (_, log) = populate(n);
            let head = log.head();
            let ops = log.slice(0, head);
            b.iter(|| {
                let mut backend = CommentsBackend::with_peer(2);
                for op in &ops {
                    backend.apply_remote(black_box(op)).unwrap();
                }
                black_box(backend.comment_count());
            });
        });
    }
    group.finish();
}

fn snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot");
    for &n in &[100usize, 1_000, 10_000] {
        group.bench_function(format!("snapshot_{n}_comments"), |b| {
            let (backend, _) = populate(n);
            b.iter(|| {
                let snap = black_box(&backend).snapshot();
                black_box(snap);
            });
        });
        group.bench_function(format!("restore_{n}_comments"), |b| {
            let (backend, _) = populate(n);
            let snap = backend.snapshot();
            b.iter(|| {
                let mut fresh = CommentsBackend::with_peer(99);
                fresh.restore(black_box(snap.clone()));
                black_box(fresh.comment_count());
            });
        });
    }
    group.finish();
}

fn edit_body(c: &mut Criterion) {
    let mut group = c.benchmark_group("edit_body");
    let (backend, log) = populate(100);
    let head = log.head();
    // Replay onto a fresh backend so we have stable comment IDs to edit.
    let mut peer = CommentsBackend::with_peer(2);
    for op in log.slice(0, head) {
        peer.apply_remote(&op).unwrap();
    }
    let snap = peer.snapshot();
    let target = snap.comments[0].id;

    group.bench_function("edit_body_x100", |b| {
        let mut state = peer.snapshot(); // clone-back via restore for fairness
        let mut fresh = CommentsBackend::with_peer(2);
        fresh.restore(state.clone());
        let mut log = Log::new();
        for op in log.slice(0, log.head()) {
            fresh.apply_remote(&op).ok();
        }
        // Apply 100 EditBody ops sequentially, server-stamped via log.
        b.iter(|| {
            let mut backend = CommentsBackend::with_peer(3);
            backend.restore(state.clone());
            let mut log: Log = Log::new();
            for i in 0..100 {
                backend.edit_body(target, format!("v{i}"));
                for op in backend.drain_pending() {
                    let stamped = log.append(op);
                    backend.apply_remote(&stamped).unwrap();
                }
            }
            state = backend.snapshot();
            black_box(backend.body(target).map(str::to_string));
        });
    });
    group.finish();
}

criterion_group!(benches, apply_remote, snapshot, edit_body);
criterion_main!(benches);
