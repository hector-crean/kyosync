//! Agent traversal: tree walks, subtree filters, NodeRef resolution.
//!
//! Uses the shared `spawn_demo_scene` fixture so test assertions track
//! the same shape printed by `cargo run -p kyoso_agent --bin demo`.

use kyoso_agent::{spawn_demo_scene, SceneAgent};
use kyoso_graph::traversal::{NodeRef, Order, TraversalQuery};

#[test]
fn agent_subtree_yields_full_descendant_set_with_depth_metadata() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let rows = agent.subtree(ents.root, TraversalQuery::new().order(Order::Bfs));

    // root + header + body + label + body_caption = 5 nodes.
    assert_eq!(rows.len(), 5, "expected 5 rows, got {rows:?}");

    // First row is always the root at depth 0.
    assert_eq!(rows[0].entity, ents.root);
    assert_eq!(rows[0].depth, 0);

    // Every other row has a parent within the tree we walked.
    for row in rows.iter().skip(1) {
        assert!(row.parent.is_some(), "child {row:?} should have a parent");
    }
}

#[test]
fn agent_subtree_resolves_replicated_and_local_node_refs() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let rows = agent.subtree(ents.root, TraversalQuery::new());

    // Fixture binds root + header + body in EntityCrdtIndex; label and
    // body_caption are deliberately unbound.
    let by_entity: std::collections::HashMap<_, _> =
        rows.iter().map(|r| (r.entity, r.id)).collect();

    assert!(matches!(by_entity.get(&ents.root), Some(NodeRef::Replicated(_))));
    assert!(matches!(by_entity.get(&ents.header), Some(NodeRef::Replicated(_))));
    assert!(matches!(by_entity.get(&ents.body), Some(NodeRef::Replicated(_))));
    assert!(matches!(by_entity.get(&ents.label), Some(NodeRef::Local(_))));
    assert!(matches!(by_entity.get(&ents.body_caption), Some(NodeRef::Local(_))));
}

#[test]
fn agent_subtree_max_depth_caps_descent() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let rows = agent.subtree(
        ents.root,
        TraversalQuery::new().order(Order::Bfs).max_depth(1),
    );

    // depth-1 cap: root (depth 0) + header + body (depth 1) = 3 rows.
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.depth <= 1));
}
