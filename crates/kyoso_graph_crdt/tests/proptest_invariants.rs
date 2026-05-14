//! Property test: for any sequence of valid graph ops applied via a
//! server-mediated total order, [`check_topology`] returns no
//! violations on the canonical replica.
//!
//! Complements the chaos sim — chaos drives many seeds against a
//! fixed config; this proptest *shrinks* a failing case to the
//! minimum op sequence that triggers it, giving a tighter repro.

use kyoso_crdt::EmptySchema;
use kyoso_graph_crdt::{check_topology, GraphBackend};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Action {
    AddNode,
    AddEdge { from: usize, to: usize },
    Move { target: usize, parent: usize },
    RemoveNode { idx: usize },
    RemoveEdge { idx: usize },
}

fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => Just(Action::AddNode),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(a, b)| Action::AddEdge { from: a, to: b }),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(t, p)| Action::Move { target: t, parent: p }),
        2 => any::<usize>().prop_map(|t| Action::RemoveNode { idx: t }),
        1 => any::<usize>().prop_map(|t| Action::RemoveEdge { idx: t }),
    ]
}

fn drive(actions: &[Action], backend: &mut GraphBackend<EmptySchema>) {
    for action in actions {
        match action {
            Action::AddNode => {
                backend.add_node();
            }
            Action::AddEdge { from, to } => {
                let snap = backend.snapshot();
                let n = snap.topology.nodes.len();
                if n < 2 {
                    continue;
                }
                let f = snap.topology.nodes[from % n].id;
                let t = snap.topology.nodes[to % n].id;
                if f == t {
                    continue;
                }
                backend.add_edge(f, t);
            }
            Action::Move { target, parent } => {
                let snap = backend.snapshot();
                let n = snap.topology.nodes.len();
                if n < 2 {
                    continue;
                }
                let tgt = snap.topology.nodes[target % n].id;
                let par = snap.topology.nodes[parent % n].id;
                if tgt == par {
                    continue;
                }
                backend.move_node(tgt, Some(par), "p".to_string());
            }
            Action::RemoveNode { idx } => {
                let snap = backend.snapshot();
                let n = snap.topology.nodes.len();
                if n == 0 {
                    continue;
                }
                backend.remove_node(snap.topology.nodes[idx % n].id);
            }
            Action::RemoveEdge { idx } => {
                let snap = backend.snapshot();
                let n = snap.topology.edges.len();
                if n == 0 {
                    continue;
                }
                backend.remove_edge(snap.topology.edges[idx % n].id);
            }
        }
        let pending = backend.drain_pending();
        let mut seq = backend.applied_seq();
        for mut op in pending {
            seq += 1;
            op.seq = Some(seq);
            let _ = backend.apply_remote(&op);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Universal: for any op stream, the canonical replica's topology
    /// has no cycles and no orphan edges. Shrinks a failing case to
    /// the minimum action list.
    #[test]
    fn topology_invariants_hold_for_any_op_stream(
        actions in proptest::collection::vec(action_strategy(), 0..160)
    ) {
        let mut backend = GraphBackend::<EmptySchema>::with_peer(1);
        drive(&actions, &mut backend);
        let violations = check_topology(backend.backend().topology());
        prop_assert!(
            violations.is_empty(),
            "found {} invariant violation(s) after {} actions:\n  - {}",
            violations.len(),
            actions.len(),
            violations
                .iter()
                .take(6)
                .map(|v| v.detail.as_str())
                .collect::<Vec<_>>()
                .join("\n  - ")
        );
    }
}
