//! Figma-side adapter: project a [`Node`] into the Figma-agnostic
//! [`NodeDescriptor`] shape consumed by AI / RPC layers.
//!
//! `kyoso_graph::descriptor::NodeDescriptor` is intentionally generic
//! (`node_type: String` + `data: Option<serde_json::Value>`), so it
//! doesn't need to know about Figma's variant set. This module is the
//! one place where the typed `Node` enum meets that generic surface.

use bevy::prelude::Entity;
use kyoso_graph::descriptor::{GraphMetadata, NodeDescriptor, SceneGraphDescriptor};

use crate::node::{FigmaNodeQuery, Node};

/// Variant tag string for a given [`Node`]. Matches the serde
/// `#[serde(tag = "kind", rename_all = "snake_case")]` discriminator
/// on the enum.
pub fn node_type_str(node: &Node) -> &'static str {
    match node {
        Node::Frame(_) => "frame",
        Node::Rectangle(_) => "rectangle",
        Node::Text(_) => "text",
    }
}

/// Serialize a [`Node`] as JSON for the descriptor `data` field. Falls
/// back to `Value::Null` if serialization fails (cannot, given the
/// Bundle's serde-derived impls).
pub fn node_payload(node: &Node) -> serde_json::Value {
    serde_json::to_value(node).unwrap_or(serde_json::Value::Null)
}

/// Build a leaf-level [`NodeDescriptor`] for `node`. `depth` and
/// `children` are caller-supplied (the tree walk lives in
/// `kyoso_graph::descriptor::SceneGraphDescriptor`); use this when
/// composing variant-rich descriptors.
pub fn figma_node_descriptor(
    entity: Entity,
    node: &Node,
    depth: usize,
    children: Vec<NodeDescriptor>,
) -> NodeDescriptor {
    NodeDescriptor {
        id: format!("{entity:?}"),
        node_type: node_type_str(node).to_string(),
        depth,
        children,
        data: Some(node_payload(node)),
    }
}

/// Recursively build a typed [`NodeDescriptor`] for the subtree rooted
/// at `entity`. Returns `None` if `entity` isn't a Figma node.
pub fn build_figma_node_descriptor(
    figma_q: &FigmaNodeQuery,
    entity: Entity,
) -> Option<NodeDescriptor> {
    let node = figma_q.get(entity)?;
    let depth = figma_q.tree.depth(entity);
    let children: Vec<NodeDescriptor> = figma_q
        .tree
        .children(entity)
        .into_iter()
        .filter_map(|c| build_figma_node_descriptor(figma_q, c))
        .collect();
    Some(figma_node_descriptor(entity, &node, depth, children))
}

/// Build a fully-typed [`SceneGraphDescriptor`] for the entire scene.
///
/// Walks the tree from every root via [`build_figma_node_descriptor`],
/// producing a `node_type` and `data` payload per node. This is the
/// typed alternative to [`SceneGraphDescriptor::from_scene_graph`],
/// which is generic and emits stringly-typed placeholders.
pub fn build_figma_scene_descriptor(figma_q: &FigmaNodeQuery) -> SceneGraphDescriptor {
    let roots = figma_q.tree.roots();
    let node_count = figma_q.tree.node_count();
    let root_count = roots.len();
    let max_depth = figma_q.tree.max_depth();

    let root_descriptors: Vec<NodeDescriptor> = roots
        .into_iter()
        .filter_map(|r| build_figma_node_descriptor(figma_q, r))
        .collect();

    SceneGraphDescriptor {
        metadata: GraphMetadata {
            node_count,
            root_count,
            max_depth,
            is_acyclic: true,
            is_tree: true,
        },
        roots: root_descriptors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{FrameData, Node, RectangleData, TextData};

    #[test]
    fn node_type_str_matches_serde_tag() {
        assert_eq!(node_type_str(&Node::Frame(FrameData::default())), "frame");
        assert_eq!(node_type_str(&Node::Rectangle(RectangleData::default())), "rectangle");
        assert_eq!(node_type_str(&Node::Text(TextData::default())), "text");
    }

    #[test]
    fn payload_round_trips_through_node() {
        let original = Node::Frame(FrameData::default());
        let payload = node_payload(&original);
        let back: Node = serde_json::from_value(payload).expect("deserialize");
        assert_eq!(original, back);
    }

    #[test]
    fn typed_scene_descriptor_carries_variant_data() {
        use crate::node::FigmaNodeQuery;
        use crate::FigmaNode;
        use bevy::ecs::system::RunSystemOnce;
        use bevy::prelude::*;
        use kyoso_graph::components::{EdgeFrom, EdgeTo};
        use kyoso_graph::tree::{OrderKey, TreeEdge, TreeParent};

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);

        app.world_mut()
            .run_system_once(|mut commands: Commands| {
                let root = commands
                    .spawn((FrameData::default(), Transform::IDENTITY, FigmaNode, TreeParent(None)))
                    .id();
                let rect = commands
                    .spawn((
                        RectangleData::default(),
                        Transform::IDENTITY,
                        FigmaNode,
                        TreeParent(Some(root)),
                        OrderKey("a".into()),
                    ))
                    .id();
                let text = commands
                    .spawn((
                        TextData::default(),
                        Transform::IDENTITY,
                        FigmaNode,
                        TreeParent(Some(root)),
                        OrderKey("b".into()),
                    ))
                    .id();
                commands.spawn((EdgeFrom(root), EdgeTo(rect), TreeEdge));
                commands.spawn((EdgeFrom(root), EdgeTo(text), TreeEdge));
            })
            .expect("spawn fixture");

        let descriptor = app
            .world_mut()
            .run_system_once(|q: FigmaNodeQuery| build_figma_scene_descriptor(&q))
            .expect("descriptor build");

        assert_eq!(descriptor.roots.len(), 1, "exactly one tree root");
        let root_desc = &descriptor.roots[0];
        assert_eq!(root_desc.node_type, "frame");
        assert!(root_desc.data.is_some(), "root has typed data");
        assert_eq!(root_desc.children.len(), 2);

        let kinds: Vec<&str> = root_desc.children.iter().map(|c| c.node_type.as_str()).collect();
        assert!(kinds.contains(&"rectangle"));
        assert!(kinds.contains(&"text"));
        for child in &root_desc.children {
            assert!(child.data.is_some(), "child has typed data");
            assert_eq!(child.depth, 1);
        }
    }
}
