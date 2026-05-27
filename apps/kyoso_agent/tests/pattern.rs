//! Agent pattern matching against the demo scene's entity-edge graph.
//!
//! The demo fixture has exactly one entity-edge — `label → body_caption`
//! — so we have a fixed expected match for each pattern shape.

use kyoso_agent::{
    spawn_demo_scene, NodePattern, NodeTarget, PatternSpec, SceneAgent,
};
use kyoso_core::NodeKind;

#[test]
fn pattern_spec_finds_any_to_any_edges() {
    let mut agent = SceneAgent::new();
    let _ents = spawn_demo_scene(agent.scene_world());

    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::any());
    let b = spec.add_node(NodePattern::any());
    spec.add_edge(a, b);

    let matches = agent.r#match(&spec);
    assert_eq!(matches.len(), 1);
    let edge = &matches[0].edges[0];
    assert_eq!(edge.from.path.to_string(), "/Root/Header/Title");
    assert_eq!(edge.to.path.to_string(), "/Root/[b]/Caption");
}

#[test]
fn pattern_spec_kind_filter_post_filters_matches() {
    let mut agent = SceneAgent::new();
    let _ents = spawn_demo_scene(agent.scene_world());

    // Only Frame→Text — the demo's single edge is Text→Text, so this
    // must return zero.
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::of_kind(NodeKind::Frame));
    let b = spec.add_node(NodePattern::of_kind(NodeKind::Text));
    spec.add_edge(a, b);
    assert_eq!(agent.r#match(&spec).len(), 0);

    // Text→Text must match the demo's one edge.
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::of_kind(NodeKind::Text));
    let b = spec.add_node(NodePattern::of_kind(NodeKind::Text));
    spec.add_edge(a, b);
    assert_eq!(agent.r#match(&spec).len(), 1);
}

#[test]
fn pattern_spec_name_filter_matches_frame_name() {
    let mut agent = SceneAgent::new();
    let _ents = spawn_demo_scene(agent.scene_world());

    // Title is the Text node feeding the edge — exact-name filter
    // matches it.
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::any().with_name("Title"));
    let b = spec.add_node(NodePattern::any());
    spec.add_edge(a, b);
    assert_eq!(agent.r#match(&spec).len(), 1);
}

#[test]
fn pattern_spec_anchor_restricts_search_to_a_specific_entity() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    // Anchor a at root — root has no outgoing entity-edges, expect 0.
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::any());
    let b = spec.add_node(NodePattern::any());
    spec.add_edge(a, b);
    spec.add_anchor(a, NodeTarget::Entity(ents.root));
    assert_eq!(agent.r#match(&spec).len(), 0);

    // Anchor a at label — label has the one outgoing SceneEdge.
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::any());
    let b = spec.add_node(NodePattern::any());
    spec.add_edge(a, b);
    spec.add_anchor(a, NodeTarget::Entity(ents.label));
    assert_eq!(agent.r#match(&spec).len(), 1);
}

#[test]
fn pattern_spec_round_trips_through_json() {
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::of_kind(NodeKind::Frame).with_name("Header"));
    let b = spec.add_node(NodePattern::any());
    spec.add_edge(a, b);

    let json = serde_json::to_string(&spec).expect("serialize");
    let back: PatternSpec = serde_json::from_str(&json).expect("deserialize");
    // Spot-check the round-trip preserved enough for the matcher to
    // behave identically.
    assert_eq!(back.nodes.len(), 2);
    assert_eq!(back.nodes[0].kind, Some(NodeKind::Frame));
    assert_eq!(back.nodes[0].name.as_deref(), Some("Header"));
    assert_eq!(back.edges.len(), 1);
    assert_eq!(back.edges[0].from, 0);
    assert_eq!(back.edges[0].to, 1);
}

#[test]
fn match_refs_projects_entity_bindings_into_noderef_space() {
    let mut agent = SceneAgent::new();
    let _ents = spawn_demo_scene(agent.scene_world());

    let mut builder = SceneAgent::pattern_builder();
    let a = builder.node(|_| true);
    let b = builder.node(|_| true);
    let e = builder.edge(a, b);
    let pattern = builder.build();

    let matches = agent.find_matches(&pattern);
    assert_eq!(matches.len(), 1);

    let refs = agent.match_refs(&matches[0]);
    // Two pattern nodes → two NodeRefs.
    assert_eq!(refs.nodes.len(), 2);
    assert_eq!(refs.edges.len(), 1);

    // The pattern's edge maps the label → body_caption SceneEdge.
    let edge_ref = refs.edge(e);
    assert_eq!(edge_ref.from.path.to_string(), "/Root/Header/Title");
    assert_eq!(edge_ref.to.path.to_string(), "/Root/[b]/Caption");
    // a and b match the edge endpoints.
    assert_eq!(refs.node(a).path, edge_ref.from.path);
    assert_eq!(refs.node(b).path, edge_ref.to.path);
}

#[test]
fn one_edge_pattern_yields_the_single_label_to_caption_edge() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let mut builder = SceneAgent::pattern_builder();
    let a = builder.node(|_| true);
    let b = builder.node(|_| true);
    let _e = builder.edge(a, b);
    let pattern = builder.build();

    let matches = agent.find_matches(&pattern);

    // Exactly one entity-edge in the fixture.
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m.node(a), ents.label);
    assert_eq!(m.node(b), ents.body_caption);
}

#[test]
fn anchored_pattern_to_unmatched_source_yields_nothing() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    // Anchor the source at `root` — root has no outgoing entity-edges,
    // so this should produce zero matches.
    let mut builder = SceneAgent::pattern_builder();
    let a = builder.node(|_| true);
    let b = builder.node(|_| true);
    builder.anchor(a, ents.root);
    let _e = builder.edge(a, b);
    let pattern = builder.build();

    let matches = agent.find_matches(&pattern);
    assert!(matches.is_empty(), "root has no outgoing edges; got {matches:?}");
}

#[test]
fn anchored_at_label_finds_the_caption() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    let mut builder = SceneAgent::pattern_builder();
    let a = builder.node(|_| true);
    let b = builder.node(|_| true);
    builder.anchor(a, ents.label);
    let _e = builder.edge(a, b);
    let pattern = builder.build();

    let matches = agent.find_matches(&pattern);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].node(b), ents.body_caption);
}
