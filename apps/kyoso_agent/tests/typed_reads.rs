//! Agent typed reads: `list_frames` / `list_rectangles` / `list_texts`
//! + closed-sum `subtree_typed`.

use kyoso_agent::{spawn_demo_scene, SceneAgent};
use kyoso_core::{Node, NodeKind};
use kyoso_graph::traversal::TraversalQuery;

#[test]
fn list_per_variant_returns_correct_counts_and_data() {
    let mut agent = SceneAgent::new();
    let _ = spawn_demo_scene(agent.scene_world());

    let frames = agent.list_frames();
    let rects = agent.list_rectangles();
    let texts = agent.list_texts();

    // Demo scene: 2 Frames (root + header), 1 Rectangle (body), 2 Texts
    // (label + body_caption).
    assert_eq!(frames.len(), 2);
    assert_eq!(rects.len(), 1);
    assert_eq!(texts.len(), 2);

    // Frame names should include "Root" and "Header".
    let names: Vec<&str> = frames.iter().map(|(_, d)| d.frame.name.as_str()).collect();
    assert!(names.contains(&"Root"));
    assert!(names.contains(&"Header"));

    // Text contents.
    let contents: Vec<&str> = texts.iter().map(|(_, d)| d.text.content.as_str()).collect();
    assert!(contents.contains(&"Title"));
    assert!(contents.contains(&"Caption"));
}

#[test]
fn subtree_typed_dispatches_closed_sum_correctly() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let rows = agent.subtree_typed(ents.root, TraversalQuery::new());

    // 5 nodes total in the demo scene.
    assert_eq!(rows.len(), 5);

    // Map entity → kind for an exhaustive check.
    let by_entity: std::collections::HashMap<_, _> = rows
        .iter()
        .map(|(r, n)| (r.entity, n.kind()))
        .collect();

    assert_eq!(by_entity.get(&ents.root).copied(), Some(NodeKind::Frame));
    assert_eq!(by_entity.get(&ents.header).copied(), Some(NodeKind::Frame));
    assert_eq!(by_entity.get(&ents.body).copied(), Some(NodeKind::Rectangle));
    assert_eq!(by_entity.get(&ents.label).copied(), Some(NodeKind::Text));
    assert_eq!(by_entity.get(&ents.body_caption).copied(), Some(NodeKind::Text));

    // Materialised data still flows through.
    for (_, node) in &rows {
        match node {
            Node::Frame(d) => assert!(d.frame.name == "Root" || d.frame.name == "Header"),
            Node::Rectangle(_) => {}
            Node::Text(d) => assert!(d.text.content == "Title" || d.text.content == "Caption"),
        }
    }
}

#[test]
fn inspect_reports_components_and_typed_node_for_known_entity() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let report = agent.inspect(ents.header);

    // The component-name dump is non-empty. (Bevy stores readable
    // type names behind its `debug` cargo feature; without it, names
    // are placeholder strings. Either way the list has one entry per
    // archetype component — that's the contract agents rely on.)
    assert!(
        !report.component_names.is_empty(),
        "inspect should always return at least one component for a live entity",
    );

    // The typed node materialised as a Frame — this is the real
    // agent-relevant assertion.
    match report.variant {
        Some(Node::Frame(data)) => assert_eq!(data.frame.name, "Header"),
        other => panic!("expected Frame, got {other:?}"),
    }
}

#[test]
fn inspect_returns_empty_for_unknown_entity() {
    let mut agent = SceneAgent::new();
    let _ = spawn_demo_scene(agent.scene_world());

    let bogus = bevy::prelude::Entity::from_raw_u32(99_999).unwrap();
    let report = agent.inspect(bogus);

    assert!(report.variant.is_none());
    assert!(report.entity_bits.is_none());
    assert!(report.component_names.is_empty());
}
