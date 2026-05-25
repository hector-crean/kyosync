//! Agent pattern matching against the demo scene's entity-edge graph.
//!
//! The demo fixture has exactly one entity-edge — `label → body_caption`
//! — so we have a fixed expected match for each pattern shape.

use kyoso_agent::{spawn_demo_scene, SceneAgent};

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
