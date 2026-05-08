//! Figma file import adapter — read-only, best-effort.
//!
//! Walks a `figma_api::CanvasNode` and spawns kyoso_figma Bevy
//! components for each supported node kind (Frame / Rectangle / Text).
//! Unsupported variants are logged via `tracing::warn!` and skipped;
//! the spawned tree may have gaps where unsupported nodes were skipped
//! (per plan doc Part XI §XI.4).
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_figma::import::import_canvas;
//!
//! fn import_system(mut commands: Commands, /* canvas source */) {
//!     let canvas: figma_api::models::CanvasNode = /* ... */;
//!     let _entities = import_canvas(commands.reborrow(), &canvas);
//! }
//! ```
//!
//! ## Hierarchy
//!
//! Children attach to their parent via `kyoso_graph::tree::TreeParent`
//! + `OrderKey`. `OrderKey`s are generated as monotonically-increasing
//! strings during the walk (`"a"`, `"b"`, ...) which is fine for an
//! import — the resulting tree's sibling order matches the Figma
//! source's iteration order.
//!
//! ## What is *not* converted
//!
//! - `relative_transform` and `absolute_bounding_box` → only
//!   `Transform::default()` is set on imported entities for now. Real
//!   conversion is a follow-up — Figma's transform is column-major and
//!   includes a 2D affine in 2×3 form; mapping to Bevy's 4×4 column-
//!   major requires care. Falling back to identity keeps the smoke
//!   path passing while we land the structure.
//! - `Size` → derived from the node's `size: Vector` field if present.
//! - Pattern paints, image paints (just the wire-shape — no asset
//!   plumbing yet).

#![allow(clippy::needless_pass_by_value)]

use bevy::prelude::*;
use figma_api::models::{
    CanvasNode, FrameNode, Paint as FigmaPaint, RectangleNode, SubcanvasNode, TextNode,
};
use kyoso_graph::tree::{OrderKey, TreeParent};

use crate::frame::LayoutMode;
use crate::paint::{GradientStop, GradientType, ImageScaleMode, Paint};
use crate::typestyle::TypeStyle;
use crate::walker::{NodeContext, NodeVisitor, Walker};
use crate::{FigmaNode, Frame, Rectangle, Size, Text};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Walk a `CanvasNode` and spawn Bevy entities for every supported
/// child. Returns the entities spawned, in walk order.
pub fn import_canvas(commands: Commands, canvas: &CanvasNode) -> Vec<Entity> {
    let visitor = KyosoVisitor::new(commands);
    let walker = Walker::new(visitor);
    walker.walk_canvas(canvas).spawned
}

// ---------------------------------------------------------------------------
// KyosoVisitor
// ---------------------------------------------------------------------------

struct KyosoVisitor<'w, 's> {
    commands: Commands<'w, 's>,
    /// Stack of parent entities for the current container's children.
    /// Pushed in `enter_container`, popped in `exit_container`.
    parent_stack: Vec<Entity>,
    /// All entities spawned during the walk, in DFS order.
    spawned: Vec<Entity>,
    /// Per-parent sibling counter: how many children have been spawned
    /// under the current parent so far. Drives `OrderKey` generation.
    sibling_counters: Vec<usize>,
}

impl<'w, 's> KyosoVisitor<'w, 's> {
    fn new(commands: Commands<'w, 's>) -> Self {
        Self {
            commands,
            parent_stack: Vec::new(),
            spawned: Vec::new(),
            // One stack frame for the canvas (the implicit root).
            sibling_counters: vec![0],
        }
    }

    fn current_parent(&self) -> Option<Entity> {
        self.parent_stack.last().copied()
    }

    /// Generate a fresh `OrderKey` for a child of the current parent.
    /// Uses lowercase letters (`a`, `b`, ..., `z`, `aa`, `ab`, ...) so
    /// the kyoso_graph fractional-index machinery has a workable
    /// initial ordering. For an import we just need stable monotonic
    /// keys; collisions across imports aren't a concern.
    fn next_order_key(&mut self) -> OrderKey {
        let counter = self
            .sibling_counters
            .last_mut()
            .expect("sibling counter stack non-empty");
        let key = make_order_key(*counter);
        *counter += 1;
        OrderKey(key)
    }

    /// Spawn the common bundle for a kyoso_figma node entity:
    /// `FigmaNode` marker + `Transform::default()` + tree edge to the
    /// current parent (if any).
    fn spawn_with_parent(&mut self, extra: impl Bundle) -> Entity {
        let parent = self.current_parent();
        let order_key = if parent.is_some() {
            self.next_order_key()
        } else {
            // Top-level under the canvas — bump the root counter so
            // multiple top-level frames each get unique keys.
            self.next_order_key()
        };
        let entity = self
            .commands
            .spawn((
                FigmaNode,
                Transform::default(),
                TreeParent(parent),
                order_key,
                extra,
            ))
            .id();
        self.spawned.push(entity);
        entity
    }
}

fn make_order_key(mut n: usize) -> String {
    let mut buf = String::new();
    loop {
        let c = (b'a' + (n % 26) as u8) as char;
        buf.insert(0, c);
        n /= 26;
        if n == 0 {
            break;
        }
        n -= 1;
    }
    buf
}

// ---------------------------------------------------------------------------
// NodeVisitor impl — supported variants only
// ---------------------------------------------------------------------------

impl<'w, 's> NodeVisitor for KyosoVisitor<'w, 's> {
    fn visit_frame(&mut self, frame: &FrameNode, _ctx: &NodeContext) {
        let size = vector_to_size(frame.size.as_deref());
        let figma_layout_mode = frame.layout_mode.unwrap_or_default();
        let layout_mode = convert_layout_mode(figma_layout_mode);
        let our_frame = Frame {
            name: frame.name.clone(),
            clips_content: frame.clips_content,
            layout_mode,
            fills: frame.fills.iter().map(convert_paint).collect(),
            strokes: frame
                .strokes
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(convert_paint)
                .collect(),
            stroke_weight: frame.stroke_weight.unwrap_or(0.0) as f32,
        };
        self.spawn_with_parent((our_frame, size));
    }

    fn visit_rectangle(&mut self, rect: &RectangleNode, _ctx: &NodeContext) {
        let size = vector_to_size(rect.size.as_deref());
        let our_rect = Rectangle {
            corner_radius: rect.corner_radius.unwrap_or(0.0) as f32,
            fills: rect.fills.iter().map(convert_paint).collect(),
            strokes: rect
                .strokes
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(convert_paint)
                .collect(),
            stroke_weight: rect.stroke_weight.unwrap_or(0.0) as f32,
        };
        self.spawn_with_parent((our_rect, size));
    }

    fn visit_text(&mut self, text: &TextNode, _ctx: &NodeContext) {
        let our_text = Text {
            content: text.characters.clone(),
            style: convert_type_style(&text.style),
            fills: text.fills.iter().map(convert_paint).collect(),
        };
        let size = vector_to_size(text.size.as_deref());
        self.spawn_with_parent((our_text, size));
    }

    // ---- Default no-op variants get a single "log + skip" handler --

    fn visit_group(&mut self, n: &figma_api::models::GroupNode, ctx: &NodeContext) {
        log_unsupported("Group", &n.id, ctx);
    }
    fn visit_component(&mut self, n: &figma_api::models::ComponentNode, ctx: &NodeContext) {
        log_unsupported("Component", &n.id, ctx);
    }
    fn visit_component_set(
        &mut self,
        n: &figma_api::models::ComponentSetNode,
        ctx: &NodeContext,
    ) {
        log_unsupported("ComponentSet", &n.id, ctx);
    }
    fn visit_instance(&mut self, n: &figma_api::models::InstanceNode, ctx: &NodeContext) {
        log_unsupported("Instance", &n.id, ctx);
    }
    fn visit_vector(&mut self, n: &figma_api::models::VectorNode, ctx: &NodeContext) {
        log_unsupported("Vector", &n.id, ctx);
    }
    fn visit_ellipse(&mut self, n: &figma_api::models::EllipseNode, ctx: &NodeContext) {
        log_unsupported("Ellipse", &n.id, ctx);
    }
    fn visit_line(&mut self, n: &figma_api::models::LineNode, ctx: &NodeContext) {
        log_unsupported("Line", &n.id, ctx);
    }
    fn visit_star(&mut self, n: &figma_api::models::StarNode, ctx: &NodeContext) {
        log_unsupported("Star", &n.id, ctx);
    }
    fn visit_regular_polygon(
        &mut self,
        n: &figma_api::models::RegularPolygonNode,
        ctx: &NodeContext,
    ) {
        log_unsupported("RegularPolygon", &n.id, ctx);
    }
    fn visit_boolean_operation(
        &mut self,
        n: &figma_api::models::BooleanOperationNode,
        ctx: &NodeContext,
    ) {
        log_unsupported("BooleanOperation", &n.id, ctx);
    }
    fn visit_section(&mut self, n: &figma_api::models::SectionNode, ctx: &NodeContext) {
        log_unsupported("Section", &n.id, ctx);
    }

    // ---- Container hooks: track parent + per-parent sibling counter --

    fn enter_container(&mut self, node: &SubcanvasNode, _ctx: &NodeContext) {
        if let Some(&entity) = self.spawned.last() {
            // The just-visited node became the new parent for its
            // children. Push it; reset sibling counter.
            //
            // Edge case: if the just-visited variant was unsupported
            // (e.g. Group), `spawned.last()` points at the *previous*
            // supported entity, not this Group. To keep the tree
            // shape consistent, only push when the just-spawned
            // entity matches a supported container variant.
            if matches!(
                node,
                SubcanvasNode::Frame(_)
                    | SubcanvasNode::Component(_)  // unsupported — won't ever land here
                    | SubcanvasNode::Instance(_)   // unsupported
            ) {
                self.parent_stack.push(entity);
                self.sibling_counters.push(0);
            } else {
                // Unsupported container — push the *current* parent
                // so children of this container attach to its parent
                // (effectively flattening). This is the "log + skip"
                // policy applied to subtrees. Children inherit the
                // unsupported container's parent_stack frame so we
                // still pop it correctly in exit_container below.
                self.parent_stack.push(self.current_parent().unwrap_or(
                    // If there's no parent at all (canvas-direct
                    // unsupported container), use a sentinel: we'll
                    // just skip pushing. Fall back to no-op.
                    Entity::PLACEHOLDER,
                ));
                self.sibling_counters.push(0);
            }
        }
    }

    fn exit_container(&mut self, _node: &SubcanvasNode, _ctx: &NodeContext) {
        self.parent_stack.pop();
        self.sibling_counters.pop();
    }
}

fn log_unsupported(kind: &str, id: &str, ctx: &NodeContext) {
    tracing::warn!(
        kind = %kind,
        id = %id,
        path = %ctx.path_string(),
        "kyoso_figma::import: unsupported figma node kind, skipping",
    );
}

// ---------------------------------------------------------------------------
// Per-field converters
// ---------------------------------------------------------------------------

fn convert_layout_mode(mode: figma_api::models::frame_node::LayoutMode) -> LayoutMode {
    use figma_api::models::frame_node::LayoutMode as F;
    match mode {
        F::None => LayoutMode::None,
        F::Horizontal => LayoutMode::Horizontal,
        F::Vertical => LayoutMode::Vertical,
        // We don't have a Grid variant in our `LayoutMode`; treat as
        // None for now. Add a Grid variant when the auto-layout grid
        // story lands.
        F::Grid => LayoutMode::None,
    }
}

fn vector_to_size(v: Option<&figma_api::models::Vector>) -> Size {
    match v {
        Some(vec) => Size {
            width: vec.x as f32,
            height: vec.y as f32,
        },
        None => Size::default(),
    }
}

fn convert_paint(paint: &FigmaPaint) -> Paint {
    match paint {
        FigmaPaint::SolidPaint(solid) => Paint::Solid {
            color: rgba_to_array(&solid.color),
        },
        FigmaPaint::GradientPaint(grad) => {
            let kind = match grad.r#type {
                figma_api::models::gradient_paint::Type::GradientLinear => GradientType::Linear,
                figma_api::models::gradient_paint::Type::GradientRadial => GradientType::Radial,
                figma_api::models::gradient_paint::Type::GradientAngular => GradientType::Angular,
                figma_api::models::gradient_paint::Type::GradientDiamond => GradientType::Diamond,
            };
            let stops = grad
                .gradient_stops
                .iter()
                .map(|s| GradientStop {
                    position: s.position as f32,
                    color: rgba_to_array(&s.color),
                })
                .collect();
            Paint::Gradient { kind, stops }
        }
        FigmaPaint::ImagePaint(img) => Paint::Image {
            image_ref: img.image_ref.clone(),
            scale_mode: match img.scale_mode {
                figma_api::models::image_paint::ScaleMode::Fill => ImageScaleMode::Fill,
                figma_api::models::image_paint::ScaleMode::Fit => ImageScaleMode::Fit,
                figma_api::models::image_paint::ScaleMode::Tile => ImageScaleMode::Tile,
                figma_api::models::image_paint::ScaleMode::Stretch => ImageScaleMode::Stretch,
            },
        },
        FigmaPaint::PatternPaint(_) => {
            // Patterns aren't in our Paint enum. Fall back to opaque
            // grey so the import doesn't silently produce an invisible
            // fill.
            tracing::warn!("kyoso_figma::import: PatternPaint -> falling back to grey solid");
            Paint::Solid {
                color: [0.5, 0.5, 0.5, 1.0],
            }
        }
    }
}

fn rgba_to_array(rgba: &figma_api::models::Rgba) -> [f32; 4] {
    [rgba.r as f32, rgba.g as f32, rgba.b as f32, rgba.a as f32]
}

fn convert_type_style(style: &figma_api::models::TypeStyle) -> TypeStyle {
    let mut out = TypeStyle::default();
    if let Some(family) = &style.font_family {
        out.font_family = family.clone();
    }
    if let Some(size) = style.font_size {
        out.font_size = size as f32;
    }
    if let Some(weight) = style.font_weight {
        out.font_weight = weight as u32;
    }
    if let Some(percent) = style.line_height_percent {
        out.line_height = (percent as f32) / 100.0;
    }
    out
}
