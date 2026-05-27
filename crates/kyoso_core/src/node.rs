//! Scene-tree node taxonomy: the [`Node`] sum enum.
//!
//! ## Storage vs. API
//!
//! Storage is ECS-native. Each variant's data lives in a Bevy `Bundle`
//! ([`crate::FrameData`], [`crate::RectangleData`], [`crate::TextData`])
//! co-located with its marker component, borrowed `QueryData`, and
//! `NodeVariant` impl in `frame.rs` / `rectangle.rs` / `text.rs`. The
//! marker's `#[require(NodeKind = ...)]` auto-inserts the discriminator,
//! so the tag and the data cannot drift apart.
//!
//! The public API is the [`Node`] enum, a serializable projection.
//! `Node::Frame(FrameData)` is symmetric: same type for spawning into
//! the ECS, materializing back out via
//! [`SceneWorld`](crate::SceneWorld)'s typed methods, and serde-tagged
//! JSON round-trip for the AI / RPC layer.
//!
//! ## Reading scene nodes
//!
//! Two paths:
//!
//! - **In-system, parallel-safe**: declare per-variant queries
//!   directly — `Query<&Frame>`, `Query<&Rectangle>`, `Query<&Text>`
//!   — and combine with [`crate::query_data::AnyNodeQueryData`] /
//!   [`crate::NodeKind`] for discriminator-aware dispatch. The
//!   [`kyoso_graph::scene::Scene`] SystemParam bundles a
//!   [`kyoso_graph::tree::TreeQuery`] + a `GraphQuery<&SceneNode, &SceneEdge>`
//!   if you want hierarchy + entity-edge graph at once.
//! - **Binding layer / between-tick**: use [`crate::SceneWorld`]'s
//!   typed methods (`read_as`, `iter_as`, `traverse_typed::<SceneNode>`)
//!   — they take `&mut self` and dispatch via the
//!   [`kyoso_graph::variant::NodeVariants`] tuple over `&World` directly.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::frame::FrameData;
use crate::rectangle::RectangleData;
use crate::text::TextData;
use crate::{NodeKind, SceneNode};

#[cfg(test)]
use bevy::ecs::system::RunSystemOnce;

// ============================================================================
// Node enum
// ============================================================================

/// The public node taxonomy. Serializes with an internal `"kind"` tag
/// (`"frame"` / `"rectangle"` / `"text"`) for AI / RPC consumers.
///
/// Spawn into the ECS via [`Node::spawn`]. Materialize back via
/// [`SceneWorld::read_as`](crate::SceneWorld::read_as) (single-entity)
/// or [`SceneWorld::traverse_typed::<SceneNode>`](crate::SceneWorld::traverse_typed)
/// (walking) — both go through the
/// [`kyoso_graph::variant::NodeVariants`] tuple dispatch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Frame(FrameData),
    Rectangle(RectangleData),
    Text(TextData),
}

impl Node {
    /// Cheap discriminator lookup without destructuring. Useful for
    /// dispatch tables, logging, and "I want the variant tag but not
    /// the data" call sites where pattern-matching the full enum is
    /// overkill.
    pub fn kind(&self) -> NodeKind {
        match self {
            Node::Frame(_) => NodeKind::Frame,
            Node::Rectangle(_) => NodeKind::Rectangle,
            Node::Text(_) => NodeKind::Text,
        }
    }

    /// Spawn the node into the ECS, attaching the given `Transform`
    /// and the [`SceneNode`] structural marker.
    pub fn spawn(self, commands: &mut Commands, transform: Transform) -> Entity {
        match self {
            Node::Frame(b) => commands.spawn((b, transform, SceneNode)).id(),
            Node::Rectangle(b) => commands.spawn((b, transform, SceneNode)).id(),
            Node::Text(b) => commands.spawn((b, transform, SceneNode)).id(),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::rectangle::Rectangle;
    use crate::size::Size;
    use crate::text::Text;
    use kyoso_graph::variant::{NodeVariant, NodeVariants};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app
    }

    #[test]
    fn node_kind_returns_correct_discriminator_for_each_variant() {
        assert_eq!(Node::Frame(FrameData::default()).kind(), NodeKind::Frame);
        assert_eq!(Node::Rectangle(RectangleData::default()).kind(), NodeKind::Rectangle);
        assert_eq!(Node::Text(TextData::default()).kind(), NodeKind::Text);
    }

    #[test]
    fn serde_round_trip_preserves_variant_tag() {
        let original = Node::Rectangle(RectangleData {
            rectangle: Rectangle { corner_radius: 4.0, ..default() },
            size: Size { width: 32.0, height: 32.0 },
        });
        let json = serde_json::to_string(&original).expect("serialize");
        assert!(json.contains("\"kind\":\"rectangle\""), "missing kind tag in {json}");
        let back: Node = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, back);
    }

    /// Regression: when the sync layer inserts variant components
    /// directly (bypassing `*Data`'s Bundle spawn), Bevy's `#[require]`
    /// still attaches the `NodeKind` tag so downstream readers find
    /// the entity.
    #[test]
    fn remote_spawn_path_gets_node_kind_via_require() {
        let mut app = test_app();

        let spawned = app
            .world_mut()
            .run_system_once(|mut commands: Commands| {
                commands
                    .spawn((
                        Frame { name: "remote".into(), ..default() },
                        Size { width: 7.0, height: 9.0 },
                    ))
                    .id()
            })
            .expect("spawn system runs");

        let kind = app
            .world()
            .entity(spawned)
            .get::<NodeKind>()
            .copied();
        assert_eq!(kind, Some(NodeKind::Frame), "NodeKind not auto-inserted on Frame");
    }

    #[test]
    fn node_variant_consts_match_kinds() {
        assert_eq!(<Frame as NodeVariant>::KIND, NodeKind::Frame);
        assert_eq!(<Rectangle as NodeVariant>::KIND, NodeKind::Rectangle);
        assert_eq!(<Text as NodeVariant>::KIND, NodeKind::Text);
        assert!(matches!(<Frame as NodeVariant>::wrap(FrameData::default()), Node::Frame(_)));
        assert!(matches!(<Rectangle as NodeVariant>::wrap(RectangleData::default()), Node::Rectangle(_)));
        assert!(matches!(<Text as NodeVariant>::wrap(TextData::default()), Node::Text(_)));
    }

    /// Compile-only check that the typed-graph trait machinery lines up.
    #[test]
    fn typed_graph_wiring_compiles() {
        fn assert_graph_is_scene_node<V>()
        where
            V: NodeVariant<Graph = SceneNode>,
        {
        }
        assert_graph_is_scene_node::<Frame>();
        assert_graph_is_scene_node::<Rectangle>();
        assert_graph_is_scene_node::<Text>();

        let owned: <SceneNode as kyoso_graph::Graph>::Node =
            <Frame as NodeVariant>::wrap(FrameData::default());
        assert!(matches!(owned, Node::Frame(_)));

        let kind: <SceneNode as kyoso_graph::Graph>::NodeDiscriminator = NodeKind::Rectangle;
        assert_eq!(kind, NodeKind::Rectangle);
    }

    /// `NodeVariants` tuple impl dispatches `(Frame, Rectangle, Text)`
    /// via try-each over `&World` directly. This is the closed-sum
    /// dispatch path that replaces the old `SceneNodeQuery::get`.
    #[test]
    fn node_variants_tuple_dispatches_to_correct_arm() {
        type V = <SceneNode as kyoso_graph::Graph>::Variants;

        let mut app = test_app();
        let (frame_e, rect_e, text_e, bare_e) = app
            .world_mut()
            .run_system_once(|mut commands: Commands| {
                let f = commands
                    .spawn((FrameData::default(), Transform::IDENTITY, SceneNode))
                    .id();
                let r = commands
                    .spawn((RectangleData::default(), Transform::IDENTITY, SceneNode))
                    .id();
                let t = commands
                    .spawn((TextData::default(), Transform::IDENTITY, SceneNode))
                    .id();
                let b = commands.spawn(()).id();
                (f, r, t, b)
            })
            .expect("spawn variants");

        let world = app.world_mut();
        let mut states = V::build_states(world);

        assert!(matches!(V::try_materialize(&mut states, world, frame_e), Some(Node::Frame(_))));
        assert!(matches!(V::try_materialize(&mut states, world, rect_e), Some(Node::Rectangle(_))));
        assert!(matches!(V::try_materialize(&mut states, world, text_e), Some(Node::Text(_))));
        assert!(V::try_materialize(&mut states, world, bare_e).is_none());
    }
}
