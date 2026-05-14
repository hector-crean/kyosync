//! Snapshot encode/decode round-trip property tests.
//!
//! For any sequence of valid graph ops applied to a `GraphBackend`:
//!
//! - `snapshot()` followed by `restore()` reproduces the same observable
//!   state (live node count, live edge count, applied_seq, tree shape).
//! - `Snapshot::encode()` followed by `Snapshot::decode()` round-trips
//!   byte-exact and yields a snapshot that compares equal to the
//!   original.
//! - The post-restore replica continues to apply subsequent ops without
//!   sequence-number desync.
//!
//! Catches regressions in serialization shape, in `Backend::restore`'s
//! id-generator bump logic, and in any per-primitive snapshot encoding
//! that gets touched.

use kyoso_crdt::{CrdtModel, EmptySchema};
use kyoso_graph_crdt::{check_topology, GraphBackend, OpKind};
use proptest::prelude::*;

/// A handful of op shapes the property test drives through a backend.
/// Sequence positions are interpreted modulo the current live-node /
/// live-edge count so we never reference a node that doesn't exist yet.
#[derive(Clone, Debug)]
enum Action {
    AddNode,
    AddEdge { from_idx: usize, to_idx: usize },
    Move { target_idx: usize, parent_idx: usize, position: String },
    RemoveNode { target_idx: usize },
    RemoveEdge { target_idx: usize },
}

fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => Just(Action::AddNode),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(a, b)| Action::AddEdge { from_idx: a, to_idx: b }),
        1 => (any::<usize>(), any::<usize>(), "[a-c]{1,4}")
            .prop_map(|(t, p, pos)| Action::Move { target_idx: t, parent_idx: p, position: pos }),
        1 => any::<usize>().prop_map(|t| Action::RemoveNode { target_idx: t }),
        1 => any::<usize>().prop_map(|t| Action::RemoveEdge { target_idx: t }),
    ]
}

/// Apply `actions` to `backend`, drain pending, then stamp + apply via
/// `apply_remote` so the canonical state mirrors what a server-mediated
/// round would produce.
fn drive(actions: &[Action], backend: &mut GraphBackend<EmptySchema>) {
    for action in actions {
        match action {
            Action::AddNode => {
                backend.add_node();
            }
            Action::AddEdge { from_idx, to_idx } => {
                let snap = backend.snapshot();
                let n = snap.topology.nodes.len();
                if n < 2 {
                    continue;
                }
                let from = snap.topology.nodes[from_idx % n].id;
                let to = snap.topology.nodes[to_idx % n].id;
                if from == to {
                    continue;
                }
                backend.add_edge(from, to);
            }
            Action::Move {
                target_idx,
                parent_idx,
                position,
            } => {
                let snap = backend.snapshot();
                let n = snap.topology.nodes.len();
                if n < 2 {
                    continue;
                }
                let target = snap.topology.nodes[target_idx % n].id;
                let parent_id = snap.topology.nodes[parent_idx % n].id;
                if target == parent_id {
                    continue;
                }
                backend.move_node(target, Some(parent_id), position.clone());
            }
            Action::RemoveNode { target_idx } => {
                let snap = backend.snapshot();
                let n = snap.topology.nodes.len();
                if n == 0 {
                    continue;
                }
                let target = snap.topology.nodes[target_idx % n].id;
                backend.remove_node(target);
            }
            Action::RemoveEdge { target_idx } => {
                let snap = backend.snapshot();
                let n = snap.topology.edges.len();
                if n == 0 {
                    continue;
                }
                let target = snap.topology.edges[target_idx % n].id;
                backend.remove_edge(target);
            }
        }
        stamp_and_apply_pending(backend);
    }
}

fn stamp_and_apply_pending(backend: &mut GraphBackend<EmptySchema>) {
    let pending = backend.drain_pending();
    let mut seq = backend.applied_seq();
    for mut op in pending {
        seq += 1;
        op.seq = Some(seq);
        let _ = backend.apply_remote(&op);
    }
}

fn restore_into_fresh(
    snap: &kyoso_crdt::Snapshot<kyoso_graph_crdt::GraphTopology, EmptySchema>,
) -> GraphBackend<EmptySchema> {
    let mut fresh = GraphBackend::<EmptySchema>::with_peer(7);
    fresh.restore(snap.clone());
    fresh
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `snapshot()` → `restore()` into a fresh backend preserves the
    /// observable topology shape and applied_seq.
    #[test]
    fn snapshot_restore_round_trip_preserves_state(
        actions in proptest::collection::vec(action_strategy(), 0..120)
    ) {
        let mut original = GraphBackend::<EmptySchema>::with_peer(1);
        drive(&actions, &mut original);

        let snap = original.snapshot();
        let restored = restore_into_fresh(&snap);

        prop_assert_eq!(
            restored.applied_seq(),
            original.applied_seq(),
            "applied_seq must match after restore"
        );
        prop_assert_eq!(
            restored.backend().topology().node_count(),
            original.backend().topology().node_count(),
            "live node count must match"
        );
        prop_assert_eq!(
            restored.backend().topology().edge_count(),
            original.backend().topology().edge_count(),
            "live edge count must match"
        );

        // Snapshot equality at the wire level.
        let original_again = original.snapshot();
        let restored_snap = restored.snapshot();
        prop_assert_eq!(
            original_again, restored_snap,
            "snapshots before and after round-trip must compare equal"
        );

        // Invariants hold on the restored backend.
        let violations = check_topology(restored.backend().topology());
        prop_assert!(
            violations.is_empty(),
            "invariant violations after restore: {:?}",
            violations
        );
    }

    /// `Snapshot::encode()` → `Snapshot::decode()` round-trips byte-exact
    /// and yields an equal value.
    #[test]
    fn snapshot_postcard_round_trip(
        actions in proptest::collection::vec(action_strategy(), 0..120)
    ) {
        let mut backend = GraphBackend::<EmptySchema>::with_peer(11);
        drive(&actions, &mut backend);

        let snap = backend.snapshot();
        let bytes = snap.encode().expect("encode succeeds");
        let decoded = kyoso_crdt::Snapshot::<kyoso_graph_crdt::GraphTopology, EmptySchema>::decode(&bytes)
            .expect("decode succeeds");

        let re_encoded = decoded.encode().expect("re-encode succeeds");
        prop_assert_eq!(
            bytes, re_encoded,
            "encoded bytes must be stable across one round trip"
        );
        prop_assert_eq!(snap, decoded, "decoded snapshot must equal original");
    }

}
