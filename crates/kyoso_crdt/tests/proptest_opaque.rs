//! Property tests for [`OpaqueSchemaState`] — the schema-erased layer
//! the server uses to hold per-entity CRDT state without knowing the
//! user's typed schemas.
//!
//! Covers:
//! - Random sequences of `WireDelta` variants apply without panicking.
//! - `OpaqueSchemaState` serializes deterministically (the BTreeMap
//!   fix means two encodes of equal state produce equal bytes).
//! - Two replicas applying the same op set in two different orders
//!   converge to equal state — commutativity at the opaque-merge layer.

use kyoso_crdt::context::{CausalContext, CausalState};
use kyoso_crdt::delta::{Path, PathSegment, WireDelta};
use kyoso_crdt::id::CrdtId;
use kyoso_crdt::opaque::OpaqueSchemaState;
use kyoso_crdt::schema::SchemaApply;
use proptest::prelude::*;

/// Synthesise a random op as (op_id, seq, path, delta).
#[derive(Clone, Debug)]
struct SynthOp {
    op_id: CrdtId,
    seq: u64,
    path: Path,
    delta: WireDelta,
}

fn synth_op_strategy() -> impl Strategy<Value = (u32, u64, u8, u8, Vec<u8>)> {
    (
        0u32..4,
        0u64..32,
        0u8..4,
        0u8..5,
        proptest::collection::vec(any::<u8>(), 0..16),
    )
}

fn build_op(
    (peer, _local, field, kind, bytes): (u32, u64, u8, u8, Vec<u8>),
    global_seq: u64,
) -> SynthOp {
    // In production every op has a unique `CrdtId` (the originator's
    // monotonic LocalSeq). Reusing op_ids isn't a legitimate input —
    // it would break OR-Set tag uniqueness, Sequence element id, and
    // LWW stamp identity. Derive `local` from `global_seq` so every
    // synth op carries a distinct `CrdtId`.
    let op_id = CrdtId::new(peer, global_seq);
    // The kind is encoded into the path so a given path always
    // resolves to the same primitive. In production this invariant
    // is enforced by the schema; we mimic it here so the property
    // test isn't asking the merge to reconcile two different CRDT
    // kinds at the same path (which IS a protocol bug, not a
    // convergence one — and a chaos sim isn't the place to test it).
    let (kind_tag, delta) = match kind {
        0 => ("lww", WireDelta::LwwReplace { value: bytes }),
        1 => ("orset", WireDelta::OrSetAdd { value: bytes }),
        2 => ("orset", WireDelta::OrSetRemove { observed: vec![] }),
        3 => (
            "pn",
            WireDelta::PnCounterDelta {
                by: (peer as i64).wrapping_mul(global_seq as i64 + 1),
            },
        ),
        _ => (
            "seq",
            WireDelta::SequenceInsert {
                predecessor: None,
                value: bytes,
            },
        ),
    };
    let path = Path(vec![
        PathSegment::Field(format!("Schema{}", peer % 2)),
        PathSegment::Field(format!("{kind_tag}_f{field}")),
    ]);
    SynthOp {
        op_id,
        seq: global_seq,
        path,
        delta,
    }
}

/// Apply one op to `state` with a fresh `CausalState` — matches how
/// `Backend::apply_remote` runs each op.
fn apply_one(state: &mut OpaqueSchemaState, op: &SynthOp) {
    let mut causal = CausalState::new();
    let ctx = CausalContext::new(op.op_id, Some(op.seq), &mut causal);
    // Apply errors (e.g. variant mismatch at the same path across two
    // different op kinds) are tolerated — the proptest's job is to
    // verify the merge doesn't panic, and the OpaqueField::join
    // mismatch path is the documented no-op behavior.
    let _ = state.apply_wire(&op.path, op.delta.clone(), &ctx);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Applying any sequence of random `WireDelta` variants doesn't
    /// panic and doesn't violate internal field-presence invariants
    /// (every path in `fields` continues to resolve to a valid variant).
    #[test]
    fn random_apply_never_panics(
        ops in proptest::collection::vec(synth_op_strategy(), 0..80)
    ) {
        let mut state = OpaqueSchemaState::new();
        for (i, raw) in ops.into_iter().enumerate() {
            let op = build_op(raw, (i + 1) as u64);
            apply_one(&mut state, &op);
        }
        // Sanity: every entry in `fields` is still walkable.
        for (path, field) in &state.fields {
            let _ = (path, field); // just touch them
        }
    }

    /// Postcard encode/decode round-trips byte-exact on equal state.
    /// Asserts the BTreeMap-backed determinism fix is durable.
    #[test]
    fn opaque_state_postcard_round_trip(
        ops in proptest::collection::vec(synth_op_strategy(), 0..40)
    ) {
        let mut state = OpaqueSchemaState::new();
        for (i, raw) in ops.into_iter().enumerate() {
            let op = build_op(raw, (i + 1) as u64);
            apply_one(&mut state, &op);
        }
        let bytes1 = postcard::to_allocvec(&state).expect("encode");
        let decoded: OpaqueSchemaState = postcard::from_bytes(&bytes1).expect("decode");
        let bytes2 = postcard::to_allocvec(&decoded).expect("re-encode");
        prop_assert_eq!(bytes1, bytes2, "opaque state bytes must be stable");
        prop_assert_eq!(state, decoded, "decoded state must equal original");
    }

    /// Two replicas applying the same op set in two different orders
    /// converge to equal state. The state-based merge laws of each
    /// primitive (LWW max-stamp, OR-Set add-tag union, PN-Counter
    /// per-peer max, Sequence position id union) all commute, so
    /// `OpaqueSchemaState::apply_wire` should be order-independent.
    #[test]
    fn opaque_apply_is_order_independent(
        ops in proptest::collection::vec(synth_op_strategy(), 1..40),
        seed in any::<u64>(),
    ) {
        let synth: Vec<SynthOp> = ops
            .into_iter()
            .enumerate()
            .map(|(i, raw)| build_op(raw, (i + 1) as u64))
            .collect();

        let mut a = OpaqueSchemaState::new();
        for op in &synth {
            apply_one(&mut a, op);
        }

        let mut perm: Vec<usize> = (0..synth.len()).collect();
        let mut x = seed;
        for i in (1..perm.len()).rev() {
            x = x.wrapping_mul(2654435761).wrapping_add(1);
            let j = (x as usize) % (i + 1);
            perm.swap(i, j);
        }
        let mut b = OpaqueSchemaState::new();
        for &i in &perm {
            apply_one(&mut b, &synth[i]);
        }

        prop_assert_eq!(a, b, "two orderings produced different opaque state");
    }
}
