//! Validates that [`SceneRead`] and [`SceneMutate`] are object-safe
//! and that `SceneAgent` round-trips through them — the property the
//! FFI / MCP wire codegen will rely on.

use kyoso_agent::{
    spawn_demo_scene, CreateSpec, MoveSpec, NewNode, NodeTarget, ScanOpts, SceneAgent,
    SceneMutate, SceneRead, UpdatePatch, WalkOpts, WatchOpts,
};
use kyoso_core::{Frame, FrameData};

#[test]
fn scene_read_is_object_safe_and_routes_through_agent() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());
    agent.scene_world().update();

    // Coerce to `&mut dyn SceneRead` — this requires object-safety.
    let reader: &mut dyn SceneRead = &mut agent;

    let index = reader.scan(ScanOpts::default());
    assert!(index.catalog.total_nodes >= 5);

    let report = reader.inspect(NodeTarget::Entity(ents.header));
    assert!(report.variant.is_some());

    let walk = reader.walk(NodeTarget::Entity(ents.root), WalkOpts::default());
    assert!(!walk.rows.is_empty());

    // Watch must work too.
    let _page = reader.watch(None, WatchOpts::default());
}

#[test]
fn scene_mutate_is_object_safe_and_routes_through_agent() {
    let mut agent = SceneAgent::new();
    let _ents = spawn_demo_scene(agent.scene_world());
    agent.scene_world().update();

    let mutator: &mut dyn SceneMutate = &mut agent;

    // Create a fresh root.
    let created = mutator
        .create(CreateSpec {
            data: NewNode::Frame(FrameData {
                frame: Frame {
                    name: "ViaTrait".into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            parent: None,
            position: None,
        })
        .expect("create");
    assert_eq!(created.node.path.to_string(), "/ViaTrait");

    // Update — same NodeRef.
    let updated = mutator
        .update(
            NodeTarget::Ref(created.node.clone()),
            UpdatePatch::default().with_frame_name("RenamedViaTrait"),
        )
        .expect("update");
    assert_eq!(updated.node.path.to_string(), "/RenamedViaTrait");

    // Delete.
    let deleted = mutator
        .delete(NodeTarget::Ref(updated.node.clone()))
        .expect("delete");
    assert_eq!(deleted.node.path.to_string(), "/RenamedViaTrait");
}

#[test]
fn agent_satisfies_both_traits_simultaneously() {
    // The function signature is the test: it enforces that
    // `SceneAgent: SceneRead + SceneMutate` is a real bound the
    // compiler accepts. (`impl Trait + Trait` is the part that
    // confirms both traits are object-safe in combination.)
    fn use_both<T: SceneRead + SceneMutate>(t: &mut T) {
        let _idx = t.scan(ScanOpts::default());
        let _ = t.create(CreateSpec {
            data: NewNode::Frame(FrameData::default()),
            parent: None,
            position: None,
        });
    }

    let mut agent = SceneAgent::new();
    use_both(&mut agent);
}

#[test]
fn scene_view_closure_runs_with_both_tree_and_graph() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());
    agent.scene_world().update();

    let (tree_node_count, graph_edge_count) = agent.scene_view(|view| {
        // Tree: count all nodes reachable from root via children.
        let root_traversal = view.traverse_tree(
            &kyoso_agent::TraversalQuery::new().start_at(ents.root),
        );
        // Graph: count entity-edges.
        let edge_count = view.scene().graph.edge_count();
        (root_traversal.len(), edge_count)
    });

    assert_eq!(tree_node_count, 5);
    assert_eq!(graph_edge_count, 1);
}

#[test]
fn move_via_trait_relocates_subtree() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());
    agent.scene_world().update();

    let mutator: &mut dyn SceneMutate = &mut agent;
    let moved = mutator
        .r#move(MoveSpec {
            target: NodeTarget::Entity(ents.label),
            new_parent: Some(NodeTarget::Entity(ents.body)),
            position: kyoso_graph::tree::OrderKey("z".into()),
        })
        .expect("move");
    assert_eq!(moved.node.path.to_string(), "/Root/[b]/Title");
}
