//! Integration tests for the graph CRDT sync model.
//!
//! Two replicas (`A` and `B`) share a server-mediated [`InMemoryOpLog`].
//! Mutations on either side flow through the log and apply on the other.
//! These tests pin down: wire round-trips, convergence under interleaved
//! ops, idempotency, and out-of-order detection.

use kyoso_crdt::{
    ApplyError, CrdtId, Diff, EmptySchema, InMemoryOpLog, Op, OpLogRead, OpLogWrite, PeerId,
};
use kyoso_graph_crdt::{EdgeCategory, GraphBackend, OpKind};

type Backend = GraphBackend<EmptySchema>;
type Log = InMemoryOpLog<OpKind>;

/// Send every pending op from `replica` through `log` and broadcast back
/// to **all** replicas — including the originator. Mirrors what a real
/// server does: it echoes confirmed ops to the sender so they learn the
/// assigned [`GlobalSeq`].
fn flush(replica: &mut Backend, log: &mut Log, peers: &mut [&mut Backend]) {
    let pending = replica.drain_pending();
    for op in pending {
        let stamped = log.append(op);
        replica.apply_remote(&stamped).expect("apply (originator)");
        for peer in peers.iter_mut() {
            peer.apply_remote(&stamped).expect("apply (peer)");
        }
    }
}

fn make_replica(peer: PeerId) -> Backend {
    Backend::with_peer(peer)
}

#[test]
fn add_node_round_trip() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let n = a.add_node();
    // Note: node_count() only reflects applied state, not pending ops
    assert_eq!(a.node_count(), 0); // Not yet applied
    assert_eq!(b.node_count(), 0);

    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(a.node_count(), 1);
    assert_eq!(b.node_count(), 1);
    assert_eq!(a.applied_seq(), 1);
    assert_eq!(b.applied_seq(), 1);
    assert_eq!(n, CrdtId::new(1, 0));
}

#[test]
fn add_edge_carries_endpoints() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let n1 = a.add_node();
    let n2 = a.add_node();
    let e = a.add_edge(n1, n2);

    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(b.node_count(), 2);
    assert_eq!(b.edge_count(), 1);
    let outgoing: Vec<_> = b.outgoing_edge_ids(n1).collect();
    assert_eq!(outgoing, vec![e]);
}

#[test]
fn remove_node_cascades_to_edges() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let n1 = a.add_node();
    let n2 = a.add_node();
    let _e = a.add_edge(n1, n2);
    flush(&mut a, &mut log, &mut [&mut b]);
    assert_eq!(b.edge_count(), 1);

    a.remove_node(n1);
    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(a.node_count(), 1);
    assert_eq!(a.edge_count(), 0);
    assert_eq!(b.node_count(), 1);
    assert_eq!(b.edge_count(), 0);
}

#[test]
fn interleaved_inserts_converge() {
    // A and B both insert nodes locally, then sync. Both replicas should
    // end with the same state regardless of who "got there first" because
    // server-stamped GlobalSeq totally orders the log.
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let _na = a.add_node();
    let _nb = b.add_node();

    // Server ingests A's pending first, then B's pending. Both replicas
    // see all ops broadcast back in seq order.
    flush(&mut a, &mut log, &mut [&mut b]);
    flush(&mut b, &mut log, &mut [&mut a]);

    assert_eq!(a.node_count(), 2);
    assert_eq!(b.node_count(), 2);
    assert_eq!(a.applied_seq(), b.applied_seq());
    assert_eq!(log.head(), 2);
}

#[test]
fn apply_remote_is_idempotent() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let _ = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    // Re-apply the only op in the log to b; should be a no-op.
    let op = log.slice(0, 1).remove(0);
    assert!(b.apply_remote(&op).is_ok());
    assert!(b.apply_remote(&op).is_ok());
    assert_eq!(b.node_count(), 1);
    assert_eq!(b.applied_seq(), 1);
}

#[test]
fn out_of_order_apply_rejected() {
    let mut b = make_replica(2);
    // Hand-craft an op claiming to be seq=5 with no predecessors.
    let bad: Op<OpKind> = Op {
        id: CrdtId::new(99, 0),
        seq: Some(5),
        kind: OpKind::AddNode,
    };
    assert_eq!(
        b.apply_remote(&bad),
        Err(ApplyError::OutOfOrder {
            expected: 1,
            got: 5
        })
    );
}

#[test]
fn unconfirmed_apply_rejected() {
    let mut b = make_replica(2);
    let pending: Op<OpKind> = Op {
        id: CrdtId::new(1, 0),
        seq: None,
        kind: OpKind::AddNode,
    };
    assert_eq!(b.apply_remote(&pending), Err(ApplyError::Unconfirmed));
}

#[test]
fn diff_encode_decode_round_trip() {
    let diff: Diff<OpKind> = Diff {
        from_seq: 0,
        to_seq: 3,
        ops: vec![
            Op::new(CrdtId::new(1, 0), OpKind::AddNode).with_seq(1),
            Op::new(CrdtId::new(1, 1), OpKind::AddNode).with_seq(2),
            Op::new(
                CrdtId::new(1, 2),
                OpKind::AddRefEdge {
                    category: EdgeCategory::Reference,
                    from: CrdtId::new(1, 0),
                    to: CrdtId::new(1, 1),
                },
            )
            .with_seq(3),
        ],
    };

    let bytes = diff.encode().expect("encode");
    let decoded: Diff<OpKind> = Diff::decode(&bytes).expect("decode");
    assert_eq!(decoded, diff);

    // Sanity-check that encoding is reasonably compact: 3 ops fit in
    // well under 100 bytes (postcard varint).
    assert!(bytes.len() < 100, "encoded size = {}", bytes.len());
}

#[test]
fn diff_since_returns_unseen_ops() {
    let mut log = Log::new();
    log.append(Op::new(CrdtId::new(1, 0), OpKind::AddNode));
    log.append(Op::new(CrdtId::new(1, 1), OpKind::AddNode));
    log.append(Op::new(CrdtId::new(1, 2), OpKind::AddNode));

    let diff = log.diff_since(1);
    assert_eq!(diff.from_seq, 1);
    assert_eq!(diff.to_seq, 3);
    assert_eq!(diff.ops.len(), 2);
    assert_eq!(diff.ops[0].seq, Some(2));
    assert_eq!(diff.ops[1].seq, Some(3));
}

#[test]
fn snapshot_round_trip() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let n1 = a.add_node();
    let n2 = a.add_node();
    let _e = a.add_edge(n1, n2);
    // n1 is a root; give it an order_key. n2 becomes n1's tree-child.
    assert!(a.move_node(n1, None, "n".into()));
    assert!(a.move_node(n2, Some(n1), "n".into()));
    flush(&mut a, &mut log, &mut [&mut b]);

    let snap = a.snapshot();
    assert_eq!(snap.at_seq, 5);
    assert_eq!(snap.topology.nodes.len(), 2);
    assert_eq!(snap.topology.edges.len(), 1);
    let n1_snap = snap.topology.nodes.iter().find(|n| n.id == n1).unwrap();
    assert_eq!(n1_snap.order_key.as_deref(), Some("n"));
    let n2_snap = snap.topology.nodes.iter().find(|n| n.id == n2).unwrap();
    assert_eq!(n2_snap.tree_parent, Some(n1));

    // Restore on a fresh backend reaches the same state.
    let mut c = Backend::with_peer(99);
    c.restore(snap);
    assert_eq!(c.applied_seq(), 5);
    assert_eq!(c.node_count(), 2);
    assert_eq!(c.edge_count(), 1);
}

#[test]
fn snapshot_excludes_tombstones() {
    let mut a = make_replica(1);
    let mut log = Log::new();
    let n1 = a.add_node();
    let n2 = a.add_node();
    let _e = a.add_edge(n1, n2);
    a.remove_node(n1);
    flush(&mut a, &mut log, &mut []);

    let snap = a.snapshot();
    // Live: just n2 (n1 tombstoned, edge cascade-tombstoned).
    assert_eq!(snap.topology.nodes.len(), 1);
    assert_eq!(snap.topology.nodes[0].id, n2);
    assert_eq!(snap.topology.edges.len(), 0);
}

#[test]
fn restore_bumps_id_generator_past_my_ids() {
    let mut a = Backend::with_peer(7);
    let mut log = Log::new();
    let _n1 = a.add_node();
    let _n2 = a.add_node();
    flush(&mut a, &mut log, &mut []);
    let snap = a.snapshot();

    // Fresh backend with the SAME peer id restores from the snapshot.
    // Its next minted id must not collide with the ones in the snapshot.
    let mut b = Backend::with_peer(7);
    b.restore(snap);
    let new_id = b.add_node();
    assert!(
        new_id.peer == 7 && new_id.seq >= 2,
        "expected fresh id past existing seqs, got {new_id:?}"
    );
}

// Snapshot encode/decode is tested by the backend snapshot/restore tests.
// The old kyoso_graph_crdt::Snapshot type with direct nodes/edges fields
// has been replaced by kyoso_crdt::Snapshot<GraphTopology, S> with
// topology/schemas structure. Encoding is handled by serde automatically.

#[test]
fn move_op_round_trips_tree_parent_and_position() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let parent = a.add_node();
    let child = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    // Hand-craft a Move op as the tree integration would.
    let op = Op::new(
        CrdtId::new(1, 99),
        OpKind::Move {
            target: child,
            new_parent: Some(parent),
            position: "n".into(),
        },
    );
    let stamped = log.append(op);
    a.apply_remote(&stamped).expect("a apply");
    b.apply_remote(&stamped).expect("b apply");

    assert_eq!(a.applied_seq(), 3);
    assert_eq!(b.applied_seq(), 3);
    assert_eq!(a.tree_parent(child), Some(parent));
    assert_eq!(b.tree_parent(child), Some(parent));
    assert_eq!(a.node_order_key(child), Some("n"));
    assert_eq!(b.node_order_key(child), Some("n"));
}

#[test]
fn move_op_rejects_cycles() {
    let mut a = make_replica(1);
    let mut log = Log::new();

    let n1 = a.add_node();
    let n2 = a.add_node();
    let n3 = a.add_node();
    flush(&mut a, &mut log, &mut []);

    // Build chain: n2 -> n1, n3 -> n2. `move_node` no longer locally
    // pre-applies (was the cause of the concurrent-move divergence
    // documented in `tests/move_race.rs`), so the chain only takes
    // effect after each move's echo lands. We flush between moves
    // so the local cycle check on the next call sees the prior move
    // applied.
    assert!(a.move_node(n2, Some(n1), "a".into()));
    flush(&mut a, &mut log, &mut []);
    assert!(a.move_node(n3, Some(n2), "a".into()));
    flush(&mut a, &mut log, &mut []);

    // Local check: making n1 a child of n3 would form a cycle.
    assert!(!a.move_node(n1, Some(n3), "a".into()));
    // Self-parent is also a cycle.
    assert!(!a.move_node(n1, Some(n1), "a".into()));

    // Apply a remote op that would form a cycle: it's silently dropped
    // (no state change) but advances applied_seq deterministically.
    let bad = Op::new(
        CrdtId::new(1, 999),
        OpKind::Move {
            target: n1,
            new_parent: Some(n3),
            position: "z".into(),
        },
    );
    let stamped = log.append(bad);
    a.apply_remote(&stamped).expect("apply");
    assert_eq!(a.tree_parent(n1), None, "n1 should remain a root");
}
