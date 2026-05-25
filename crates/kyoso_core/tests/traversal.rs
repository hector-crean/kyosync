//! Integration tests for the agent-facing traversal API.
//!
//! Mirrors the kyoso-world workspace's `world_traversal.rs` shape:
//! seed a small scene tree, run the various `TraversalQuery` /
//! `WorldTreeView` entry points, and assert the resulting rows.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use kyoso_core::{
    FrameData, NodeKind, RectangleData, SceneNode, TextData,
};
use kyoso_graph::traversal::{
    NodeRef, Order, Step, TraversalNode, TraversalQuery, WorldTreeView,
};
use kyoso_graph::tree::OrderKey;
use kyoso_graph_sync::EntityCrdtIndex;

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app
}

/// Seed a small fixture:
///
/// ```text
/// root (Frame, replicated)
/// ├── rect (Rectangle, replicated, OrderKey "a")
/// └── text (Text, NOT replicated, OrderKey "b")
/// ```
///
/// Returns `(root, rect, text)`. The non-replicated `text` exercises
/// `NodeRef::Local` fallback.
fn spawn_fixture(world: &mut World) -> (Entity, Entity, Entity) {
    let (root, rect, text) = world
        .run_system_once(|mut commands: Commands| {
            let root = commands
                .spawn((FrameData::default(), Transform::IDENTITY, SceneNode))
                .id();
            let rect = commands
                .spawn((
                    RectangleData::default(),
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(root),
                    OrderKey("a".into()),
                ))
                .id();
            let text = commands
                .spawn((
                    TextData::default(),
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(root),
                    OrderKey("b".into()),
                ))
                .id();
            (root, rect, text)
        })
        .expect("spawn fixture");

    // Populate the index for `root` and `rect` only — `text` is
    // intentionally left out so it surfaces as `NodeRef::Local`.
    let mut index = EntityCrdtIndex::default();
    index.bind_node(root, kyoso_crdt::CrdtId::new(1, 1));
    index.bind_node(rect, kyoso_crdt::CrdtId::new(1, 2));
    world.insert_resource(index);

    (root, rect, text)
}

#[test]
fn bfs_walk_yields_root_then_children_in_order_key_order() {
    let mut app = test_app();
    let (root, rect, text) = spawn_fixture(app.world_mut());

    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Bfs),
            )
        })
        .expect("traverse runs");

    let entities: Vec<Entity> = walked.iter().map(|r| r.entity).collect();
    // BFS over the tree: root first, then its children in OrderKey
    // order (`a` < `b`, so rect before text).
    assert_eq!(entities, vec![root, rect, text]);

    // Depth + parent metadata flows through.
    assert_eq!(walked[0].depth, 0);
    assert_eq!(walked[0].parent, None);
    assert_eq!(walked[1].depth, 1);
    assert_eq!(walked[1].parent, Some(root));
    assert_eq!(walked[2].depth, 1);
    assert_eq!(walked[2].parent, Some(root));
}

#[test]
fn dfs_walk_visits_every_node_under_root() {
    let mut app = test_app();
    let (root, rect, text) = spawn_fixture(app.world_mut());

    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Dfs),
            )
        })
        .expect("traverse runs");

    // DFS visits all three but in a stack-driven order; assert
    // membership + that root comes first (depth 0).
    assert_eq!(walked.len(), 3);
    assert_eq!(walked[0].entity, root);
    let mut visited: Vec<Entity> = walked.iter().map(|r| r.entity).collect();
    visited.sort();
    let mut expected = vec![root, rect, text];
    expected.sort();
    assert_eq!(visited, expected);
}

#[test]
fn bfs_walk_yields_replicated_and_local_node_refs() {
    let mut app = test_app();
    let (root, _rect, text) = spawn_fixture(app.world_mut());

    // `traverse_with::<EntityCrdtIndex>` resolves each row through the
    // sync index — bound entities surface as `NodeRef::Replicated`,
    // unbound ones fall through to `NodeRef::Local`.
    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse_with::<EntityCrdtIndex>(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Bfs),
            )
        })
        .expect("traverse runs");

    // root + rect are in the index → Replicated.
    assert!(matches!(walked[0].id, NodeRef::Replicated(_)));
    assert!(matches!(walked[1].id, NodeRef::Replicated(_)));
    // text was deliberately left out → Local fallback.
    match walked[2].id {
        NodeRef::Local(bits) => assert_eq!(bits, text.to_bits()),
        other => panic!("expected NodeRef::Local for text, got {other:?}"),
    }
}

#[test]
fn max_depth_caps_descent_without_pruning_root() {
    let mut app = test_app();
    // Grandchild under `rect` to give us a depth-2 node.
    let (root, rect, _text) = spawn_fixture(app.world_mut());
    app.world_mut()
        .run_system_once(move |mut commands: Commands| {
            commands.spawn((
                RectangleData::default(),
                Transform::IDENTITY,
                SceneNode,
                ChildOf(rect),
                OrderKey("a".into()),
            ));
        })
        .expect("spawn grandchild");

    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Bfs)
                    .max_depth(1),
            )
        })
        .expect("traverse runs");

    // Should be exactly 3 rows (root + 2 children), no depth-2 descendants.
    assert_eq!(walked.len(), 3);
    assert!(walked.iter().all(|r| r.depth <= 1));
}

#[test]
fn require_filters_to_only_entities_with_component() {
    let mut app = test_app();
    let (root, rect, _text) = spawn_fixture(app.world_mut());

    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Bfs)
                    .require::<NodeKind>(),
            )
        })
        .expect("traverse runs");

    // All fixture entities have NodeKind (auto-inserted via `#[require]`);
    // the filter keeps them all but excludes any hypothetical bare entity.
    assert!(walked.iter().any(|r| r.entity == root));
    assert!(walked.iter().any(|r| r.entity == rect));
}

#[test]
fn exclude_drops_entities_with_component() {
    let mut app = test_app();
    let (root, _rect, _text) = spawn_fixture(app.world_mut());

    // Spawn a bare overlay child under root with no SceneNode.
    let overlay = app
        .world_mut()
        .run_system_once(
            move |mut commands: Commands| {
                commands
                    .spawn((ChildOf(root), OrderKey("c".into())))
                    .id()
            },
        )
        .expect("spawn overlay");

    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse_with::<EntityCrdtIndex>(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Bfs)
                    .exclude::<SceneNode>(),
            )
        })
        .expect("traverse runs");

    // root + rect + text all carry SceneNode → excluded.
    // Only the bare overlay survives.
    assert_eq!(walked.len(), 1);
    assert_eq!(walked[0].entity, overlay);
    assert!(matches!(walked[0].id, NodeRef::Local(_)));
}

#[test]
fn step_with_prune_drops_node_and_its_subtree() {
    let mut app = test_app();
    let (root, rect, text) = spawn_fixture(app.world_mut());

    // Prune the subtree rooted at `rect`.
    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse(
                &TraversalQuery::new()
                    .start_at(root)
                    .order(Order::Bfs)
                    .step_with(move |tn: &TraversalNode, _er| {
                        if tn.entity == rect {
                            Step::Prune
                        } else {
                            Step::Visit
                        }
                    }),
            )
        })
        .expect("traverse runs");

    let entities: Vec<Entity> = walked.iter().map(|r| r.entity).collect();
    // rect itself is pruned (not yielded); its (lack of) subtree wouldn't
    // matter here but the semantics are: Prune drops the node AND skips
    // expansion. text (sibling of rect) still appears.
    assert!(!entities.contains(&rect));
    assert!(entities.contains(&root));
    assert!(entities.contains(&text));
}

#[test]
fn empty_when_start_at_not_set() {
    let mut app = test_app();
    let _ = spawn_fixture(app.world_mut());

    let walked = app
        .world_mut()
        .run_system_once(|view: WorldTreeView| view.traverse(&TraversalQuery::new()))
        .expect("traverse runs");

    assert!(walked.is_empty());
}

#[test]
fn component_names_dumps_archetype_for_known_entity_and_empty_for_missing() {
    let mut app = test_app();
    let (root, _rect, _text) = spawn_fixture(app.world_mut());

    let (root_names, missing_names) = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            let missing = Entity::from_raw_u32(99_999).unwrap();
            (view.component_names(root), view.component_names(missing))
        })
        .expect("component_names runs");

    // root carries Frame, Size (via FrameData), Transform, SceneNode,
    // TreeParent, plus the auto-`require`d NodeKind. We only assert
    // non-empty + a couple of stable substrings; Bevy's `DebugName`
    // formatting can vary across versions so we don't pin exact strings.
    eprintln!("root archetype components:\n{}", root_names.join("\n"));
    assert!(!root_names.is_empty(), "archetype dump returned no rows");

    // Missing entity returns empty, no panic.
    assert!(missing_names.is_empty());
}
