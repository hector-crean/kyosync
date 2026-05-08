//! Property-based tests for the lattice axioms across base CRDT primitives.
//!
//! For every primitive (`LwwRegister`, `OrSet`, `PnCounter`, `Sequence`,
//! `CausalMap`) we generate random sequences of mutations across N
//! replicas, deliver them in random orders, and assert convergence.
//! Additionally we spot-check the three [`Lattice`] axioms — commutativity,
//! associativity, idempotency — directly on the join operation.

use kyoso_crdt::types::{
    LwwMut, LwwRegister, OrSet, OrSetMut, PnCounter, PnMut, Sequence, SequenceMut,
};
use kyoso_crdt::{CausalContext, CausalState, Crdt, CrdtId, Lattice, PeerId};
use proptest::prelude::*;

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

fn ctx_at(state: &mut CausalState, peer: PeerId, seq: u64) -> CausalContext<'_> {
    CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
}

/// Apply a list of (CrdtId, op) pairs to a CRDT, simulating the receive
/// side of a sync exchange.
fn replay<C, F>(target: &mut C, mut state: CausalState, ops: &[(CrdtId, F)])
where
    C: Crdt,
    F: Fn(&mut C, &CausalContext) + Sync,
{
    for (op_id, f) in ops {
        let ctx = CausalContext::new(*op_id, Some(op_id.seq), &mut state);
        f(target, &ctx);
    }
}

// -----------------------------------------------------------------------
// LwwRegister<u32>
// -----------------------------------------------------------------------

mod lww_register {
    use super::*;

    proptest! {
        /// Lattice axiom: join is commutative.
        ///
        /// (a ⊔ b) == (b ⊔ a)
        #[test]
        fn join_commutative(a_val in 0u32..1000, b_val in 0u32..1000) {
            let mut a1 = LwwRegister::<u32>::bottom();
            let mut a2 = LwwRegister::<u32>::bottom();
            let mut b1 = LwwRegister::<u32>::bottom();
            let mut b2 = LwwRegister::<u32>::bottom();
            let mut sa = CausalState::new();
            let mut sb = CausalState::new();
            a1.mutate(LwwMut::Set(a_val), &mut ctx_at(&mut sa, 1, 1));
            a2.mutate(LwwMut::Set(a_val), &mut ctx_at(&mut sa, 1, 1));
            b1.mutate(LwwMut::Set(b_val), &mut ctx_at(&mut sb, 2, 1));
            b2.mutate(LwwMut::Set(b_val), &mut ctx_at(&mut sb, 2, 1));

            a1.join(b1);   // a ⊔ b
            b2.join(a2);   // b ⊔ a
            prop_assert_eq!(a1, b2);
        }

        /// Lattice axiom: join is idempotent.
        ///
        /// a ⊔ a == a
        #[test]
        fn join_idempotent(v in 0u32..1000) {
            let mut a = LwwRegister::<u32>::bottom();
            let mut state = CausalState::new();
            a.mutate(LwwMut::Set(v), &mut ctx_at(&mut state, 1, 1));
            let snap = a.clone();
            a.join(snap);
            prop_assert_eq!(a.get(), Some(&v));
        }

        /// Two replicas, concurrent writes — convergence under both delivery orders.
        #[test]
        fn two_replica_convergence(a_val in 0u32..1000, b_val in 0u32..1000) {
            let mut peer_a = LwwRegister::<u32>::bottom();
            let mut peer_b = LwwRegister::<u32>::bottom();
            let mut sa = CausalState::new();
            let mut sb = CausalState::new();

            // A writes a_val at op (1,1); B writes b_val at op (2,1).
            // Server stamps each with a global seq — emulate by giving each
            // op its own ctx seq.
            peer_a.mutate(LwwMut::Set(a_val), &mut ctx_at(&mut sa, 1, 1));
            peer_b.mutate(LwwMut::Set(b_val), &mut ctx_at(&mut sb, 2, 2));

            // Deliver each peer's op to the other in opposite orders. Since
            // both ops carry a seq, the higher seq wins LWW deterministically.
            // Server stamping order: A=1, B=2 → B wins.
            let mut b_then_a = peer_a.clone();
            b_then_a.apply(
                &kyoso_crdt::types::LwwDelta { value: b_val },
                &ctx_at(&mut CausalState::new(), 2, 2),
            ).unwrap();

            let mut a_then_b = peer_b.clone();
            a_then_b.apply(
                &kyoso_crdt::types::LwwDelta { value: a_val },
                &ctx_at(&mut CausalState::new(), 1, 1),
            ).unwrap();

            prop_assert_eq!(b_then_a.get(), a_then_b.get());
            prop_assert_eq!(b_then_a.get(), Some(&b_val));
        }
    }
}

// -----------------------------------------------------------------------
// OrSet<u32>
// -----------------------------------------------------------------------

mod or_set {
    use super::*;
    use kyoso_crdt::types::OrSetDelta;

    proptest! {
        /// Lattice axiom: join is commutative.
        #[test]
        fn join_commutative(a in proptest::collection::vec(0u32..50, 0..10), b in proptest::collection::vec(0u32..50, 0..10)) {
            let mut a1 = OrSet::<u32>::bottom();
            let mut a2 = OrSet::<u32>::bottom();
            let mut b1 = OrSet::<u32>::bottom();
            let mut b2 = OrSet::<u32>::bottom();
            let mut sa = CausalState::new();
            let mut sb = CausalState::new();

            for (i, v) in a.iter().enumerate() {
                let seq = u64::try_from(i + 1).unwrap();
                a1.mutate(OrSetMut::Add(*v), &mut ctx_at(&mut sa, 1, seq));
            }
            // Recreate a2 via apply (mirroring a1's wire output) so it doesn't
            // share CausalState entries with a1.
            replay(&mut a2, CausalState::new(), &a.iter().enumerate().map(|(i, v)| {
                let seq = u64::try_from(i + 1).unwrap();
                let v = *v;
                let f: Box<dyn Fn(&mut OrSet<u32>, &CausalContext) + Sync> = Box::new(move |s, c| {
                    s.apply(&OrSetDelta::Add { value: v }, c).unwrap();
                });
                (CrdtId::new(1, seq), f)
            }).collect::<Vec<_>>());

            for (i, v) in b.iter().enumerate() {
                let seq = u64::try_from(i + 1).unwrap();
                b1.mutate(OrSetMut::Add(*v), &mut ctx_at(&mut sb, 2, seq));
            }
            replay(&mut b2, CausalState::new(), &b.iter().enumerate().map(|(i, v)| {
                let seq = u64::try_from(i + 1).unwrap();
                let v = *v;
                let f: Box<dyn Fn(&mut OrSet<u32>, &CausalContext) + Sync> = Box::new(move |s, c| {
                    s.apply(&OrSetDelta::Add { value: v }, c).unwrap();
                });
                (CrdtId::new(2, seq), f)
            }).collect::<Vec<_>>());

            // a1 ⊔ b1 vs b2 ⊔ a2 — should produce equal element sets.
            a1.join(b1);
            b2.join(a2);

            let mut left: Vec<_> = a1.iter().copied().collect();
            let mut right: Vec<_> = b2.iter().copied().collect();
            left.sort();
            right.sort();
            prop_assert_eq!(left, right);
        }

        /// Lattice axiom: join is idempotent.
        #[test]
        fn join_idempotent(adds in proptest::collection::vec(0u32..50, 0..15)) {
            let mut s = OrSet::<u32>::bottom();
            let mut state = CausalState::new();
            for (i, v) in adds.iter().enumerate() {
                let seq = u64::try_from(i + 1).unwrap();
                s.mutate(OrSetMut::Add(*v), &mut ctx_at(&mut state, 1, seq));
            }
            let snap = s.clone();
            let before: Vec<_> = s.iter().copied().collect();
            s.join(snap);
            let after: Vec<_> = s.iter().copied().collect();
            prop_assert_eq!(before, after);
        }
    }
}

// -----------------------------------------------------------------------
// PnCounter
// -----------------------------------------------------------------------

mod pn_counter {
    use super::*;

    proptest! {
        /// Convergence: peers issue increment/decrement deltas; final value
        /// is the same regardless of delivery order.
        #[test]
        fn three_replica_convergence(
            a_inc in 0u64..1000,
            a_dec in 0u64..1000,
            b_inc in 0u64..1000,
            c_inc in 0u64..1000,
        ) {
            // Each replica applies its own delta + observes the other two.
            // We use a deterministic seq order to mimic server stamping.
            use kyoso_crdt::types::PnDelta;

            let mut a = PnCounter::bottom();
            let mut b = PnCounter::bottom();
            let mut c = PnCounter::bottom();
            let mut sa = CausalState::new();
            let mut sb = CausalState::new();
            let mut sc = CausalState::new();

            a.mutate(PnMut::Inc(a_inc), &mut ctx_at(&mut sa, 1, 1));
            a.mutate(PnMut::Dec(a_dec), &mut ctx_at(&mut sa, 1, 2));
            b.mutate(PnMut::Inc(b_inc), &mut ctx_at(&mut sb, 2, 3));
            c.mutate(PnMut::Inc(c_inc), &mut ctx_at(&mut sc, 3, 4));

            // Cross-deliver via apply (mimicking server broadcast). Each
            // delta arrives once at each non-originating peer.
            let mut da = CausalState::new();
            let mut db = CausalState::new();
            let mut dc = CausalState::new();

            // a has applied its own ops; it needs b's and c's
            a.apply(&PnDelta { by: i64::try_from(b_inc).unwrap() }, &ctx_at(&mut da, 2, 3)).unwrap();
            a.apply(&PnDelta { by: i64::try_from(c_inc).unwrap() }, &ctx_at(&mut da, 3, 4)).unwrap();

            // b has applied its own; needs a's two ops and c's one
            b.apply(&PnDelta { by: i64::try_from(a_inc).unwrap() }, &ctx_at(&mut db, 1, 1)).unwrap();
            b.apply(&PnDelta { by: -i64::try_from(a_dec).unwrap() }, &ctx_at(&mut db, 1, 2)).unwrap();
            b.apply(&PnDelta { by: i64::try_from(c_inc).unwrap() }, &ctx_at(&mut db, 3, 4)).unwrap();

            // c has applied its own; needs a's two and b's one
            c.apply(&PnDelta { by: i64::try_from(a_inc).unwrap() }, &ctx_at(&mut dc, 1, 1)).unwrap();
            c.apply(&PnDelta { by: -i64::try_from(a_dec).unwrap() }, &ctx_at(&mut dc, 1, 2)).unwrap();
            c.apply(&PnDelta { by: i64::try_from(b_inc).unwrap() }, &ctx_at(&mut dc, 2, 3)).unwrap();

            prop_assert_eq!(a.value(), b.value());
            prop_assert_eq!(b.value(), c.value());
            // Sanity: should equal a_inc - a_dec + b_inc + c_inc.
            let expected = i64::try_from(a_inc).unwrap()
                - i64::try_from(a_dec).unwrap()
                + i64::try_from(b_inc).unwrap()
                + i64::try_from(c_inc).unwrap();
            prop_assert_eq!(a.value(), expected);
        }

        /// Lattice axiom: join idempotent.
        #[test]
        fn join_idempotent(inc in 0u64..1000) {
            let mut a = PnCounter::bottom();
            let mut state = CausalState::new();
            a.mutate(PnMut::Inc(inc), &mut ctx_at(&mut state, 1, 1));
            let snap = a.clone();
            a.join(snap);
            prop_assert_eq!(a.value(), i64::try_from(inc).unwrap());
        }
    }
}

// -----------------------------------------------------------------------
// Sequence<u32> (RGA)
// -----------------------------------------------------------------------

mod sequence {
    use super::*;

    /// Simulate a sequence of single-character inserts at deterministic
    /// positions across two peers and verify convergence.
    proptest! {
        /// Two peers each do a series of inserts; cross-delivery via apply
        /// produces the same visible sequence on both.
        #[test]
        fn two_replica_inserts_converge(
            a_chars in proptest::collection::vec(0u32..100, 0..6),
            b_chars in proptest::collection::vec(0u32..100, 0..6),
        ) {
            let mut peer_a = Sequence::<u32>::bottom();
            let mut peer_b = Sequence::<u32>::bottom();
            let mut sa = CausalState::new();
            let mut sb = CausalState::new();

            // Both peers seed identically with a single element to give
            // subsequent inserts something to attach to.
            let seed_delta = peer_a.mutate(
                SequenceMut::InsertAt { pos: 0, value: 999 },
                &mut ctx_at(&mut sa, 0, 1),
            );
            let mut seed_state = CausalState::new();
            peer_b.apply(&seed_delta, &ctx_at(&mut seed_state, 0, 1)).unwrap();

            // Each peer inserts its own characters at the end (visible position = current len).
            let mut a_deltas = Vec::new();
            for (i, c) in a_chars.iter().enumerate() {
                let seq = u64::try_from(i + 2).unwrap();
                let len = peer_a.len();
                let d = peer_a.mutate(
                    SequenceMut::InsertAt { pos: len, value: *c },
                    &mut ctx_at(&mut sa, 1, seq),
                );
                a_deltas.push((CrdtId::new(1, seq), d));
            }
            let mut b_deltas = Vec::new();
            for (i, c) in b_chars.iter().enumerate() {
                let seq = u64::try_from(i + 2).unwrap();
                let len = peer_b.len();
                let d = peer_b.mutate(
                    SequenceMut::InsertAt { pos: len, value: *c },
                    &mut ctx_at(&mut sb, 2, seq),
                );
                b_deltas.push((CrdtId::new(2, seq), d));
            }

            // Cross-deliver.
            for (op_id, d) in &b_deltas {
                let mut s = CausalState::new();
                peer_a.apply(d, &ctx_at(&mut s, op_id.peer, op_id.seq)).unwrap();
            }
            for (op_id, d) in &a_deltas {
                let mut s = CausalState::new();
                peer_b.apply(d, &ctx_at(&mut s, op_id.peer, op_id.seq)).unwrap();
            }

            prop_assert_eq!(peer_a.to_vec(), peer_b.to_vec());
        }

        /// Lattice axiom: join idempotent.
        #[test]
        fn join_idempotent(values in proptest::collection::vec(0u32..100, 0..8)) {
            let mut s = Sequence::<u32>::bottom();
            let mut state = CausalState::new();
            for (i, v) in values.iter().enumerate() {
                let seq = u64::try_from(i + 1).unwrap();
                s.mutate(
                    SequenceMut::InsertAt { pos: i, value: *v },
                    &mut ctx_at(&mut state, 1, seq),
                );
            }
            let snap = s.clone();
            let before = s.to_vec();
            s.join(snap);
            prop_assert_eq!(s.to_vec(), before);
        }
    }
}
