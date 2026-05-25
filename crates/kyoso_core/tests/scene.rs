//! Integration tests for the typed `SceneView` SystemParam.
//!
//! `SceneView` (alias for `kyoso_graph::scene::Scene<&SceneNode, &SceneEdge>`)
//! is the combined view: the [`kyoso_graph::tree::TreeQuery`] for
//! hierarchy + the [`kyoso_graph::queries::GraphQuery<&SceneNode, &SceneEdge>`]
//! for entity-edge ops. These tests confirm both layers work
//! end-to-end against real kyoso_core components.
//!
//! ## Fixture
//!
//! ```text
//! root (Frame, SceneNode)
//! ├── rect (Rectangle, SceneNode, OrderKey "a")  ──┐
//! └── text (Text, SceneNode, OrderKey "b")  ◀──────┘
//!                                       (SceneEdge entity, rect → text)
//! ```
//!
//! Hierarchy is via Bevy's `ChildOf` / `Children`. The cross-frame
//! edge `rect → text` is a separate entity carrying `EdgeFrom(rect)`,
//! `EdgeTo(text)`, and `SceneEdge`.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use kyoso_core::{FrameData, RectangleData, SceneEdge, SceneNode, SceneView, Text, TextData};
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph::tree::OrderKey;

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app
}

/// Returns `(root, rect, text, edge)` — three scene nodes plus the
/// edge entity connecting rect → text.
fn spawn_fixture(world: &mut World) -> (Entity, Entity, Entity, Entity) {
    world
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
            // Cross-frame edge: rect → text. Distinct from the
            // hierarchy (which is purely Bevy `ChildOf`-based).
            let edge = commands
                .spawn((EdgeFrom(rect), EdgeTo(text), SceneEdge))
                .id();
            (root, rect, text, edge)
        })
        .expect("spawn fixture")
}

// ---------------------------------------------------------------------------
// Tree layer
// ---------------------------------------------------------------------------

#[test]
fn scene_tree_yields_ordered_children_via_order_key() {
    let mut app = test_app();
    let (root, rect, text, _edge) = spawn_fixture(app.world_mut());

    let children = app
        .world_mut()
        .run_system_once(move |scene: SceneView| scene.tree.children(root))
        .expect("query runs");

    // OrderKey "a" sorts before "b", so rect before text.
    assert_eq!(children, vec![rect, text]);
}

#[test]
fn scene_tree_parent_lookup_via_child_of() {
    let mut app = test_app();
    let (root, rect, text, _edge) = spawn_fixture(app.world_mut());

    let (rect_parent, text_parent, root_parent) = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            (
                scene.tree.parent(rect),
                scene.tree.parent(text),
                scene.tree.parent(root),
            )
        })
        .expect("query runs");

    assert_eq!(rect_parent, Some(root));
    assert_eq!(text_parent, Some(root));
    assert_eq!(root_parent, None, "root has no ChildOf");
}

#[test]
fn scene_tree_walk_dfs_with_depth() {
    let mut app = test_app();
    let (root, rect, text, _edge) = spawn_fixture(app.world_mut());

    let walked: Vec<(Entity, usize)> = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            scene.tree.walk_dfs_with_depth(root).collect::<Vec<_>>()
        })
        .expect("query runs");

    // DFS pops the stack so children come out in reverse-pushed order.
    // The walk visits root first (depth 0), then iterates children;
    // assert on membership + root-first.
    assert_eq!(walked.len(), 3);
    assert_eq!(walked[0], (root, 0));
    let entities: Vec<Entity> = walked.iter().map(|(e, _)| *e).collect();
    assert!(entities.contains(&rect));
    assert!(entities.contains(&text));
    // Children are at depth 1.
    for (e, d) in &walked[1..] {
        assert_eq!(*d, 1, "child {e:?} expected at depth 1");
    }
}

// ---------------------------------------------------------------------------
// Edge-graph layer
// ---------------------------------------------------------------------------

#[test]
fn scene_graph_find_edge_returns_the_entity_edge() {
    let mut app = test_app();
    let (_root, rect, text, edge) = spawn_fixture(app.world_mut());

    let found = app
        .world_mut()
        .run_system_once(move |scene: SceneView| scene.graph.find_edge(rect, text))
        .expect("query runs");

    assert_eq!(found, Some(edge));
}

#[test]
fn scene_graph_neighbors_and_predecessors() {
    let mut app = test_app();
    let (_root, rect, text, _edge) = spawn_fixture(app.world_mut());

    let (rect_neighbours, text_predecessors) = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let n: Vec<Entity> = scene.graph.neighbors(rect).collect();
            let p: Vec<Entity> = scene.graph.predecessors(text).collect();
            (n, p)
        })
        .expect("query runs");

    // rect → text is the only entity-edge.
    assert_eq!(rect_neighbours, vec![text]);
    assert_eq!(text_predecessors, vec![rect]);
}

#[test]
fn scene_graph_degrees() {
    let mut app = test_app();
    let (_root, rect, text, _edge) = spawn_fixture(app.world_mut());

    let (rect_out, rect_in, text_out, text_in) = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            (
                scene.graph.out_degree(rect),
                scene.graph.in_degree(rect),
                scene.graph.out_degree(text),
                scene.graph.in_degree(text),
            )
        })
        .expect("query runs");

    assert_eq!(rect_out, 1, "rect has one outgoing entity-edge");
    assert_eq!(rect_in, 0);
    assert_eq!(text_out, 0);
    assert_eq!(text_in, 1, "text has one incoming entity-edge");
}

#[test]
fn scene_graph_node_and_edge_counts() {
    let mut app = test_app();
    let _ = spawn_fixture(app.world_mut());

    let (node_count, edge_count) = app
        .world_mut()
        .run_system_once(|scene: SceneView| (scene.graph.node_count(), scene.graph.edge_count()))
        .expect("query runs");

    // 3 SceneNode entities (root + rect + text), 1 SceneEdge entity.
    assert_eq!(node_count, 3);
    assert_eq!(edge_count, 1);
}

// ---------------------------------------------------------------------------
// Both layers in one system
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Filter coupling: SceneView only sees SceneNode entities
// ---------------------------------------------------------------------------

#[test]
fn scene_view_filter_hides_non_scenenode_overlay_from_tree() {
    use kyoso_graph::tree::OrderKey;

    let mut app = test_app();
    let (root, rect, text, _edge) = spawn_fixture(app.world_mut());

    // Spawn a bare overlay child of root — `ChildOf(root)` + `OrderKey`
    // but **no** `SceneNode` marker. SceneView's tree should not see it.
    let overlay = app
        .world_mut()
        .run_system_once(move |mut commands: Commands| {
            commands
                .spawn((ChildOf(root), OrderKey("z".into())))
                .id()
        })
        .expect("spawn overlay");

    let scene_view_children = app
        .world_mut()
        .run_system_once(move |scene: SceneView| scene.tree.children(root))
        .expect("query runs");

    // The overlay must not appear via SceneView.tree.
    assert!(
        !scene_view_children.contains(&overlay),
        "overlay leaked into SceneView.tree.children: {scene_view_children:?}",
    );
    assert_eq!(scene_view_children, vec![rect, text]);
}

// ---------------------------------------------------------------------------
// WorldGraphView — entity-edge traversal with runtime filters
// ---------------------------------------------------------------------------

#[test]
fn world_graph_view_bfs_walks_entity_edges() {
    use kyoso_graph::traversal::{Order, TraversalQuery, WorldGraphView};

    let mut app = test_app();
    let (_root, rect, text, _edge) = spawn_fixture(app.world_mut());

    // BFS from `rect` over the entity-edge graph: rect → text.
    let walked = app
        .world_mut()
        .run_system_once(
            move |view: WorldGraphView<&SceneNode, &SceneEdge>| {
                view.traverse(
                    &TraversalQuery::new().start_at(rect).order(Order::Bfs),
                )
            },
        )
        .expect("traverse runs");

    let entities: Vec<bevy::prelude::Entity> = walked.iter().map(|r| r.entity).collect();
    // Only the rect→text edge — so we visit rect then text.
    assert_eq!(entities, vec![rect, text]);
    assert_eq!(walked[0].depth, 0);
    assert_eq!(walked[1].depth, 1);
}

#[test]
fn world_graph_view_runtime_typed_require_filters_results() {
    use kyoso_graph::traversal::{Order, TraversalQuery, WorldGraphView};

    let mut app = test_app();
    let (_root, rect, _text, _edge) = spawn_fixture(app.world_mut());

    // `require::<Text>` filters out non-Text nodes from yielded rows.
    // rect → text walk: rect (Rectangle) is dropped; text (Text) kept.
    let walked = app
        .world_mut()
        .run_system_once(
            move |view: WorldGraphView<&SceneNode, &SceneEdge>| {
                view.traverse(
                    &TraversalQuery::new()
                        .start_at(rect)
                        .order(Order::Bfs)
                        .require::<Text>(),
                )
            },
        )
        .expect("traverse runs");

    let entities: Vec<bevy::prelude::Entity> = walked.iter().map(|r| r.entity).collect();
    // rect doesn't have Text; text does. Only text survives.
    assert_eq!(entities.len(), 1);
    assert!(!entities.contains(&rect));
}

#[test]
fn world_scene_view_traverse_tree_and_graph_are_separate() {
    use kyoso_graph::tree::OrderKey;
    use kyoso_graph::traversal::{Order, TraversalQuery, WorldSceneView};

    let mut app = test_app();
    let (root, rect, text, _edge) = spawn_fixture(app.world_mut());

    // Spawn a non-tree-related cross-frame edge: root → text. Now the
    // tree and graph layers point at different topologies from root.
    app.world_mut()
        .run_system_once(move |mut commands: Commands| {
            commands.spawn((
                kyoso_graph::components::EdgeFrom(root),
                kyoso_graph::components::EdgeTo(text),
                SceneEdge,
            ));
            // Bonus: a non-SceneNode overlay so we can confirm the
            // filter is doing work on the tree side too.
            commands.spawn((ChildOf(root), OrderKey("z".into())));
        })
        .expect("spawn extra edge + overlay");

    let (tree_rows, graph_rows) = app
        .world_mut()
        .run_system_once(
            move |view: WorldSceneView<
                &SceneNode,
                &SceneEdge,
                bevy::ecs::query::With<SceneNode>,
            >| {
                let tree = view.traverse_tree(
                    &TraversalQuery::new().start_at(root).order(Order::Bfs),
                );
                let graph = view.traverse_graph(
                    &TraversalQuery::new().start_at(root).order(Order::Bfs),
                );
                (tree, graph)
            },
        )
        .expect("traverse runs");

    // Tree from root: root + its two SceneNode children (overlay filtered).
    let tree_entities: Vec<bevy::prelude::Entity> =
        tree_rows.iter().map(|r| r.entity).collect();
    assert_eq!(tree_entities, vec![root, rect, text]);

    // Graph from root: root → text via the cross-frame edge we added.
    // (root has no outgoing entity-edges to rect — that's only a tree
    // child relationship; rect→text isn't reachable from root via the
    // edge graph either.)
    let graph_entities: Vec<bevy::prelude::Entity> =
        graph_rows.iter().map(|r| r.entity).collect();
    assert_eq!(graph_entities, vec![root, text]);
}

#[test]
fn world_graph_view_still_sees_non_scenenode_overlay() {
    use kyoso_graph::traversal::{Order, TraversalQuery, WorldTreeView};
    use kyoso_graph::tree::OrderKey;

    let mut app = test_app();
    let (root, _rect, _text, _edge) = spawn_fixture(app.world_mut());

    // Same overlay spawn — bare child of root, no SceneNode marker.
    let overlay = app
        .world_mut()
        .run_system_once(move |mut commands: Commands| {
            commands
                .spawn((ChildOf(root), OrderKey("z".into())))
                .id()
        })
        .expect("spawn overlay");

    let walked = app
        .world_mut()
        .run_system_once(move |view: WorldTreeView| {
            view.traverse(
                &TraversalQuery::new().start_at(root).order(Order::Bfs),
            )
        })
        .expect("traverse runs");

    // WorldTreeView uses an *unfiltered* TreeQuery (agent traversal —
    // surfaces bare overlays via NodeRef::Local). The overlay must
    // appear.
    let entities: Vec<bevy::prelude::Entity> = walked.iter().map(|r| r.entity).collect();
    assert!(
        entities.contains(&overlay),
        "overlay should be visible via WorldTreeView (agent path), got {entities:?}",
    );
}

#[test]
fn scene_combines_tree_walk_and_edge_query_in_one_pass() {
    let mut app = test_app();
    let (root, rect, text, _edge) = spawn_fixture(app.world_mut());

    // For each descendant of `root`, count outgoing entity-edges.
    let edge_counts: Vec<(Entity, usize)> = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            scene
                .tree
                .children(root)
                .into_iter()
                .map(|child| (child, scene.graph.out_degree(child)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    // rect has 1 outgoing (the rect→text edge); text has 0.
    let by_entity: std::collections::HashMap<Entity, usize> = edge_counts.into_iter().collect();
    assert_eq!(by_entity.get(&rect).copied(), Some(1));
    assert_eq!(by_entity.get(&text).copied(), Some(0));
}
