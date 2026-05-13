//! Direct unit tests for the suspected `move_node` concurrent-move
//! divergence the chaos sim flagged at seed `0xCAFEF038`.
//!
//! The scenario: peer X locally pre-applies `move_node(N1, P1)` and
//! peer Y locally pre-applies `move_node(P1, N1)` in the same logical
//! moment. Both ops get stamped (X@K, Y@K+1). On apply:
//!
//! - Canonical (no pre-apply, applies in seq order): X first → N1.parent=P1.
//!   Then Y: cycle-check from N1.parent=P1, walk P1 → None. Hits target P1?
//!   Yes (P1 is the move's target). Cycle. Reject Y.
//!   Final: N1 child of P1, P1 root.
//!
//! - Peer Y (issued Y, locally pre-applied P1.parent=N1): apply X
//!   echo first. Cycle-check from P1.parent=N1, walk N1 → None. Hits
//!   target N1? Yes. Cycle. Reject X.
//!   Then apply Y echo: cycle-check from N1.parent=None. No cycle.
//!   Apply: P1.parent=N1 (no change, already there).
//!   Final: P1 child of N1, N1 root.
//!
//! Different final trees — divergence. The bug is the same shape as
//! the cascade-tombstone race we just fixed: local pre-apply + a
//! per-replica check (here the cycle check) that may decide
//! differently from canonical.

use kyoso_crdt::{EmptySchema, InMemoryOpLog, OpLogRead, OpLogWrite};
use kyoso_graph_crdt::{GraphBackend, OpKind};

type Backend = GraphBackend<EmptySchema>;
type Log = InMemoryOpLog<OpKind>;

/// Send peer's pending ops through the shared log + broadcast back to
/// every replica including the originator. Mirrors the convention
/// from `tests/sync.rs`.
fn flush(replica: &mut Backend, log: &mut Log, peers: &mut [&mut Backend]) {
    for op in replica.drain_pending() {
        let stamped = log.append(op);
        replica.apply_remote(&stamped).expect("originator");
        for peer in peers.iter_mut() {
            peer.apply_remote(&stamped).expect("peer");
        }
    }
}

#[test]
fn concurrent_swap_moves_converge() {
    // Setup: two peers see two root nodes N1, P1.
    let mut x = Backend::with_peer(1);
    let mut y = Backend::with_peer(2);
    let mut log = Log::new();

    let n1 = x.add_node();
    let p1 = x.add_node();
    flush(&mut x, &mut log, &mut [&mut y]);

    // Concurrent moves: X wants N1 under P1, Y wants P1 under N1.
    // Both pre-apply locally (each sees no cycle in its OWN view).
    assert!(x.move_node(n1, Some(p1), "x".into()), "X's local move should pre-apply");
    assert!(y.move_node(p1, Some(n1), "y".into()), "Y's local move should pre-apply");

    // Server stamps in iteration order: X@3 (after the two AddNodes
    // and assuming flush stamps first), Y@4.
    flush(&mut x, &mut log, &mut [&mut y]);
    flush(&mut y, &mut log, &mut [&mut x]);

    // Build a canonical replica that applies every op from the log
    // in seq order, no pre-apply. This is the ground truth for what
    // every replica SHOULD converge to.
    let mut canonical = Backend::with_peer(99);
    let head = log.head();
    for op in log.slice(0, head) {
        canonical.apply_remote(&op).expect("canonical");
    }

    // The CRDT invariant: every replica's snapshot equals canonical's.
    let canon_snap = canonical.snapshot();
    let x_snap = x.snapshot();
    let y_snap = y.snapshot();

    assert_eq!(
        x.applied_seq(),
        canonical.applied_seq(),
        "X applied_seq matches canonical"
    );
    assert_eq!(
        y.applied_seq(),
        canonical.applied_seq(),
        "Y applied_seq matches canonical"
    );

    let n1_canon = canon_snap.topology.nodes.iter().find(|n| n.id == n1).unwrap();
    let p1_canon = canon_snap.topology.nodes.iter().find(|n| n.id == p1).unwrap();
    let n1_x = x_snap.topology.nodes.iter().find(|n| n.id == n1).unwrap();
    let p1_x = x_snap.topology.nodes.iter().find(|n| n.id == p1).unwrap();
    let n1_y = y_snap.topology.nodes.iter().find(|n| n.id == n1).unwrap();
    let p1_y = y_snap.topology.nodes.iter().find(|n| n.id == p1).unwrap();

    // The ASSERTION the bug violates:
    assert_eq!(
        (n1_x.tree_parent, p1_x.tree_parent),
        (n1_canon.tree_parent, p1_canon.tree_parent),
        "X's tree state must match canonical (canonical: N1.parent={:?}, P1.parent={:?}; \
         X has: N1.parent={:?}, P1.parent={:?})",
        n1_canon.tree_parent,
        p1_canon.tree_parent,
        n1_x.tree_parent,
        p1_x.tree_parent,
    );
    assert_eq!(
        (n1_y.tree_parent, p1_y.tree_parent),
        (n1_canon.tree_parent, p1_canon.tree_parent),
        "Y's tree state must match canonical (canonical: N1.parent={:?}, P1.parent={:?}; \
         Y has: N1.parent={:?}, P1.parent={:?})",
        n1_canon.tree_parent,
        p1_canon.tree_parent,
        n1_y.tree_parent,
        p1_y.tree_parent,
    );
}
