//! End-to-end composition convergence tests.
//!
//! Two replicas drive [`Document<NodeProperties>`] over a shared
//! [`InMemoryOpLog`]. Mutations on either side flow through the log and
//! converge on the other. Exercises:
//!
//! - typed mutate → wire round-trip via `derive(Crdt)` + `Document<S>`
//! - convergence under interleaved concurrent ops across mixed CRDT
//!   types (LWW + OrSet + PnCounter)
//! - idempotency under duplicate delivery
//! - order-independence (the lattice axioms in action)

use kyoso_crdt::types::{LwwMut, LwwRegister, OrSet, OrSetMut, PnCounter, PnMut};
use kyoso_crdt::{CrdtId, DeriveCrdt, InMemoryOpLog, OpLogRead, OpLogWrite, PeerId};
use kyoso_graph_crdt::{Document, OpKind};

#[derive(Clone, Debug, Default, PartialEq, DeriveCrdt)]
pub struct NodeProperties {
    pub name: LwwRegister<String>,
    pub tags: OrSet<String>,
    pub counter: PnCounter,
}

type Doc = Document<NodeProperties>;
type Log = InMemoryOpLog<OpKind>;

fn make_doc(peer: PeerId) -> Doc {
    Doc::with_peer(peer)
}

/// Send every pending op from `replica` through the shared log and broadcast
/// back to all replicas (including the originator) — same shape as
/// `crates/kyoso_graph_crdt/tests/sync.rs::flush`.
fn flush(replica: &mut Doc, log: &mut Log, peers: &mut [&mut Doc]) {
    let pending = replica.drain_pending();
    for op in pending {
        let stamped = log.append(op);
        replica
            .apply_remote(&stamped)
            .expect("apply (originator)");
        for peer in peers.iter_mut() {
            peer.apply_remote(&stamped).expect("apply (peer)");
        }
    }
}

#[test]
fn add_node_round_trips_via_document() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    assert!(a.node(id).is_some(), "originator sees the node");
    assert!(b.node(id).is_some(), "peer sees the node");
    assert_eq!(a.applied_seq(), b.applied_seq());
    assert_eq!(id, CrdtId::new(1, 0));
}

#[test]
fn lww_property_converges_across_replicas() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    a.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("alice".to_string())));
    flush(&mut a, &mut log, &mut [&mut b]);

    assert_eq!(a.node(id).and_then(|n| n.name.get()), Some(&"alice".to_string()));
    assert_eq!(b.node(id).and_then(|n| n.name.get()), Some(&"alice".to_string()));
}

#[test]
fn or_set_property_converges() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    a.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Add("draft".to_string())));
    b.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Add("review".to_string())));

    flush(&mut a, &mut log, &mut [&mut b]);
    flush(&mut b, &mut log, &mut [&mut a]);

    let a_tags: Vec<_> = a.node(id).unwrap().tags.iter().cloned().collect();
    let b_tags: Vec<_> = b.node(id).unwrap().tags.iter().cloned().collect();
    let mut a_sorted = a_tags.clone();
    let mut b_sorted = b_tags.clone();
    a_sorted.sort();
    b_sorted.sort();
    assert_eq!(a_sorted, vec!["draft", "review"]);
    assert_eq!(a_sorted, b_sorted);
}

#[test]
fn pn_counter_property_aggregates_per_replica() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    a.mutate_node(id, NodePropertiesMut::Counter(PnMut::Inc(3)));
    a.mutate_node(id, NodePropertiesMut::Counter(PnMut::Inc(2)));
    b.mutate_node(id, NodePropertiesMut::Counter(PnMut::Dec(1)));

    flush(&mut a, &mut log, &mut [&mut b]);
    flush(&mut b, &mut log, &mut [&mut a]);

    assert_eq!(a.node(id).unwrap().counter.value(), 4);
    assert_eq!(b.node(id).unwrap().counter.value(), 4);
}

#[test]
fn mixed_crdt_kinds_converge_under_interleaved_ops() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);

    // Interleave LWW + OrSet + PnCounter mutations across both replicas.
    a.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("alpha".to_string())));
    b.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Add("urgent".to_string())));
    a.mutate_node(id, NodePropertiesMut::Counter(PnMut::Inc(7)));
    b.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("beta".to_string())));
    a.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Add("draft".to_string())));

    // Drain in alternating order so concurrent ops actually interleave.
    flush(&mut a, &mut log, &mut [&mut b]);
    flush(&mut b, &mut log, &mut [&mut a]);

    // Both replicas should reach the same applied seq and same state.
    assert_eq!(a.applied_seq(), b.applied_seq());

    let a_name = a.node(id).unwrap().name.get().cloned();
    let b_name = b.node(id).unwrap().name.get().cloned();
    assert_eq!(a_name, b_name);

    let mut a_tags: Vec<_> = a.node(id).unwrap().tags.iter().cloned().collect();
    let mut b_tags: Vec<_> = b.node(id).unwrap().tags.iter().cloned().collect();
    a_tags.sort();
    b_tags.sort();
    assert_eq!(a_tags, b_tags);
    assert_eq!(a_tags, vec!["draft", "urgent"]);

    assert_eq!(
        a.node(id).unwrap().counter.value(),
        b.node(id).unwrap().counter.value()
    );
    assert_eq!(a.node(id).unwrap().counter.value(), 7);
}

#[test]
fn duplicate_delivery_is_idempotent() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    a.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("hello".to_string())));
    flush(&mut a, &mut log, &mut [&mut b]);

    // Re-deliver the entire log to b. Each apply_remote with seq <=
    // applied_seq is a no-op.
    let head = log.head();
    let diff: Vec<_> = log.slice(0, head);
    for op in &diff {
        b.apply_remote(op).unwrap();
    }
    for op in &diff {
        b.apply_remote(op).unwrap();
    }

    assert_eq!(b.node(id).unwrap().name.get(), Some(&"hello".to_string()));
}

#[test]
fn three_replicas_converge_under_concurrent_lww_writes() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut c = make_doc(3);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b, &mut c]);

    // Three concurrent writes to the same field. Server linearization
    // picks a winner deterministically; all three should converge.
    a.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("from-a".to_string())));
    b.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("from-b".to_string())));
    c.mutate_node(id, NodePropertiesMut::Name(LwwMut::Set("from-c".to_string())));

    flush(&mut a, &mut log, &mut [&mut b, &mut c]);
    flush(&mut b, &mut log, &mut [&mut a, &mut c]);
    flush(&mut c, &mut log, &mut [&mut a, &mut b]);

    let an = a.node(id).unwrap().name.get().cloned();
    let bn = b.node(id).unwrap().name.get().cloned();
    let cn = c.node(id).unwrap().name.get().cloned();
    assert_eq!(an, bn);
    assert_eq!(bn, cn);
    // The winner must be one of the three submitted values.
    assert!(matches!(an.as_deref(), Some("from-a") | Some("from-b") | Some("from-c")));
}

#[test]
fn remove_node_propagates() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    flush(&mut a, &mut log, &mut [&mut b]);
    a.remove_node(id);
    flush(&mut a, &mut log, &mut [&mut b]);

    assert!(a.node(id).is_none());
    assert!(b.node(id).is_none());
}

#[test]
fn or_set_concurrent_add_and_remove_add_wins() {
    let mut a = make_doc(1);
    let mut b = make_doc(2);
    let mut log = Log::new();

    let id = a.add_node();
    a.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Add("draft".to_string())));
    flush(&mut a, &mut log, &mut [&mut b]);

    // a removes "draft". b concurrently re-adds "draft" (with a fresh
    // dot — the new add's tag isn't observed by the remove).
    a.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Remove("draft".to_string())));
    b.mutate_node(id, NodePropertiesMut::Tags(OrSetMut::Add("draft".to_string())));

    flush(&mut a, &mut log, &mut [&mut b]);
    flush(&mut b, &mut log, &mut [&mut a]);

    // Add-wins: "draft" survives because b's add was concurrent with a's remove.
    assert!(a.node(id).unwrap().tags.contains(&"draft".to_string()));
    assert!(b.node(id).unwrap().tags.contains(&"draft".to_string()));
}
