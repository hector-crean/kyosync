//! Headless demo of the [`kyoso_graph`] tree primitive.
//!
//! Builds a tiny Figma-shaped scene tree, then exercises every tree-shaped
//! [`GraphCommand`] variant: `InsertChild`, `Reparent`, `MoveSibling`. Each
//! phase prints the current tree (children sorted by [`OrderKey`]).
//!
//! Run with: `cargo run -p kyoso_graph --example scene_tree`

use bevy::ecs::message::Messages;
use bevy::prelude::*;

use kyoso_graph::components::{EdgeTo, OutgoingEdges};
use kyoso_graph::tree::{OrderKey, TreeEdge, TreePlugin};
use kyoso_graph::{GraphCommand, GraphManagerPlugin};

// ---------------------------------------------------------------------------
// Domain types — the consumer's own node/edge components.
// ---------------------------------------------------------------------------

#[derive(Component, Debug, Default, Clone)]
struct SceneNode {
    name: String,
    kind: NodeKind,
}

impl SceneNode {
    fn new(name: &str, kind: NodeKind) -> Self {
        Self {
            name: name.to_string(),
            kind,
        }
    }
}

#[derive(Default, Debug, Clone, Copy)]
enum NodeKind {
    #[default]
    Frame,
    Group,
    Rect,
    Text,
}

#[derive(Component, Default, Debug, Clone, Copy)]
struct SceneEdge;

// ---------------------------------------------------------------------------
// Demo
// ---------------------------------------------------------------------------

struct SceneEntities {
    root: Entity,
    body: Entity,
    logo: Entity,
    title: Entity,
    tagline: Entity,
    label: Entity,
}

fn main() {
    let mut app = App::new();
    app.add_plugins((
        GraphManagerPlugin::<SceneNode, SceneEdge>::new(),
        TreePlugin,
    ));

    let entities = build_initial(&mut app);
    app.update();
    println!("=== Phase 1: initial tree ===");
    print_tree(app.world(), entities.root, 0);

    reparent_tagline_into_body(&mut app, &entities);
    app.update();
    println!("\n=== Phase 2: reparent `tagline` from header → body ===");
    print_tree(app.world(), entities.root, 0);

    move_logo_after_title(&mut app, &entities);
    app.update();
    println!("\n=== Phase 3: reorder `logo` after `title` within header ===");
    print_tree(app.world(), entities.root, 0);
}

// ---------------------------------------------------------------------------
// Phase setup
// ---------------------------------------------------------------------------

fn build_initial(app: &mut App) -> SceneEntities {
    let world = app.world_mut();
    let root = world.spawn(SceneNode::new("root", NodeKind::Frame)).id();
    let header = world.spawn(SceneNode::new("header", NodeKind::Group)).id();
    let body = world.spawn(SceneNode::new("body", NodeKind::Group)).id();
    let logo = world.spawn(SceneNode::new("logo", NodeKind::Rect)).id();
    let title = world.spawn(SceneNode::new("title", NodeKind::Text)).id();
    let tagline = world.spawn(SceneNode::new("tagline", NodeKind::Text)).id();
    let button = world.spawn(SceneNode::new("button", NodeKind::Rect)).id();
    let label = world.spawn(SceneNode::new("label", NodeKind::Text)).id();

    // Pick a few well-spaced fractional keys by hand. Same alphabet as
    // OrderKey::between, so all between-derived inserts will fit cleanly.
    let k_n = OrderKey("n".into()); // first slot
    let k_q = OrderKey("q".into()); // second slot
    let k_s = OrderKey("s".into()); // third slot

    let mut msgs = world.resource_mut::<Messages<GraphCommand>>();
    // root → [header, body]
    msgs.write(GraphCommand::InsertChild {
        parent: root,
        child: header,
        position: k_n.clone(),
    });
    msgs.write(GraphCommand::InsertChild {
        parent: root,
        child: body,
        position: k_q.clone(),
    });
    // header → [logo, title, tagline]
    msgs.write(GraphCommand::InsertChild {
        parent: header,
        child: logo,
        position: k_n.clone(),
    });
    msgs.write(GraphCommand::InsertChild {
        parent: header,
        child: title,
        position: k_q.clone(),
    });
    msgs.write(GraphCommand::InsertChild {
        parent: header,
        child: tagline,
        position: k_s,
    });
    // body → [button, label]
    msgs.write(GraphCommand::InsertChild {
        parent: body,
        child: button,
        position: k_n,
    });
    msgs.write(GraphCommand::InsertChild {
        parent: body,
        child: label,
        position: k_q,
    });

    SceneEntities {
        root,
        body,
        logo,
        title,
        tagline,
        label,
    }
}

fn reparent_tagline_into_body(app: &mut App, entities: &SceneEntities) {
    // Place tagline after the existing last child of body (label, key="q").
    let label_key = app
        .world()
        .get::<OrderKey>(entities.label)
        .expect("label has an OrderKey")
        .clone();
    let position = OrderKey::between(Some(&label_key), None);

    app.world_mut()
        .resource_mut::<Messages<GraphCommand>>()
        .write(GraphCommand::Reparent {
            child: entities.tagline,
            new_parent: entities.body,
            position,
        });
}

fn move_logo_after_title(app: &mut App, entities: &SceneEntities) {
    let title_key = app
        .world()
        .get::<OrderKey>(entities.title)
        .expect("title has an OrderKey")
        .clone();
    let position = OrderKey::between(Some(&title_key), None);

    app.world_mut()
        .resource_mut::<Messages<GraphCommand>>()
        .write(GraphCommand::MoveSibling {
            child: entities.logo,
            position,
        });
}

// ---------------------------------------------------------------------------
// Tree print helper
// ---------------------------------------------------------------------------

fn print_tree(world: &World, entity: Entity, depth: usize) {
    let indent = "  ".repeat(depth);
    let scene = world.get::<SceneNode>(entity);
    let name = scene.map(|s| s.name.as_str()).unwrap_or("?");
    let kind = scene.map(|s| s.kind).unwrap_or_default();
    let key = world
        .get::<OrderKey>(entity)
        .map(|k| k.0.as_str())
        .unwrap_or("-");

    println!("{indent}{name} <{kind:?}> [key={key}]");

    let Some(out) = world.get::<OutgoingEdges>(entity) else {
        return;
    };
    let mut children: Vec<(Entity, OrderKey)> = out
        .iter()
        .filter_map(|edge| {
            world.get::<TreeEdge>(edge)?;
            let to = world.get::<EdgeTo>(edge)?;
            let k = world.get::<OrderKey>(to.0)?.clone();
            Some((to.0, k))
        })
        .collect();
    children.sort_by(|(_, a), (_, b)| a.cmp(b));
    for (child, _) in children {
        print_tree(world, child, depth + 1);
    }
}
