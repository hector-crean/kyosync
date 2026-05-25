//! Test the `import_canvas` adapter against a hand-built figma_api fixture.
//!
//! Building the fixture in Rust (rather than loading a JSON file) keeps
//! the test self-contained — no schema-version drift, the figma-api
//! crate's required-field list is enforced by the type system.
//!
//! Fixture: a Canvas containing one Frame, which contains a Rectangle,
//! a Text, and one unsupported variant (Vector) that's logged-and-
//! skipped.

use std::collections::HashMap;

use bevy::prelude::*;
use figma_api::models;
use figma_api::models::{BlendMode, Effect, FlowStartingPoint, Paint, PrototypeDevice, Rgba};
use kyoso_core::{Frame, Rectangle, SceneNode, Text};
use kyoso_figma::import::import_canvas;

fn make_rgba(r: f64, g: f64, b: f64, a: f64) -> Rgba {
    Rgba { r, g, b, a }
}

fn make_solid_paint(color: Rgba) -> Paint {
    let mut solid = models::SolidPaint::default();
    solid.color = Box::new(color);
    Paint::SolidPaint(Box::new(solid))
}

fn make_canvas() -> models::CanvasNode {
    // ---- Rectangle ----
    let mut rect = models::RectangleNode::new(
        "rect-1".into(),
        "Bg".into(),
        models::rectangle_node::ScrollBehavior::Scrolls,
        BlendMode::Normal,
        vec![make_solid_paint(make_rgba(0.0, 0.5, 1.0, 1.0))],
        Vec::<Effect>::new(),
    );
    rect.corner_radius = Some(8.0);

    // ---- Text ----
    let mut text_style = models::TypeStyle::default();
    text_style.font_family = Some("Helvetica".into());
    text_style.font_size = Some(18.0);
    text_style.font_weight = Some(700.0);

    let text = models::TextNode::new(
        "text-1".into(),
        "Heading".into(),
        models::text_node::ScrollBehavior::Scrolls,
        BlendMode::Normal,
        Vec::<Paint>::new(),
        Vec::<Effect>::new(),
        "Hello, kyoso".into(),
        text_style,
        Vec::<f64>::new(),
        HashMap::new(),
        Vec::<models::text_node::LineTypes>::new(),
        Vec::<f64>::new(),
    );

    // ---- Vector (unsupported) ----
    let vector = models::VectorNode::new(
        "vec-1".into(),
        "Path".into(),
        models::vector_node::ScrollBehavior::Scrolls,
        BlendMode::Normal,
        Vec::<Paint>::new(),
        Vec::<Effect>::new(),
    );

    // ---- Frame ----
    let mut frame = models::FrameNode::new(
        "frame-1".into(),
        "Header".into(),
        models::frame_node::ScrollBehavior::Scrolls,
        BlendMode::Normal,
        vec![
            models::SubcanvasNode::Rectangle(Box::new(rect)),
            models::SubcanvasNode::Text(Box::new(text)),
            models::SubcanvasNode::Vector(Box::new(vector)),
        ],
        true, // clips_content
        vec![make_solid_paint(make_rgba(0.95, 0.95, 0.95, 1.0))],
        Vec::<Effect>::new(),
    );
    frame.layout_mode =
        Some(figma_api::models::frame_node::LayoutMode::Horizontal);

    // ---- Canvas ----
    models::CanvasNode::new(
        "canvas-1".into(),
        "Page 1".into(),
        models::canvas_node::ScrollBehavior::Scrolls,
        vec![models::SubcanvasNode::Frame(Box::new(frame))],
        make_rgba(1.0, 1.0, 1.0, 1.0),
        None,
        Vec::<FlowStartingPoint>::new(),
        PrototypeDevice::default(),
    )
}

#[test]
fn import_supported_nodes_and_skip_unsupported() {
    let canvas = make_canvas();
    let mut world = World::new();

    let entities = world
        .run_system_cached_with(import_canvas_system, canvas)
        .unwrap();

    assert_eq!(
        entities.len(),
        3,
        "expected exactly 3 spawned entities (Frame + Rectangle + Text); \
         Vector is unsupported and should be skipped",
    );

    // ---- Frame ----
    let frame = world
        .query::<&Frame>()
        .iter(&world)
        .next()
        .expect("a Frame was imported");
    assert_eq!(frame.name, "Header");
    assert!(frame.clips_content);
    assert_eq!(frame.layout_mode, kyoso_core::LayoutMode::Horizontal);
    assert_eq!(frame.fills.len(), 1);

    // ---- Rectangle ----
    let rect = world
        .query::<&Rectangle>()
        .iter(&world)
        .next()
        .expect("a Rectangle was imported");
    assert!((rect.corner_radius - 8.0).abs() < 0.001);
    assert_eq!(rect.fills.len(), 1);

    // ---- Text ----
    let text = world
        .query::<&Text>()
        .iter(&world)
        .next()
        .expect("a Text was imported");
    assert_eq!(text.content, "Hello, kyoso");
    assert_eq!(text.style.font_family, "Helvetica");
    assert!((text.style.font_size - 18.0).abs() < 0.001);
    assert_eq!(text.style.font_weight, 700);

    // Every spawned entity carries the structural SceneNode marker.
    let scene_nodes = world.query::<&SceneNode>().iter(&world).count();
    assert_eq!(scene_nodes, 3);
}

/// Wrap `import_canvas` so it runs through `world.run_system_cached_with`
/// — the simplest way to get a `Commands` instance against a fresh
/// `World` without spinning up a full `App`.
fn import_canvas_system(
    In(canvas): In<models::CanvasNode>,
    commands: Commands,
) -> Vec<Entity> {
    import_canvas(commands, &canvas)
}
