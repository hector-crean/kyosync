//! Agent LLM-shaped scene description — confirms the JSON shape an
//! agent would consume.

use kyoso_agent::{spawn_demo_scene, SceneAgent};

#[test]
fn describe_returns_a_single_tree_root_with_nested_children() {
    let mut agent = SceneAgent::new();
    let _ = spawn_demo_scene(agent.scene_world());

    let desc = agent.describe();

    // Demo scene: a single tree root (`Root`) carrying two children.
    assert_eq!(desc.metadata.root_count, 1);
    assert_eq!(desc.roots.len(), 1);

    let root = &desc.roots[0];
    assert_eq!(root.node_type, "frame");
    assert!(root.data.is_some(), "root carries typed data");

    // Root's two children: header (Frame) + body (Rectangle).
    let kinds: Vec<&str> = root.children.iter().map(|c| c.node_type.as_str()).collect();
    assert_eq!(root.children.len(), 2);
    assert!(kinds.contains(&"frame"));
    assert!(kinds.contains(&"rectangle"));
}

#[test]
fn describe_walks_all_depth_levels() {
    let mut agent = SceneAgent::new();
    let _ = spawn_demo_scene(agent.scene_world());

    let desc = agent.describe();

    // Demo tree depth: root(0) → header(1) → label(2). Max depth = 2.
    assert_eq!(desc.metadata.max_depth, 2);
}

#[test]
fn describe_round_trips_through_serde_json() {
    let mut agent = SceneAgent::new();
    let _ = spawn_demo_scene(agent.scene_world());

    let desc = agent.describe();
    let json = serde_json::to_string(&desc).expect("serialise");
    // Variant tags from the serde-tagged `Node` enum should appear in
    // the descriptor's `data` payloads.
    assert!(json.contains("\"kind\":\"frame\""), "missing frame tag in {json}");
    assert!(json.contains("\"kind\":\"rectangle\""), "missing rectangle tag in {json}");
    assert!(json.contains("\"kind\":\"text\""), "missing text tag in {json}");
}
