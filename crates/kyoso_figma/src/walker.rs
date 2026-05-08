//! Generic walker for traversing Figma node trees using the visitor pattern.
//!
//! ## Provenance
//!
//! Vendored from `etch_figma` (Hector Crean's TSX-codegen crate) at commit-of-
//! copy. Source files:
//! - `etch/crates/etch_figma/src/core/walker.rs`
//! - `etch/crates/etch_figma/src/lib.rs` (the `SubcanvasNodeExt` trait + impl)
//!
//! The walker is dep-free (only `figma_api`) and self-contained, so vendoring
//! avoids dragging in etch_figma's TSX/SVG/Tailwind toolchain and its
//! `swc_*` / `reqwest` / `etch_tsx` / `etch_svgr` transitive deps.
//!
//! Re-sync if etch_figma adds useful visitor methods. Long-term, the right
//! structural move is splitting this into a shared `figma-walker` crate;
//! tracked as a follow-up in the etch repo, not a kyoso prerequisite.

use figma_api::models::{
    BooleanOperationNode, CanvasNode, ComponentNode, ComponentSetNode, ConnectorNode, EllipseNode,
    EmbedNode, FrameNode, GroupNode, InstanceNode, LineNode, LinkUnfurlNode, RectangleNode,
    RegularPolygonNode, SectionNode, ShapeWithTextNode, SliceNode, StarNode, StickyNode,
    SubcanvasNode, TableCellNode, TableNode, TextNode, TextPathNode, TransformGroupNode,
    VectorNode, WashiTapeNode, WidgetNode,
};

// ---------------------------------------------------------------------------
// SubcanvasNodeExt
// ---------------------------------------------------------------------------

/// Extension trait for [`SubcanvasNode`] to provide common node operations
/// over its many variants without re-writing the giant match.
pub trait SubcanvasNodeExt {
    fn id(&self) -> Option<&str>;
    fn name(&self) -> Option<&str>;
    fn children(&self) -> Option<&Vec<SubcanvasNode>>;
    fn has_children(&self) -> bool {
        self.children()
            .map_or(false, |children| !children.is_empty())
    }
    fn node_type(&self) -> &'static str;
}

impl SubcanvasNodeExt for SubcanvasNode {
    fn id(&self) -> Option<&str> {
        match self {
            SubcanvasNode::BooleanOperation(n) => Some(&n.id),
            SubcanvasNode::Component(n) => Some(&n.id),
            SubcanvasNode::ComponentSet(n) => Some(&n.id),
            SubcanvasNode::Connector(n) => Some(&n.id),
            SubcanvasNode::Ellipse(n) => Some(&n.id),
            SubcanvasNode::Embed(n) => Some(&n.id),
            SubcanvasNode::Frame(n) => Some(&n.id),
            SubcanvasNode::Group(n) => Some(&n.id),
            SubcanvasNode::Instance(n) => Some(&n.id),
            SubcanvasNode::Line(n) => Some(&n.id),
            SubcanvasNode::LinkUnfurl(n) => Some(&n.id),
            SubcanvasNode::Rectangle(n) => Some(&n.id),
            SubcanvasNode::RegularPolygon(n) => Some(&n.id),
            SubcanvasNode::Section(n) => Some(&n.id),
            SubcanvasNode::ShapeWithText(n) => Some(&n.id),
            SubcanvasNode::Slice(n) => Some(&n.id),
            SubcanvasNode::Star(n) => Some(&n.id),
            SubcanvasNode::Sticky(n) => Some(&n.id),
            SubcanvasNode::Table(n) => Some(&n.id),
            SubcanvasNode::TableCell(n) => Some(&n.id),
            SubcanvasNode::Text(n) => Some(&n.id),
            SubcanvasNode::TextPath(n) => Some(&n.id),
            SubcanvasNode::TransformGroup(n) => Some(&n.id),
            SubcanvasNode::Vector(n) => Some(&n.id),
            SubcanvasNode::WashiTape(n) => Some(&n.id),
            SubcanvasNode::Widget(n) => Some(&n.id),
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            SubcanvasNode::BooleanOperation(n) => Some(&n.name),
            SubcanvasNode::Component(n) => Some(&n.name),
            SubcanvasNode::ComponentSet(n) => Some(&n.name),
            SubcanvasNode::Frame(n) => Some(&n.name),
            SubcanvasNode::Group(n) => Some(&n.name),
            SubcanvasNode::Instance(n) => Some(&n.name),
            SubcanvasNode::Section(n) => Some(&n.name),
            SubcanvasNode::Table(n) => Some(&n.name),
            SubcanvasNode::TableCell(n) => Some(&n.name),
            SubcanvasNode::TransformGroup(n) => Some(&n.name),
            _ => None,
        }
    }

    fn children(&self) -> Option<&Vec<SubcanvasNode>> {
        match self {
            SubcanvasNode::BooleanOperation(n) => Some(&n.children),
            SubcanvasNode::Component(n) => Some(&n.children),
            SubcanvasNode::ComponentSet(n) => Some(&n.children),
            SubcanvasNode::Frame(n) => Some(&n.children),
            SubcanvasNode::Group(n) => Some(&n.children),
            SubcanvasNode::Instance(n) => Some(&n.children),
            SubcanvasNode::Section(n) => Some(&n.children),
            SubcanvasNode::Table(n) => Some(&n.children),
            SubcanvasNode::TransformGroup(n) => Some(&n.children),
            _ => None,
        }
    }

    fn node_type(&self) -> &'static str {
        match self {
            SubcanvasNode::BooleanOperation(_) => "BooleanOperation",
            SubcanvasNode::Component(_) => "Component",
            SubcanvasNode::ComponentSet(_) => "ComponentSet",
            SubcanvasNode::Connector(_) => "Connector",
            SubcanvasNode::Ellipse(_) => "Ellipse",
            SubcanvasNode::Embed(_) => "Embed",
            SubcanvasNode::Frame(_) => "Frame",
            SubcanvasNode::Group(_) => "Group",
            SubcanvasNode::Instance(_) => "Instance",
            SubcanvasNode::Line(_) => "Line",
            SubcanvasNode::LinkUnfurl(_) => "LinkUnfurl",
            SubcanvasNode::Rectangle(_) => "Rectangle",
            SubcanvasNode::RegularPolygon(_) => "RegularPolygon",
            SubcanvasNode::Section(_) => "Section",
            SubcanvasNode::ShapeWithText(_) => "ShapeWithText",
            SubcanvasNode::Slice(_) => "Slice",
            SubcanvasNode::Star(_) => "Star",
            SubcanvasNode::Sticky(_) => "Sticky",
            SubcanvasNode::Table(_) => "Table",
            SubcanvasNode::TableCell(_) => "TableCell",
            SubcanvasNode::Text(_) => "Text",
            SubcanvasNode::TextPath(_) => "TextPath",
            SubcanvasNode::TransformGroup(_) => "TransformGroup",
            SubcanvasNode::Vector(_) => "Vector",
            SubcanvasNode::WashiTape(_) => "WashiTape",
            SubcanvasNode::Widget(_) => "Widget",
        }
    }
}

// ---------------------------------------------------------------------------
// NodeContext
// ---------------------------------------------------------------------------

/// Context information provided during node traversal.
#[derive(Debug, Clone, Default)]
pub struct NodeContext {
    /// Hierarchical path from root to the current node, by `name`.
    pub path: Vec<String>,
    /// Current traversal depth (0 = direct child of canvas root).
    pub depth: usize,
    /// Stable Figma id of the current node being visited.
    pub node_id: String,
}

impl NodeContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// `"Canvas > Frame > Group"`-style display path, useful for
    /// log messages.
    pub fn path_string(&self) -> String {
        self.path.join(" > ")
    }
}

// ---------------------------------------------------------------------------
// NodeVisitor
// ---------------------------------------------------------------------------

/// Trait for visiting Figma nodes with rich context information.
///
/// All methods have default no-op implementations, so an implementor only
/// overrides the variants it cares about.
#[allow(unused_variables)]
pub trait NodeVisitor {
    // Container nodes (have children).
    fn visit_frame(&mut self, frame: &FrameNode, context: &NodeContext) {}
    fn visit_group(&mut self, group: &GroupNode, context: &NodeContext) {}
    fn visit_component(&mut self, component: &ComponentNode, context: &NodeContext) {}
    fn visit_component_set(&mut self, component_set: &ComponentSetNode, context: &NodeContext) {}
    fn visit_instance(&mut self, instance: &InstanceNode, context: &NodeContext) {}
    fn visit_section(&mut self, section: &SectionNode, context: &NodeContext) {}
    fn visit_boolean_operation(
        &mut self,
        boolean_op: &BooleanOperationNode,
        context: &NodeContext,
    ) {
    }
    fn visit_table(&mut self, table: &TableNode, context: &NodeContext) {}
    fn visit_transform_group(
        &mut self,
        transform_group: &TransformGroupNode,
        context: &NodeContext,
    ) {
    }

    // Leaf nodes (no children).
    fn visit_table_cell(&mut self, table_cell: &TableCellNode, context: &NodeContext) {}
    fn visit_text(&mut self, text: &TextNode, context: &NodeContext) {}
    fn visit_vector(&mut self, vector: &VectorNode, context: &NodeContext) {}
    fn visit_rectangle(&mut self, rectangle: &RectangleNode, context: &NodeContext) {}
    fn visit_ellipse(&mut self, ellipse: &EllipseNode, context: &NodeContext) {}
    fn visit_line(&mut self, line: &LineNode, context: &NodeContext) {}
    fn visit_star(&mut self, star: &StarNode, context: &NodeContext) {}
    fn visit_regular_polygon(&mut self, polygon: &RegularPolygonNode, context: &NodeContext) {}
    fn visit_shape_with_text(
        &mut self,
        shape_with_text: &ShapeWithTextNode,
        context: &NodeContext,
    ) {
    }
    fn visit_text_path(&mut self, text_path: &TextPathNode, context: &NodeContext) {}
    fn visit_sticky(&mut self, sticky: &StickyNode, context: &NodeContext) {}
    fn visit_connector(&mut self, connector: &ConnectorNode, context: &NodeContext) {}
    fn visit_washi_tape(&mut self, washi_tape: &WashiTapeNode, context: &NodeContext) {}
    fn visit_embed(&mut self, embed: &EmbedNode, context: &NodeContext) {}
    fn visit_link_unfurl(&mut self, link_unfurl: &LinkUnfurlNode, context: &NodeContext) {}
    fn visit_slice(&mut self, slice: &SliceNode, context: &NodeContext) {}
    fn visit_widget(&mut self, widget: &WidgetNode, context: &NodeContext) {}

    /// Override to skip traversing into the children of a container.
    /// Default: traverse all children.
    fn should_traverse_children(&self, node: &SubcanvasNode) -> bool {
        true
    }

    /// Called before traversing into a container node's children.
    fn enter_container(&mut self, node: &SubcanvasNode, context: &NodeContext) {}

    /// Called after traversing a container node's children.
    fn exit_container(&mut self, node: &SubcanvasNode, context: &NodeContext) {}
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

/// Generic walker that traverses Figma nodes using a visitor pattern.
///
/// Performs a depth-first, pre-order traversal. The visitor's
/// per-variant `visit_*` method is called as each node is encountered;
/// `enter_container` / `exit_container` bracket child traversal.
pub struct Walker<V> {
    visitor: V,
}

impl<V: NodeVisitor> Walker<V> {
    pub fn new(visitor: V) -> Self {
        Self { visitor }
    }

    /// Walk a single node and return the (mutated) visitor.
    pub fn walk(mut self, node: &SubcanvasNode) -> V {
        let mut context = NodeContext::new();
        self.walk_recursive(node, &mut context);
        self.visitor
    }

    /// Walk all children of a [`CanvasNode`] (the typical entry point —
    /// a Figma file's top-level pages).
    pub fn walk_canvas(mut self, canvas: &CanvasNode) -> V {
        let mut context = NodeContext::new();
        context.path.push(canvas.name.clone());
        for child in &canvas.children {
            self.walk_recursive(child, &mut context);
        }
        self.visitor
    }

    fn walk_recursive(&mut self, node: &SubcanvasNode, context: &mut NodeContext) {
        if let Some(node_id) = node.id() {
            context.node_id = node_id.to_string();
        }

        match node {
            // Container nodes
            SubcanvasNode::Frame(n) => self.visitor.visit_frame(n, context),
            SubcanvasNode::Group(n) => self.visitor.visit_group(n, context),
            SubcanvasNode::Component(n) => self.visitor.visit_component(n, context),
            SubcanvasNode::ComponentSet(n) => self.visitor.visit_component_set(n, context),
            SubcanvasNode::Instance(n) => self.visitor.visit_instance(n, context),
            SubcanvasNode::Section(n) => self.visitor.visit_section(n, context),
            SubcanvasNode::BooleanOperation(n) => self.visitor.visit_boolean_operation(n, context),
            SubcanvasNode::Table(n) => self.visitor.visit_table(n, context),
            SubcanvasNode::TransformGroup(n) => self.visitor.visit_transform_group(n, context),

            // Leaf nodes
            SubcanvasNode::TableCell(n) => self.visitor.visit_table_cell(n, context),
            SubcanvasNode::Text(n) => self.visitor.visit_text(n, context),
            SubcanvasNode::Vector(n) => self.visitor.visit_vector(n, context),
            SubcanvasNode::Rectangle(n) => self.visitor.visit_rectangle(n, context),
            SubcanvasNode::Ellipse(n) => self.visitor.visit_ellipse(n, context),
            SubcanvasNode::Line(n) => self.visitor.visit_line(n, context),
            SubcanvasNode::Star(n) => self.visitor.visit_star(n, context),
            SubcanvasNode::RegularPolygon(n) => self.visitor.visit_regular_polygon(n, context),
            SubcanvasNode::ShapeWithText(n) => self.visitor.visit_shape_with_text(n, context),
            SubcanvasNode::TextPath(n) => self.visitor.visit_text_path(n, context),
            SubcanvasNode::Sticky(n) => self.visitor.visit_sticky(n, context),
            SubcanvasNode::Connector(n) => self.visitor.visit_connector(n, context),
            SubcanvasNode::WashiTape(n) => self.visitor.visit_washi_tape(n, context),
            SubcanvasNode::Embed(n) => self.visitor.visit_embed(n, context),
            SubcanvasNode::LinkUnfurl(n) => self.visitor.visit_link_unfurl(n, context),
            SubcanvasNode::Slice(n) => self.visitor.visit_slice(n, context),
            SubcanvasNode::Widget(n) => self.visitor.visit_widget(n, context),
        }

        if self.visitor.should_traverse_children(node) {
            if let Some(children) = node.children() {
                self.visitor.enter_container(node, context);
                if let Some(name) = node.name() {
                    context.path.push(name.to_string());
                }
                context.depth += 1;
                for child in children {
                    self.walk_recursive(child, context);
                }
                context.depth -= 1;
                if node.name().is_some() {
                    context.path.pop();
                }
                self.visitor.exit_container(node, context);
            }
        }
    }
}
