//! Integration tests for the comments CRDT sync model.
//!
//! Same shape as `kyoso_graph_crdt/tests/sync.rs`: two replicas share
//! an [`InMemoryOpLog<CommentOpKind>`], mutations on either side flow
//! through the log and apply on the other.

use kyoso_comments_crdt::{CommentOpKind, CommentsBackend, CommentsSnapshot};
use kyoso_crdt::{
    ApplyError, CrdtId, CrdtModel, IdGen, InMemoryOpLog, Op, OpLogRead, OpLogWrite, PeerId,
};

type Log = InMemoryOpLog<CommentOpKind>;

fn make_replica(peer: PeerId) -> CommentsBackend {
    CommentsBackend::with_peer(peer)
}

fn flush(replica: &mut CommentsBackend, log: &mut Log, peers: &mut [&mut CommentsBackend]) {
    for op in replica.drain_pending() {
        let stamped = log.append(op);
        replica.apply_remote(&stamped).expect("apply (originator)");
        for peer in peers.iter_mut() {
            peer.apply_remote(&stamped).expect("apply (peer)");
        }
    }
}

/// Stand-in for a graph node id — comments don't validate cross-model
/// references, the shared `IdGen` is what makes them safe in production.
fn anchor() -> CrdtId {
    CrdtId::new(99, 42)
}

#[test]
fn add_comment_round_trip() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let c = a.add_comment(anchor(), None, "hello".into());
    assert_eq!(a.comment_count(), 1, "originator pre-applies the create");
    assert_eq!(b.comment_count(), 0);

    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(a.comment_count(), 1);
    assert_eq!(b.comment_count(), 1);
    assert_eq!(a.applied_seq(), 1);
    assert_eq!(b.applied_seq(), 1);
    assert_eq!(a.body(c), Some("hello"));
    assert_eq!(b.body(c), Some("hello"));
    assert_eq!(c, CrdtId::new(1, 0));
    assert_eq!(a.anchor(c), Some(anchor()));
    assert_eq!(a.parent(c), Some(None));
}

#[test]
fn reply_carries_parent() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let root = a.add_comment(anchor(), None, "root".into());
    let reply = a.add_comment(anchor(), Some(root), "reply".into());
    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(b.parent(reply), Some(Some(root)));
    assert_eq!(b.body(reply), Some("reply"));
    assert_eq!(b.body(root), Some("root"));
}

#[test]
fn edit_body_lww_resolves_concurrent_edits() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let c = a.add_comment(anchor(), None, "v1".into());
    flush(&mut a, &mut log, &mut [&mut b]);

    a.edit_body(c, "from-a".into());
    b.edit_body(c, "from-b".into());

    flush(&mut a, &mut log, &mut [&mut b]);
    flush(&mut b, &mut log, &mut [&mut a]);

    assert_eq!(a.applied_seq(), b.applied_seq());
    let a_body = a.body(c).map(str::to_string);
    let b_body = b.body(c).map(str::to_string);
    assert_eq!(a_body, b_body, "both replicas must converge");
    assert!(
        matches!(a_body.as_deref(), Some("from-a") | Some("from-b")),
        "winner must be one of the submitted values, got {a_body:?}",
    );
}

#[test]
fn delete_comment_hides_from_count() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let c = a.add_comment(anchor(), None, "doomed".into());
    flush(&mut a, &mut log, &mut [&mut b]);
    a.delete_comment(c);
    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(a.comment_count(), 0);
    assert_eq!(b.comment_count(), 0);
    assert!(a.is_deleted(c));
    assert!(b.is_deleted(c));
    // Anchor is still queryable — needed when late-arriving replies
    // need to find their thread.
    assert_eq!(a.anchor(c), Some(anchor()));
}

#[test]
fn apply_remote_is_idempotent() {
    let mut a = make_replica(1);
    let mut b = make_replica(2);
    let mut log = Log::new();

    let _c = a.add_comment(anchor(), None, "once".into());
    flush(&mut a, &mut log, &mut [&mut b]);

    let op = log.slice(0, 1).remove(0);
    assert!(b.apply_remote(&op).is_ok());
    assert!(b.apply_remote(&op).is_ok());
    assert_eq!(b.comment_count(), 1);
    assert_eq!(b.applied_seq(), 1);
}

#[test]
fn out_of_order_apply_rejected() {
    let mut b = make_replica(2);
    let bad: Op<CommentOpKind> = Op {
        id: CrdtId::new(99, 0),
        seq: Some(5),
        kind: CommentOpKind::AddComment {
            anchor: anchor(),
            parent: None,
            body: "bad".into(),
        },
    };
    assert_eq!(
        b.apply_remote(&bad),
        Err(ApplyError::OutOfOrder { expected: 1, got: 5 })
    );
}

#[test]
fn unconfirmed_apply_rejected() {
    let mut b = make_replica(2);
    let pending: Op<CommentOpKind> = Op::new(
        CrdtId::new(1, 0),
        CommentOpKind::AddComment {
            anchor: anchor(),
            parent: None,
            body: "x".into(),
        },
    );
    assert_eq!(b.apply_remote(&pending), Err(ApplyError::Unconfirmed));
}

#[test]
fn snapshot_round_trip() {
    let mut a = make_replica(1);
    let mut log = Log::new();

    let root = a.add_comment(anchor(), None, "root".into());
    let _reply = a.add_comment(anchor(), Some(root), "reply".into());
    a.edit_body(root, "root v2".into());
    flush(&mut a, &mut log, &mut []);

    let snap = a.snapshot();
    assert_eq!(snap.at_seq, 3);
    assert_eq!(snap.comments.len(), 2);

    let mut c = CommentsBackend::with_peer(99);
    c.restore(snap);
    assert_eq!(c.applied_seq(), 3);
    assert_eq!(c.comment_count(), 2);
    assert_eq!(c.body(root), Some("root v2"));
}

#[test]
fn snapshot_encode_decode() {
    let snap = CommentsSnapshot {
        at_seq: 7,
        comments: vec![kyoso_comments_crdt::CommentSnap {
            id: CrdtId::new(1, 0),
            anchor: anchor(),
            parent: None,
            body: Some("hi".into()),
            deleted: false,
        }],
    };
    let bytes = snap.encode().unwrap();
    let decoded = CommentsSnapshot::decode(&bytes).unwrap();
    assert_eq!(decoded, snap);
}

#[test]
fn shared_id_gen_keeps_cross_model_ids_unique() {
    // Simulate the multi-model case: graph_crdt and comments share one
    // IdGen handle. After both backends mint IDs, none collide.
    let ids = IdGen::new(7);
    let mut comments_a = CommentsBackend::with_shared_ids(ids.clone());
    let mut comments_b = CommentsBackend::with_shared_ids(ids.clone());

    let c1 = comments_a.add_comment(anchor(), None, "a1".into());
    let c2 = comments_b.add_comment(anchor(), None, "b1".into());
    let c3 = comments_a.add_comment(anchor(), None, "a2".into());

    // All three IDs should have peer=7 and distinct seqs.
    assert_eq!(c1.peer, 7);
    assert_eq!(c2.peer, 7);
    assert_eq!(c3.peer, 7);
    let mut seqs = vec![c1.seq, c2.seq, c3.seq];
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), 3, "shared IdGen must yield distinct seqs");
}

#[test]
fn cross_model_anchor_uses_graph_id() {
    // A comment can anchor to any CrdtId — typically a graph node id
    // minted by `kyoso_graph_crdt`. The comments backend doesn't
    // validate; the shared `IdGen` is what guarantees the reference
    // is unambiguous.
    use kyoso_crdt::EmptySchema;
    use kyoso_graph_crdt::GraphBackend;

    let ids = IdGen::new(5);
    let mut graph = GraphBackend::<EmptySchema>::with_shared_ids(ids.clone());
    let mut comments = CommentsBackend::with_shared_ids(ids.clone());

    let node_id = graph.add_node();
    let comment_id = comments.add_comment(node_id, None, "anchored to node".into());

    assert_eq!(comments.anchor(comment_id), Some(node_id));
    assert_ne!(node_id, comment_id, "graph node and comment have distinct ids");
    assert_eq!(node_id.peer, comment_id.peer);
}
