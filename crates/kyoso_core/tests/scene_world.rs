//! Integration tests for the `SceneWorld` binding-layer handle.
//!
//! Exercises the typed methods that the bare `WorldTreeView`
//! SystemParam can't service from inside a regular system:
//! `iter_as::<V>`, `read_as::<V>`, `traverse_as::<V>`, and
//! `traverse_typed::<G>`. Plus a smoke for the cached-`SystemState`
//! `graph_view` path.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use kyoso_core::{
    Frame, FrameData, Node, NodeKind, Rectangle, RectangleData, SceneNode,
    SceneWorld, Text, TextData,
};
use kyoso_graph::traversal::{NodeRef, Order, TraversalQuery};
use kyoso_graph::tree::OrderKey;
use kyoso_graph_sync::EntityCrdtIndex;

/// Same fixture as `tests/traversal.rs`:
/// ```text
/// root (Frame, replicated)
/// ├── rect (Rectangle, replicated, OrderKey "a")
/// └── text (Text, NOT replicated, OrderKey "b")
/// ```
fn spawn_fixture(sw: &mut SceneWorld) -> (Entity, Entity, Entity) {
    let (root, rect, text) = sw
        .world_mut()
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

    // Populate the index for `root` + `rect` only — `text` surfaces as
    // `NodeRef::Local`.
    let mut index = EntityCrdtIndex::default();
    index.bind_node(root, kyoso_crdt::CrdtId::new(1, 1));
    index.bind_node(rect, kyoso_crdt::CrdtId::new(1, 2));
    sw.world_mut().insert_resource(index);

    (root, rect, text)
}

#[test]
fn iter_as_frame_yields_only_frames() {
    let mut sw = SceneWorld::new();
    let (root, _rect, _text) = spawn_fixture(&mut sw);

    let frames = sw.iter_as::<Frame>();

    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, root);
    // The materialised Data is `FrameData { frame, size }` — the
    // inner `frame.name` is the default empty string.
    assert_eq!(frames[0].1.frame.name, "");
}

#[test]
fn iter_as_rectangle_and_text_each_yield_one() {
    let mut sw = SceneWorld::new();
    let _ = spawn_fixture(&mut sw);

    let rects = sw.iter_as::<Rectangle>();
    let texts = sw.iter_as::<Text>();

    assert_eq!(rects.len(), 1);
    assert_eq!(texts.len(), 1);
}

#[test]
fn read_as_returns_none_for_wrong_variant() {
    let mut sw = SceneWorld::new();
    let (root, rect, text) = spawn_fixture(&mut sw);

    // root is a Frame — Frame OK, Rectangle/Text not.
    assert!(sw.read_as::<Frame>(root).is_some());
    assert!(sw.read_as::<Rectangle>(root).is_none());
    assert!(sw.read_as::<Text>(root).is_none());

    // rect is a Rectangle.
    assert!(sw.read_as::<Rectangle>(rect).is_some());
    assert!(sw.read_as::<Frame>(rect).is_none());

    // text is a Text.
    assert!(sw.read_as::<Text>(text).is_some());
}

#[test]
fn traverse_as_frame_walks_hierarchy_and_filters_to_typed_rows() {
    let mut sw = SceneWorld::new();
    let (root, _rect, _text) = spawn_fixture(&mut sw);

    let rows = sw.traverse_as::<Frame>(
        &TraversalQuery::new().start_at(root).order(Order::Bfs),
    );

    // Only the root Frame matches the V=Frame projection; rect + text
    // are dropped.
    assert_eq!(rows.len(), 1);
    let (worldref, data) = &rows[0];
    assert_eq!(worldref.entity, root);
    assert_eq!(worldref.depth, 0);
    assert_eq!(data.frame.name, "");
}

#[test]
fn traverse_typed_scene_node_yields_sum_type_rows_with_correct_kinds() {
    let mut sw = SceneWorld::new();
    let (root, rect, text) = spawn_fixture(&mut sw);

    let rows = sw.traverse_typed::<SceneNode>(
        &TraversalQuery::new().start_at(root).order(Order::Bfs),
    );

    // All three nodes round-trip through the (Frame, Rectangle, Text)
    // tuple impl.
    assert_eq!(rows.len(), 3);

    // Map entity → variant tag to assert the closed-sum dispatch.
    let by_entity: std::collections::HashMap<Entity, NodeKind> = rows
        .iter()
        .map(|(r, n)| {
            let kind = match n {
                Node::Frame(_) => NodeKind::Frame,
                Node::Rectangle(_) => NodeKind::Rectangle,
                Node::Text(_) => NodeKind::Text,
            };
            (r.entity, kind)
        })
        .collect();

    assert_eq!(by_entity.get(&root), Some(&NodeKind::Frame));
    assert_eq!(by_entity.get(&rect), Some(&NodeKind::Rectangle));
    assert_eq!(by_entity.get(&text), Some(&NodeKind::Text));
}

#[test]
fn graph_view_passthrough_resolves_node_refs() {
    let mut sw = SceneWorld::new();
    let (root, _rect, text) = spawn_fixture(&mut sw);

    // Go through the wrapper's `traverse` (which uses the cached
    // SystemState) — covers the borrow shape end-to-end.
    let rows = sw.traverse(&TraversalQuery::new().start_at(root).order(Order::Bfs));

    assert_eq!(rows.len(), 3);
    // root is bound in EntityCrdtIndex → Replicated.
    assert!(matches!(rows[0].id, NodeRef::Replicated(_)));
    // text is unbound → Local fallback with `Entity::to_bits()`.
    let text_row = rows.iter().find(|r| r.entity == text).expect("text row present");
    match text_row.id {
        NodeRef::Local(bits) => assert_eq!(bits, text.to_bits()),
        other => panic!("expected NodeRef::Local for text, got {other:?}"),
    }
}

#[test]
fn cached_system_state_survives_multiple_calls() {
    let mut sw = SceneWorld::new();
    let (root, _rect, _text) = spawn_fixture(&mut sw);

    // Re-run the same query four times against the cached SystemState.
    // Asserts the second / third / fourth call still see the same world.
    for _ in 0..4 {
        let rows = sw.traverse(&TraversalQuery::new().start_at(root));
        assert_eq!(rows.len(), 3);
    }
}

#[test]
fn scene_descriptor_carries_per_variant_data() {
    let mut sw = SceneWorld::new();
    let _ = spawn_fixture(&mut sw);

    let descriptor = sw.scene_descriptor();

    assert_eq!(descriptor.roots.len(), 1, "exactly one tree root");
    let root_desc = &descriptor.roots[0];
    assert_eq!(root_desc.node_type, "frame");
    assert!(root_desc.data.is_some(), "root has typed data");
    assert_eq!(root_desc.children.len(), 2);

    let kinds: Vec<&str> = root_desc.children.iter().map(|c| c.node_type.as_str()).collect();
    assert!(kinds.contains(&"rectangle"));
    assert!(kinds.contains(&"text"));
    for child in &root_desc.children {
        assert!(child.data.is_some(), "child has typed data");
        assert_eq!(child.depth, 1);
    }
}
