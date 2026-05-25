//! Cross-store agreement test for the
//! [`kyoso_graph_crdt::GraphView`] abstraction.
//!
//! Builds a random topology on the headless
//! [`kyoso_graph_crdt::GraphTopology`], projects the same shape into
//! Bevy ECS, and asserts that
//! [`kyoso_graph_crdt::would_create_cycle`] agrees on both stores for
//! every `(target, proposed_parent)` pair of live nodes.
//!
//! What this catches:
//! - Drift between the two graph-algorithm implementations after a
//!   refactor (one side learns a new edge case, the other doesn't).
//! - Mistakes in the ECS adapter's "live edges to live neighbors only"
//!   filtering — anything that makes the ECS view see a node the CRDT
//!   side has tombstoned (or vice versa).
//! - Misclassification of `TreeParent::None` vs missing `TreeParent`
//!   on the ECS side, since the cycle walk relies on the parent chain
//!   terminating at the root.

use std::collections::HashMap;

use bevy::prelude::*;
use kyoso_crdt::{CrdtId, EmptySchema};
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph::ecs_view::EcsGraphView;
use kyoso_graph_crdt::{GraphBackend, would_create_cycle};
use proptest::prelude::*;

/// Marker for entities that participate in this test's graph. Pairs
/// with `EcsGraphView<TestNode>` to give the view a definition of
/// "what counts as a node". Real apps use their domain marker
/// (e.g. `kyoso_core::SceneNode`).
#[derive(Component, Default, Debug, Clone, Copy)]
struct TestNode;

/// A deterministic op stream that builds a random topology. Kept
/// abstract (indices instead of `CrdtId`s) so we can drive both
/// stores from the same script.
#[derive(Clone, Debug)]
enum AbstractOp {
    AddNode,
    /// Add an edge between two existing node indices.
    AddEdge {
        from_idx: usize,
        to_idx: usize,
    },
    /// Move a node under a new parent. `Some(idx)` means parent is the
    /// node at that index; `None` means reparent to root.
    Move {
        target_idx: usize,
        parent_idx: Option<usize>,
    },
    /// Tombstone the node at this index.
    RemoveNode {
        target_idx: usize,
    },
}

/// Drive a [`GraphBackend`] through the op stream and return:
/// - the resulting `(GraphBackend, crdt_ids_in_creation_order)`.
///
/// `crdt_ids_in_creation_order` contains every `AddNode`'s minted id;
/// `RemoveNode` does not shrink it, so indices in the abstract stream
/// stay stable across tombstones. The CRDT side's apply layer takes
/// care of dropping ops that name tombstoned targets.
fn apply_to_crdt(ops: &[AbstractOp]) -> (GraphBackend<EmptySchema>, Vec<CrdtId>) {
    let mut backend = GraphBackend::<EmptySchema>::with_peer(1);
    let mut ids: Vec<CrdtId> = Vec::new();
    for op in ops {
        match op {
            AbstractOp::AddNode => {
                let id = backend.add_node();
                ids.push(id);
            }
            AbstractOp::AddEdge { from_idx, to_idx } => {
                if let (Some(&from), Some(&to)) = (ids.get(*from_idx), ids.get(*to_idx)) {
                    if from != to {
                        backend.add_edge(from, to);
                    }
                }
            }
            AbstractOp::Move {
                target_idx,
                parent_idx,
            } => {
                let Some(&target) = ids.get(*target_idx) else {
                    continue;
                };
                let parent = match parent_idx {
                    Some(idx) => match ids.get(*idx).copied() {
                        Some(p) if p != target => Some(p),
                        _ => continue,
                    },
                    None => None,
                };
                // GraphBackend::move_node refuses cycles internally —
                // we can throw moves at it freely; the apply layer
                // gates them.
                backend.move_node(target, parent, format!("k{}", ids.len()));
            }
            AbstractOp::RemoveNode { target_idx } => {
                if let Some(&id) = ids.get(*target_idx) {
                    backend.remove_node(id);
                }
            }
        }
    }
    (backend, ids)
}

/// Project the CRDT topology into an ECS world. Returns the
/// `CrdtId → Entity` map so the cross-check can translate ids.
///
/// Only live nodes and live edges land in the world. Tree parents are
/// translated via `ChildOf`; if a parent id maps to nothing (it was
/// tombstoned), the child remains a root (no `ChildOf`). That matches
/// the CRDT side's `tree_parent` semantics (`topology.tree_parent`
/// returns `None` for tombstoned parents).
fn project_to_ecs(
    backend: &GraphBackend<EmptySchema>,
) -> (World, HashMap<CrdtId, Entity>, HashMap<Entity, CrdtId>) {
    let mut world = World::new();
    let topology = backend.backend().topology();
    let mut id_to_entity: HashMap<CrdtId, Entity> = HashMap::new();
    let mut entity_to_id: HashMap<Entity, CrdtId> = HashMap::new();
    // Pass 1: spawn every live node carrying the `TestNode` marker.
    // The marker is the "is a graph node" signal that `EcsGraphView`
    // queries against — we need the entity for every live id before
    // we can set parents, because a child can be created before its
    // parent in the order we iterate `live_node_ids`.
    for id in topology.live_node_ids() {
        let entity = world.spawn(TestNode).id();
        id_to_entity.insert(id, entity);
        entity_to_id.insert(entity, id);
    }
    // Pass 2: stamp `ChildOf` on each non-root node. Roots have no
    // `ChildOf` component at all.
    for id in topology.live_node_ids() {
        let Some(parent_id) = topology.tree_parent(id) else {
            continue;
        };
        let Some(&parent_entity) = id_to_entity.get(&parent_id) else {
            continue;
        };
        let entity = id_to_entity[&id];
        world.entity_mut(entity).insert(ChildOf(parent_entity));
    }
    // Pass 3: spawn edge entities. Each edge is its own entity
    // carrying `EdgeFrom` / `EdgeTo` (Bevy auto-maintains the
    // `OutgoingEdges` / `IncomingEdges` reverse indices on the
    // endpoints).
    for edge_id in topology.live_edge_ids() {
        let Some((from, to)) = topology.edge_endpoints(edge_id) else {
            continue;
        };
        let (Some(&from_e), Some(&to_e)) = (id_to_entity.get(&from), id_to_entity.get(&to)) else {
            // Skip orphan edges (endpoint tombstoned). The CRDT
            // invariants module catches this as `OrphanEdge` — it
            // shouldn't happen on a well-behaved topology, but we
            // defend against the case so the test doesn't panic.
            continue;
        };
        world.spawn((EdgeFrom(from_e), EdgeTo(to_e)));
    }
    (world, id_to_entity, entity_to_id)
}

/// Run `f(view)` inside a one-shot system on `world` so we can
/// borrow [`EcsGraphView`] (a `SystemParam`) safely. Returns the
/// result of `f`.
fn with_ecs_view<R: Send + Sync + 'static>(
    world: &mut World,
    f: impl FnOnce(&EcsGraphView<TestNode>) -> R + Send + Sync + 'static,
) -> R {
    let (sender, receiver) = std::sync::mpsc::channel();
    let f_cell = std::sync::Mutex::new(Some(f));
    let mut sys = bevy::ecs::system::IntoSystem::into_system(
        move |view: EcsGraphView<TestNode>| {
            let f = f_cell.lock().unwrap().take().expect("f consumed once");
            let result = f(&view);
            sender.send(result).expect("channel send");
        },
    );
    sys.initialize(world);
    let _ = sys.run((), world);
    receiver.try_recv().expect("system never ran")
}

/// Cross-check: for every `(target, parent)` pair of live CRDT nodes,
/// both implementations of `would_create_cycle` must return the same
/// answer.
fn check_agreement(ops: &[AbstractOp]) {
    let (backend, _ids) = apply_to_crdt(ops);
    let topology = backend.backend().topology();
    let live_ids: Vec<CrdtId> = topology.live_node_ids().collect();
    let (mut world, id_to_entity, _entity_to_id) = project_to_ecs(&backend);

    // Hand the ECS side the same set of `(target, parent)` pairs
    // (translated via the map) and collect its answers.
    let pairs_ecs: Vec<(Entity, Entity)> = {
        let map = &id_to_entity;
        live_ids
            .iter()
            .flat_map(|t| {
                let t_e = map[t];
                live_ids.iter().map(move |p| (t_e, map[p]))
            })
            .collect()
    };
    let ecs_answers = with_ecs_view(&mut world, move |view| {
        pairs_ecs
            .into_iter()
            .map(|(t, p)| would_create_cycle(view, t, p))
            .collect::<Vec<_>>()
    });

    let crdt_answers: Vec<bool> = live_ids
        .iter()
        .flat_map(|t| {
            live_ids
                .iter()
                .map(move |p| would_create_cycle(topology, *t, *p))
        })
        .collect();

    assert_eq!(
        crdt_answers.len(),
        ecs_answers.len(),
        "pair counts diverged"
    );
    for (i, (a, b)) in crdt_answers.iter().zip(ecs_answers.iter()).enumerate() {
        if a != b {
            // Translate index back to the offending pair for the
            // failure message — easier to debug than a raw vec diff.
            let t_idx = i / live_ids.len();
            let p_idx = i % live_ids.len();
            panic!(
                "cycle-check disagreement on (target={:?}, parent={:?}): \
                 crdt={a}, ecs={b}",
                live_ids[t_idx], live_ids[p_idx]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Hand-crafted regression cases
// ---------------------------------------------------------------------------

#[test]
fn empty_graph_agrees() {
    check_agreement(&[]);
}

#[test]
fn single_node_self_check_is_cycle() {
    check_agreement(&[AbstractOp::AddNode]);
}

#[test]
fn simple_tree_agrees() {
    // Three nodes; node[1] and node[2] are children of node[0].
    check_agreement(&[
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::Move {
            target_idx: 1,
            parent_idx: Some(0),
        },
        AbstractOp::Move {
            target_idx: 2,
            parent_idx: Some(0),
        },
    ]);
}

#[test]
fn chain_agrees() {
    // 0 ← 1 ← 2 ← 3. Both stores must agree that making 0 a child
    // of 3 cycles.
    check_agreement(&[
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::Move {
            target_idx: 1,
            parent_idx: Some(0),
        },
        AbstractOp::Move {
            target_idx: 2,
            parent_idx: Some(1),
        },
        AbstractOp::Move {
            target_idx: 3,
            parent_idx: Some(2),
        },
    ]);
}

#[test]
fn tombstoned_parent_breaks_chain() {
    // Build a chain, then remove the middle node. The CRDT side's
    // `tree_parent` returns `None` for tombstoned nodes; the ECS
    // projection drops the dead entity entirely. The check below
    // exercises that "tombstoned parent" doesn't leave the surviving
    // child claiming a phantom ancestor.
    check_agreement(&[
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::Move {
            target_idx: 1,
            parent_idx: Some(0),
        },
        AbstractOp::Move {
            target_idx: 2,
            parent_idx: Some(1),
        },
        AbstractOp::RemoveNode { target_idx: 1 },
    ]);
}

#[test]
fn edges_do_not_affect_tree_cycle_check() {
    // Reference edges (not TreeEdge) must not influence
    // would_create_cycle, which walks `tree_parent` only.
    check_agreement(&[
        AbstractOp::AddNode,
        AbstractOp::AddNode,
        AbstractOp::AddEdge {
            from_idx: 0,
            to_idx: 1,
        },
        AbstractOp::AddEdge {
            from_idx: 1,
            to_idx: 0,
        },
    ]);
}

// ---------------------------------------------------------------------------
// proptest — random op streams, small fan-out
// ---------------------------------------------------------------------------

prop_compose! {
    /// 0..=15 ops over a graph with at most 8 nodes. Keeps the search
    /// space small so shrinking finishes fast on a failure.
    fn any_op_stream()(ops in proptest::collection::vec(any_abstract_op(), 0..=15)) -> Vec<AbstractOp> {
        ops
    }
}

fn any_abstract_op() -> impl Strategy<Value = AbstractOp> {
    // Bias toward AddNode so the topology actually has nodes to
    // operate on — otherwise most ops short-circuit on missing
    // indices and the test mostly covers the empty case.
    prop_oneof![
        4 => Just(AbstractOp::AddNode),
        2 => (0usize..8, 0usize..8).prop_map(|(from_idx, to_idx)| AbstractOp::AddEdge { from_idx, to_idx }),
        3 => (0usize..8, prop::option::weighted(0.8, 0usize..8))
            .prop_map(|(target_idx, parent_idx)| AbstractOp::Move { target_idx, parent_idx }),
        1 => (0usize..8).prop_map(|target_idx| AbstractOp::RemoveNode { target_idx }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..Default::default() })]

    #[test]
    fn random_op_streams_agree(ops in any_op_stream()) {
        check_agreement(&ops);
    }
}
