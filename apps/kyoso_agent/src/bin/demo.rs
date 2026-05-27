//! `kyoso_agent` demo binary.
//!
//! Builds a small scene and walks it through every agent tool the
//! Rerun-style verb set offers — `scan`, `inspect`, `walk`, `navigate`,
//! `match`, and `watch`. Prints what an agent would see.
//!
//! Run with `cargo run -p kyoso_agent --bin demo`.

use kyoso_agent::{
    spawn_demo_scene, NavDir, NavEdgeFilter, NavOpts, NodePattern, PatternSpec, SceneAgent,
    ScanOpts, WalkOpts, WatchOpts,
};
use kyoso_core::NodeKind;

fn main() {
    let mut agent = SceneAgent::new();
    let ents = spawn_demo_scene(agent.scene_world());
    // Pump one frame so any change-detection systems flush events
    // and the watch buffer reflects the initial spawn.
    agent.scene_world().update();
    println!("session = {}", agent.session());
    println!();

    println!("── 1. scan() — catalog + outline ──");
    let index = agent.scan(ScanOpts::default());
    println!(
        "  catalog: total={} frames={} rectangles={} texts={} max_depth={}",
        index.catalog.total_nodes,
        index.catalog.kind_counts.get(&NodeKind::Frame).unwrap_or(&0),
        index
            .catalog
            .kind_counts
            .get(&NodeKind::Rectangle)
            .unwrap_or(&0),
        index.catalog.kind_counts.get(&NodeKind::Text).unwrap_or(&0),
        index.catalog.max_depth,
    );
    for root in &index.roots {
        print_outline(root, 1);
    }
    println!();

    println!("── 2. inspect(header) — typed variant + components ──");
    let report = agent.inspect(ents.header);
    println!(
        "  node = {}  variant = {}",
        report.node.path,
        match &report.variant {
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

    println!("── 3. walk(root) — depth-bounded, NodeRef rows ──");
    let walk = agent.walk(
        ents.root,
        WalkOpts {
            depth_limit: Some(2),
            ..Default::default()
        },
    );
    for row in &walk.rows {
        println!(
            "  depth={}  kind={:?}  path={}",
            row.depth, row.kind, row.node.path
        );
    }
    println!(
        "  cost: items={} work={}  truncated={}",
        walk.cost.estimated_items, walk.cost.estimated_work, walk.truncated,
    );
    println!();

    println!("── 4. navigate(label, Out, EntityEdgesOnly) — cross-frame edge ──");
    let neighbours = agent.navigate(
        ents.label,
        NavOpts {
            direction: NavDir::Out,
            edges: NavEdgeFilter::EntityEdgesOnly,
            ..Default::default()
        },
    );
    for n in &neighbours {
        println!("  → {}", n.path);
    }
    println!();

    println!("── 5. navigate(label, Upstream, TreeOnly) — ancestors ──");
    let ancestors = agent.navigate(
        ents.label,
        NavOpts {
            direction: NavDir::Upstream,
            edges: NavEdgeFilter::TreeOnly,
            ..Default::default()
        },
    );
    for n in &ancestors {
        println!("  ↑ {}", n.path);
    }
    println!();

    println!("── 6. r#match — every A → B in NodeRef space (PatternSpec) ──");
    let mut spec = PatternSpec::new();
    let a = spec.add_node(NodePattern::any());
    let b = spec.add_node(NodePattern::any());
    spec.add_edge(a, b);
    let matches = agent.r#match(&spec);
    for refs in &matches {
        let edge = &refs.edges[0];
        println!("  {} → {}", edge.from.path, edge.to.path);
    }
    println!("  total edges matched: {}", matches.len());
    println!();

    println!("── 7. watch(None) — baseline change stream after spawn ──");
    let page = agent.watch(None, WatchOpts::default());
    println!(
        "  generation = {}  changes = {}  buffer_overflow = {}",
        page.next_cursor.generation,
        page.changes.len(),
        page.buffer_overflow,
    );
    for c in page.changes.iter().take(6) {
        println!(
            "    {:?} ×{}  {}",
            c.kind, c.change_count, c.node.path,
        );
    }
    println!();

    println!("✓ demo complete");
    let _ = ents;
}

fn print_outline(node: &kyoso_agent::OutlineNode, indent: usize) {
    let pad = "  ".repeat(indent);
    let label = node.name.clone().unwrap_or_else(|| "<unnamed>".into());
    println!(
        "{pad}{label} ({:?})  depth={}  child_count={}  elided={}  cost={}/{}  path={}",
        node.kind,
        node.depth,
        node.child_count,
        node.elided_children,
        node.subtree_cost.estimated_items,
        node.subtree_cost.estimated_work,
        node.node.path,
    );
    for child in &node.children {
        print_outline(child, indent + 1);
    }
}
