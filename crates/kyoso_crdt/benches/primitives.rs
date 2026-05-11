//! Micro-benches for the CRDT primitives + wire encoding.
//!
//! Run: `cargo bench -p kyoso_crdt`. Output:
//! `target/criterion/<group>/<name>/` (HTML report + JSON).
//!
//! What these catch:
//! - Lattice-merge regressions on the primitives. These run on every
//!   apply, so an O(n) → O(n²) regression here matters.
//! - Wire encode/decode cost growth with op size. Useful when changing
//!   how `WireDelta` or snapshot shapes are serialised.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use kyoso_crdt::context::{CausalContext, CausalState};
use kyoso_crdt::types::{LwwDelta, LwwMut, LwwRegister, OrSet, OrSetMut, PnCounter, PnMut};
use kyoso_crdt::{Crdt, CrdtId, Lattice};

fn ctx<'a>(state: &'a mut CausalState, peer: u32, seq: u64) -> CausalContext<'a> {
    CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
}

fn lww_register(c: &mut Criterion) {
    let mut group = c.benchmark_group("lww_register");
    group.bench_function("apply_replace_wins", |b| {
        let mut state = CausalState::new();
        let mut reg = LwwRegister::<u64>::empty();
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            let _ = reg.apply(
                &LwwDelta { value: black_box(seq) },
                &ctx(&mut state, 1, seq),
            );
            black_box(reg.get());
        });
    });
    group.bench_function("mutate_set", |b| {
        let mut state = CausalState::new();
        let mut reg = LwwRegister::<u64>::empty();
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            let _ = reg.mutate(LwwMut::Set(black_box(seq)), &mut ctx(&mut state, 1, seq));
        });
    });
    group.bench_function("join_pointwise", |b| {
        let mut a = LwwRegister::<u64>::empty();
        let mut b_state = CausalState::new();
        let mut other = LwwRegister::<u64>::empty();
        let _ = other.apply(&LwwDelta { value: 7 }, &ctx(&mut b_state, 2, 1));
        b.iter(|| {
            let mut clone = a.clone();
            clone.join(black_box(other.clone()));
            black_box(&clone);
            a = clone;
        });
    });
    group.finish();
}

fn or_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("or_set");
    for &n in &[10u64, 100, 1_000] {
        group.bench_function(format!("add_{n}_then_join"), |b| {
            b.iter(|| {
                let mut state = CausalState::new();
                let mut s = OrSet::<u64>::bottom();
                for i in 0..n {
                    let _ = s.mutate(
                        OrSetMut::Add(black_box(i)),
                        &mut ctx(&mut state, 1, i + 1),
                    );
                }
                let mut peer = OrSet::<u64>::bottom();
                let mut peer_state = CausalState::new();
                for i in 0..n {
                    let _ = peer.mutate(
                        OrSetMut::Add(black_box(n + i)),
                        &mut ctx(&mut peer_state, 2, i + 1),
                    );
                }
                s.join(peer);
                black_box(s);
            });
        });
    }
    group.finish();
}

fn pn_counter(c: &mut Criterion) {
    let mut group = c.benchmark_group("pn_counter");
    group.bench_function("inc_dec_value", |b| {
        let mut state = CausalState::new();
        let mut counter = PnCounter::default();
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            if seq % 2 == 0 {
                let _ = counter.mutate(PnMut::Inc(1), &mut ctx(&mut state, 1, seq));
            } else {
                let _ = counter.mutate(PnMut::Dec(1), &mut ctx(&mut state, 1, seq));
            }
            black_box(counter.value());
        });
    });
    group.finish();
}

fn wire_encode(c: &mut Criterion) {
    use kyoso_crdt::{Diff, Op};
    let mut group = c.benchmark_group("wire_encode");

    // Build representative diffs of varying sizes by reusing
    // LwwDelta wire bytes — model-agnostic so this lives in kyoso_crdt
    // rather than per-model.
    for &n in &[1usize, 10, 100, 1_000] {
        // We use `()` as the K placeholder — the encoded size depends
        // on Op<K>'s envelope, not on K itself, for empty K.
        let ops: Vec<Op<u64>> = (0..n as u64)
            .map(|i| Op::new(CrdtId::new(1, i), i))
            .collect();
        let diff: Diff<u64> = Diff {
            from_seq: 0,
            to_seq: n as u64,
            ops,
        };
        group.bench_function(format!("encode_diff_{n}"), |b| {
            b.iter(|| {
                let bytes = black_box(&diff).encode().unwrap();
                black_box(bytes);
            });
        });
        let bytes = diff.encode().unwrap();
        group.bench_function(format!("decode_diff_{n}"), |b| {
            b.iter(|| {
                let decoded: Diff<u64> = Diff::decode(black_box(&bytes)).unwrap();
                black_box(decoded);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, lww_register, or_set, pn_counter, wire_encode);
criterion_main!(benches);
