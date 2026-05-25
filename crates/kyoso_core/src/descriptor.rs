//! Scene-descriptor helpers — the small typed conversions between
//! [`Node`] and [`NodeDescriptor`].
//!
//! The runner that builds a full [`SceneGraphDescriptor`] for a scene
//! lives on [`SceneWorld::scene_descriptor`](crate::SceneWorld::scene_descriptor)
//! — it needs `&mut World` access to drive
//! [`kyoso_graph::variant::NodeVariants::try_materialize`] for the
//! closed-sum dispatch, which only the binding-layer handle has.

use kyoso_graph::descriptor::NodeDescriptor;

use crate::node::Node;

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
/// `children` are caller-supplied (the tree walk lives in the
/// `SceneWorld::scene_descriptor` runner); use this when composing
/// variant-rich descriptors by hand.
pub fn scene_node_descriptor(
    entity: bevy::prelude::Entity,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameData;
    use crate::node::Node;
    use crate::rectangle::RectangleData;
    use crate::text::TextData;

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
}
