//! `kyoso_agent` demo binary.
//!
//! Builds a small scene, walks it through every agent tool, and
//! prints what an agent would see:
//!
//! 1. Full LLM-shaped JSON descriptor (`describe`).
//! 2. Per-variant listings (`list_frames` / `list_rectangles` / `list_texts`).
//! 3. Subtree walks resolved to `NodeRef::{Replicated, Local}`.
//! 4. Typed sum-type subtree walk (`subtree_typed`).
//! 5. Per-entity inspection (`inspect`) — component-name dump.
//! 6. Pattern matching — find every `Frame → Text` edge in the scene.
//!
//! Run with `cargo run -p kyoso_agent --bin demo`.

use kyoso_agent::{spawn_demo_scene, SceneAgent};
use kyoso_core::{Frame, Text};
use kyoso_graph::traversal::TraversalQuery;

fn main() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());

    println!("── 1. describe() — LLM-shaped scene descriptor ──");
    let descriptor = agent.describe();
    println!(
        "{}\n",
        serde_json::to_string_pretty(&descriptor).unwrap_or_else(|_| "<serialize failed>".into())
    );

    println!("── 2. list_frames() / list_rectangles() / list_texts() ──");
    let frames = agent.list_frames();
    println!("frames: {}", frames.len());
    for (e, data) in &frames {
        println!("  {e:?}  name={:?}", data.frame.name);
    }
    let rects = agent.list_rectangles();
    println!("rectangles: {}", rects.len());
    for (e, _data) in &rects {
        println!("  {e:?}");
    }
    let texts = agent.list_texts();
    println!("texts: {}", texts.len());
    for (e, data) in &texts {
        println!("  {e:?}  content={:?}", data.text.content);
    }
    println!();

    println!("── 3. subtree() from root — NodeRef-resolved rows ──");
    let rows = agent.subtree(ents.root, TraversalQuery::new());
    for r in &rows {
        println!(
            "  depth={} entity={:?}  id={:?}",
            r.depth, r.entity, r.id
        );
    }
    println!();

    println!("── 4. subtree_typed::<SceneNode>() from root — closed-sum dispatch ──");
    let typed_rows = agent.subtree_typed(ents.root, TraversalQuery::new());
    for (row, node) in &typed_rows {
        let kind = match node {
            kyoso_core::Node::Frame(d) => format!("Frame name={:?}", d.frame.name),
            kyoso_core::Node::Rectangle(_) => "Rectangle".into(),
            kyoso_core::Node::Text(d) => format!("Text content={:?}", d.text.content),
        };
        println!("  depth={} {:?}  {}", row.depth, row.id, kind);
    }
    println!();

    println!("── 5. inspect(header) ──");
    let report = agent.inspect(ents.header);
    println!(
        "  entity={:?}  node={}",
        report.entity,
        match &report.node {
            Some(kyoso_core::Node::Frame(d)) => format!("Frame name={:?}", d.frame.name),
            Some(kyoso_core::Node::Rectangle(_)) => "Rectangle".into(),
            Some(kyoso_core::Node::Text(_)) => "Text".into(),
            None => "(unmatched)".into(),
        }
    );
    println!("  components ({}):", report.component_names.len());
    for name in &report.component_names {
        println!("    {name}");
    }
    println!();

    println!("── 6. find_matches — Frame → Text cross-edges ──");
    let mut builder = SceneAgent::pattern_builder();
    // We need to close over Bevy queries to predicate by component;
    // for this demo we approximate by anchoring & inspecting after-
    // wards. Run the pattern unconstrained (any A → any B) and then
    // filter rows where source is a Frame and target is a Text.
    let a = builder.node(|_| true);
    let b = builder.node(|_| true);
    let _e = builder.edge(a, b);
    let pattern = builder.build();
    let matches = agent.find_matches(&pattern);
    let mut frame_to_text = 0usize;
    for m in &matches {
        let src = m.node(a);
        let dst = m.node(b);
        let src_is_frame = agent.scene_world().read_as::<Frame>(src).is_some();
        let dst_is_text = agent.scene_world().read_as::<Text>(dst).is_some();
        if src_is_frame && dst_is_text {
            println!("  Frame {src:?} → Text {dst:?}");
            frame_to_text += 1;
        }
    }
    println!("  total Frame→Text matches: {frame_to_text}");
    println!("  total A→B matches (no filter): {}", matches.len());
    println!();

    println!("✓ demo complete");
    let _ = ents;
}
