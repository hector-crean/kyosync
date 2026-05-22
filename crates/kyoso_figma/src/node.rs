//! Figma-shaped node taxonomy: `Node` enum + per-variant `*Data`
//! Bundles + dispatch.
//!
//! ## Storage vs. API
//!
//! Storage is ECS-native. Each variant's data lives in a Bevy `Bundle`
//! (`FrameData`, `RectangleData`, `TextData`) containing the variant's
//! component + shared `Size` + a [`NodeKind`](crate::NodeKind) tag.
//! Inserting the bundle inserts the tag atomically, so the
//! discriminator can't drift apart from the data.
//!
//! The public API is the [`Node`] enum, a serializable projection.
//! `Node::Frame(FrameData)` is symmetric: same type for spawning into
//! the ECS, materializing back out via [`FigmaNodeQuery`], and
//! serde-tagged JSON round-trip for the AI / RPC layer.
//!
//! ## Delegation
//!
//! Cross-variant behaviour is captured in the [`NodeBehavior`] trait.
//! `#[enum_dispatch(NodeBehavior)]` on `Node` auto-generates
//! `impl NodeBehavior for Node` by forwarding each method to whichever
//! variant is held — and also gives us free `From<FrameData> for Node`
//! / `From<RectangleData> for Node` / `From<TextData> for Node`.

use bevy::ecs::query::ROQueryItem;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use enum_dispatch::enum_dispatch;
use kyoso_graph::scene::SceneGraph;
use kyoso_graph::{Materialize, MaterializeEdge, NodeVariant};
use serde::{Deserialize, Serialize};

use crate::frame::Frame;
use crate::query_data::{AnyNodeQueryData, FrameQueryData, RectangleQueryData, TextQueryData};
use crate::rectangle::Rectangle;
use crate::size::Size;
use crate::text::Text;
use crate::visitor::{SceneVisitor, Traverse, VisitContext};
use crate::{FigmaEdge, FigmaNode, NodeKind};

#[cfg(test)]
use bevy::ecs::system::RunSystemOnce;

// ============================================================================
// NodeBehavior trait
// ============================================================================

/// Shared behaviour across all node variants. The
/// `#[enum_dispatch(NodeBehavior)]` attribute on [`Node`] (below)
/// auto-generates `impl NodeBehavior for Node` by forwarding each
/// method to the wrapped variant — no hand-written match arms required.
#[enum_dispatch]
pub trait NodeBehavior {
    fn kind(&self) -> NodeKind;
    fn size(&self) -> &Size;
}

// ============================================================================
// Per-variant *Data bundles
// ============================================================================
//
// Each variant data component carries `#[require(NodeKind = ...)]` (see
// `frame.rs`, `rectangle.rs`, `text.rs`), so inserting the component —
// whether via these Bundles' local-spawn path or via the sync layer's
// per-component ops — Bevy auto-inserts the right `NodeKind` discriminator.
// That means we don't carry an explicit `kind` field here.

/// Frame variant payload + Bevy `Bundle`.
#[derive(Bundle, Default, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FrameData {
    pub frame: Frame,
    pub size: Size,
}

impl NodeBehavior for FrameData {
    fn kind(&self) -> NodeKind { NodeKind::Frame }
    fn size(&self) -> &Size { &self.size }
}

#[derive(Bundle, Default, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RectangleData {
    pub rectangle: Rectangle,
    pub size: Size,
}

impl NodeBehavior for RectangleData {
    fn kind(&self) -> NodeKind { NodeKind::Rectangle }
    fn size(&self) -> &Size { &self.size }
}

#[derive(Bundle, Default, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TextData {
    pub text: Text,
    pub size: Size,
}

impl NodeBehavior for TextData {
    fn kind(&self) -> NodeKind { NodeKind::Text }
    fn size(&self) -> &Size { &self.size }
}

// ============================================================================
// Node enum
// ============================================================================

/// The public node taxonomy. Serializes with an internal `"kind"` tag
/// (`"frame"` / `"rectangle"` / `"text"`) for AI / RPC consumers.
///
/// Spawn into the ECS via [`Node::spawn`]. Materialize back via
/// [`FigmaNodeQuery::get`].
#[enum_dispatch(NodeBehavior)]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Frame(FrameData),
    Rectangle(RectangleData),
    Text(TextData),
}

impl Node {
    /// Spawn the node into the ECS, attaching the given `Transform`
    /// and the [`FigmaNode`] structural marker. The four-arm match is
    /// the only hand-written dispatch in this module — every other
    /// per-variant operation goes through `enum_dispatch`.
    pub fn spawn(self, commands: &mut Commands, transform: Transform) -> Entity {
        match self {
            Node::Frame(b) => commands.spawn((b, transform, FigmaNode)).id(),
            Node::Rectangle(b) => commands.spawn((b, transform, FigmaNode)).id(),
            Node::Text(b) => commands.spawn((b, transform, FigmaNode)).id(),
        }
    }
}

// ============================================================================
// NodeVariant impls — `kyoso_graph::NodeVariant` is the relator trait
// ============================================================================
//
// The trait now lives in `kyoso_graph` (a graph-crate concern, not Figma's),
// and decouples per-variant typing from this crate. Each variant declares
// `type Graph = FigmaNode` to anchor it to the Figma typed graph; everything
// else (Discriminator, AnyOwned, etc.) flows through `kyoso_graph::Graph`.

impl NodeVariant for Frame {
    type Graph = FigmaNode;
    type Data = FrameData;
    type Query = FrameQueryData;
    const KIND: NodeKind = NodeKind::Frame;
    fn wrap(data: FrameData) -> Node { Node::Frame(data) }
    fn materialize(item: ROQueryItem<'_, '_, FrameQueryData>) -> FrameData {
        FrameData { frame: item.frame.clone(), size: item.size.clone() }
    }
}

impl NodeVariant for Rectangle {
    type Graph = FigmaNode;
    type Data = RectangleData;
    type Query = RectangleQueryData;
    const KIND: NodeKind = NodeKind::Rectangle;
    fn wrap(data: RectangleData) -> Node { Node::Rectangle(data) }
    fn materialize(item: ROQueryItem<'_, '_, RectangleQueryData>) -> RectangleData {
        RectangleData { rectangle: item.rectangle.clone(), size: item.size.clone() }
    }
}

impl NodeVariant for Text {
    type Graph = FigmaNode;
    type Data = TextData;
    type Query = TextQueryData;
    const KIND: NodeKind = NodeKind::Text;
    fn wrap(data: TextData) -> Node { Node::Text(data) }
    fn materialize(item: ROQueryItem<'_, '_, TextQueryData>) -> TextData {
        TextData { text: item.text.clone(), size: item.size.clone() }
    }
}

// ============================================================================
// FigmaNodeQuery — cross-variant fetch
// ============================================================================

/// System parameter for reading any Figma node as a typed [`Node`].
///
/// Bundles the discriminator lookup with each per-variant
/// [`Query`](bevy::ecs::system::Query) so a single
/// `FigmaNodeQuery::get(entity)` round-trips through ECS → `Node`.
#[derive(SystemParam)]
pub struct FigmaNodeQuery<'w, 's> {
    pub kinds: Query<'w, 's, (Entity, &'static NodeKind)>,
    pub frames: Query<'w, 's, FrameQueryData>,
    pub rectangles: Query<'w, 's, RectangleQueryData>,
    pub texts: Query<'w, 's, TextQueryData>,
    /// Tree-walk substrate. `SceneGraph` brings in the tree-edge /
    /// `OrderKey` / parent-child machinery used by [`walk_subtree`].
    ///
    /// [`walk_subtree`]: FigmaNodeQuery::walk_subtree
    pub tree: SceneGraph<'w, 's, AnyNodeQueryData, ()>,
    /// Edge entities — needed for `MaterializeEdge<FigmaNode>` and the
    /// `kyoso_graph::for_each_edge` helper. `FigmaEdge` has no
    /// variants yet, so the edge enum is `()` and the materializer is
    /// essentially a presence check.
    pub edges: Query<'w, 's, Entity, With<FigmaEdge>>,
}

impl FigmaNodeQuery<'_, '_> {
    /// Materialize the node at `entity` into a [`Node`], or `None` if
    /// no such Figma node exists. Dispatches on [`NodeKind`].
    pub fn get(&self, entity: Entity) -> Option<Node> {
        let (_, kind) = self.kinds.get(entity).ok()?;
        match *kind {
            NodeKind::Frame => self
                .frames
                .get(entity)
                .ok()
                .map(Frame::materialize)
                .map(Frame::wrap),
            NodeKind::Rectangle => self
                .rectangles
                .get(entity)
                .ok()
                .map(Rectangle::materialize)
                .map(Rectangle::wrap),
            NodeKind::Text => self
                .texts
                .get(entity)
                .ok()
                .map(Text::materialize)
                .map(Text::wrap),
        }
    }

    /// Iterate every Figma node as `(Entity, Node)`. Skips entities
    /// whose typed sub-query lookup fails (component inconsistency).
    pub fn iter(&self) -> impl Iterator<Item = (Entity, Node)> + '_ {
        self.kinds
            .iter()
            .filter_map(move |(entity, _)| self.get(entity).map(|n| (entity, n)))
    }

    /// Typed iteration over frames — no `NodeKind` dispatch cost.
    pub fn frames(&self) -> impl Iterator<Item = (Entity, FrameData)> + '_ {
        self.frames.iter().map(|item| (item.entity, Frame::materialize(item)))
    }

    /// Typed iteration over rectangles.
    pub fn rectangles(&self) -> impl Iterator<Item = (Entity, RectangleData)> + '_ {
        self.rectangles.iter().map(|item| (item.entity, Rectangle::materialize(item)))
    }

    /// Typed iteration over text nodes.
    pub fn texts(&self) -> impl Iterator<Item = (Entity, TextData)> + '_ {
        self.texts.iter().map(|item| (item.entity, Text::materialize(item)))
    }

    /// Count nodes of a given kind.
    pub fn count(&self, kind: NodeKind) -> usize {
        match kind {
            NodeKind::Frame => self.frames.iter().count(),
            NodeKind::Rectangle => self.rectangles.iter().count(),
            NodeKind::Text => self.texts.iter().count(),
        }
    }
}

impl Materialize<FigmaNode> for FigmaNodeQuery<'_, '_> {
    fn materialize_any(&self, entity: Entity) -> Option<Node> {
        self.get(entity)
    }
}

impl MaterializeEdge<FigmaNode> for FigmaNodeQuery<'_, '_> {
    /// `FigmaEdge` has no typed variants yet — `G::Edge` is `()`, so
    /// this acts as a presence check: `Some(())` if `entity` carries
    /// `FigmaEdge`, `None` otherwise. When typed edges land
    /// (`EdgeCategory` / reference edges), this dispatches per-variant
    /// just like the node side.
    fn materialize_any_edge(&self, entity: Entity) -> Option<()> {
        self.edges.get(entity).ok().map(|_| ())
    }
}

// Now add the generic `walk_subtree` back as an inherent helper that
// delegates to the trait. Keeps the existing API name stable for
// Figma callers; under the hood it's the generic Materialize+SceneGraph
// composition.
impl FigmaNodeQuery<'_, '_> {

    /// Pre-order DFS over the subtree rooted at `root`, yielding
    /// `(Entity, depth, Node)` for each visited Figma node. Children
    /// are visited in `OrderKey` order. Skips entities whose typed
    /// sub-query lookup fails (i.e. entities that have `NodeKind` but
    /// not the matching variant's data components).
    ///
    /// This is the canonical typed-graph traversal: compose
    /// [`SceneGraph::walk_dfs_with_depth`] with
    /// [`Materialize::materialize_any`].
    pub fn walk_subtree(
        &self,
        root: Entity,
    ) -> impl Iterator<Item = (Entity, usize, Node)> + '_ {
        self.tree
            .walk_dfs_with_depth(root)
            .filter_map(move |(entity, depth)| {
                self.materialize_any(entity).map(|n| (entity, depth, n))
            })
    }

    /// Pre-order DFS over the subtree rooted at `root`, dispatching to
    /// the visitor's per-variant `visit_*` method at each step. The
    /// visitor's return controls descent (`Continue`/`SkipChildren`/`Stop`).
    /// This is the ECS-side analogue of [`crate::walker::Walker`].
    pub fn walk_visit<V: SceneVisitor>(&self, root: Entity, visitor: &mut V) {
        let _ = self.walk_visit_inner(root, 0, None, visitor);
    }

    fn walk_visit_inner<V: SceneVisitor>(
        &self,
        entity: Entity,
        depth: usize,
        parent: Option<Entity>,
        visitor: &mut V,
    ) -> Traverse {
        let ctx = VisitContext { entity, depth, parent };
        let Ok((_, kind)) = self.kinds.get(entity) else {
            return Traverse::Continue;
        };

        let traverse = match *kind {
            NodeKind::Frame => match self.frames.get(entity) {
                Ok(item) => visitor.visit_frame(&Frame::materialize(item), &ctx),
                Err(_) => Traverse::Continue,
            },
            NodeKind::Rectangle => match self.rectangles.get(entity) {
                Ok(item) => visitor.visit_rectangle(&Rectangle::materialize(item), &ctx),
                Err(_) => Traverse::Continue,
            },
            NodeKind::Text => match self.texts.get(entity) {
                Ok(item) => visitor.visit_text(&Text::materialize(item), &ctx),
                Err(_) => Traverse::Continue,
            },
        };

        match traverse {
            Traverse::Stop => Traverse::Stop,
            Traverse::SkipChildren => Traverse::Continue,
            Traverse::Continue => {
                for child in self.tree.children(entity) {
                    if self.walk_visit_inner(child, depth + 1, Some(entity), visitor)
                        == Traverse::Stop
                    {
                        return Traverse::Stop;
                    }
                }
                Traverse::Continue
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::LayoutMode;
    use kyoso_graph::components::{EdgeFrom, EdgeTo};
    use kyoso_graph::tree::{OrderKey, TreeEdge, TreeParent};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app
    }

    /// Spawn a Frame root with two children — a Rectangle (order "a")
    /// and a Text (order "b"). Returns `(root, rect, text)`. Edge
    /// entities carry `FigmaEdge` so `MaterializeEdge<FigmaNode>` and
    /// `for_each_edge` see them.
    fn spawn_tree_fixture(commands: &mut Commands) -> (Entity, Entity, Entity) {
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
        commands.spawn((EdgeFrom(root), EdgeTo(rect), TreeEdge, FigmaEdge));
        commands.spawn((EdgeFrom(root), EdgeTo(text), TreeEdge, FigmaEdge));
        (root, rect, text)
    }

    #[test]
    fn spawn_frame_round_trips_through_figma_node_query() {
        let mut app = test_app();

        let spawned = app
            .world_mut()
            .run_system_once(|mut commands: Commands| {
                Node::Frame(FrameData {
                    frame: Frame {
                        name: "hello".into(),
                        layout_mode: LayoutMode::Horizontal,
                        ..default()
                    },
                    size: Size { width: 100.0, height: 50.0 },
                })
                .spawn(&mut commands, Transform::IDENTITY)
            })
            .expect("spawn system runs");

        let got = app
            .world_mut()
            .run_system_once(move |q: FigmaNodeQuery| q.get(spawned))
            .expect("read system runs");

        match got {
            Some(Node::Frame(data)) => {
                assert_eq!(data.frame.name, "hello");
                assert_eq!(data.size.width, 100.0);
                assert_eq!(data.kind(), NodeKind::Frame);
            }
            other => panic!("expected Node::Frame, got {other:?}"),
        }
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

    #[test]
    fn enum_dispatch_delegates_node_behavior() {
        let node = Node::Text(TextData::default());
        assert_eq!(node.kind(), NodeKind::Text);
        assert_eq!(node.size().width, 0.0);
    }

    /// Regression: when the sync layer inserts variant components
    /// directly (bypassing `*Data`'s Bundle spawn), Bevy's `#[require]`
    /// still attaches the `NodeKind` tag so `FigmaNodeQuery` finds the
    /// entity. Simulates a remote-sync codepath that inserts `Frame`
    /// and `Size` as independent component ops rather than as a Bundle.
    #[test]
    fn remote_spawn_path_gets_node_kind_via_require() {
        let mut app = test_app();

        let spawned = app
            .world_mut()
            .run_system_once(|mut commands: Commands| {
                // Insert Frame + Size individually — not via FrameData. This is
                // how kyoso_graph_sync's per-component ops materialize a remotely
                // spawned node locally.
                commands
                    .spawn((
                        Frame { name: "remote".into(), ..default() },
                        Size { width: 7.0, height: 9.0 },
                    ))
                    .id()
            })
            .expect("spawn system runs");

        // Verify the NodeKind tag was auto-inserted (the require attribute
        // on Frame is what makes this work for non-Bundle insertions).
        let kind = app
            .world()
            .entity(spawned)
            .get::<NodeKind>()
            .copied();
        assert_eq!(kind, Some(NodeKind::Frame), "NodeKind not auto-inserted on Frame");

        let got = app
            .world_mut()
            .run_system_once(move |q: FigmaNodeQuery| q.get(spawned))
            .expect("read system runs");

        match got {
            Some(Node::Frame(data)) => assert_eq!(data.frame.name, "remote"),
            other => panic!("expected Node::Frame, got {other:?}"),
        }
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

    /// Compile-only check that the typed-graph trait machinery in
    /// `kyoso_graph` lines up with `kyoso_figma`'s `NodeVariant` impls:
    /// every `Frame`/`Rectangle`/`Text` traces back to `FigmaNode` as
    /// the typed graph, and the discriminator + owned-enum + any-data
    /// types all match what we'd write by hand.
    #[test]
    fn typed_graph_wiring_compiles() {
        // Every variant's `Graph` resolves to FigmaNode.
        fn assert_graph_is_figma_node<V>()
        where
            V: NodeVariant<Graph = FigmaNode>,
        {
        }
        assert_graph_is_figma_node::<Frame>();
        assert_graph_is_figma_node::<Rectangle>();
        assert_graph_is_figma_node::<Text>();

        // `<FigmaNode as Graph>::Node == Node`.
        // (Use the produced value, since type-equality assertions need turbofish.)
        let owned: <FigmaNode as kyoso_graph::Graph>::Node =
            <Frame as NodeVariant>::wrap(FrameData::default());
        assert!(matches!(owned, Node::Frame(_)));

        // `<FigmaNode as Graph>::NodeDiscriminator == NodeKind`.
        let kind: <FigmaNode as kyoso_graph::Graph>::NodeDiscriminator = NodeKind::Rectangle;
        assert_eq!(kind, NodeKind::Rectangle);
    }

    #[test]
    fn walk_subtree_yields_pre_order_typed_nodes() {
        let mut app = test_app();
        let (root, rect, text) = app
            .world_mut()
            .run_system_once(|mut commands: Commands| spawn_tree_fixture(&mut commands))
            .expect("spawn fixture");

        let walked = app
            .world_mut()
            .run_system_once(move |q: FigmaNodeQuery| {
                q.walk_subtree(root)
                    .map(|(e, d, n)| (e, d, super::super::descriptor::node_type_str(&n)))
                    .collect::<Vec<_>>()
            })
            .expect("walk system runs");

        assert_eq!(
            walked,
            vec![
                (root, 0, "frame"),
                (rect, 1, "rectangle"),
                (text, 1, "text"),
            ],
        );
    }

    #[test]
    fn for_each_node_and_edge_helpers() {
        let mut app = test_app();
        app.world_mut()
            .run_system_once(|mut commands: Commands| spawn_tree_fixture(&mut commands))
            .expect("spawn fixture");

        let counts = app
            .world_mut()
            .run_system_once(
                |node_markers: Query<Entity, With<FigmaNode>>,
                 edge_markers: Query<Entity, With<FigmaEdge>>,
                 figma_q: FigmaNodeQuery| {
                    let mut node_count = 0usize;
                    let mut edge_count = 0usize;
                    kyoso_graph::for_each_node::<FigmaNode, _>(
                        &node_markers,
                        &figma_q,
                        |_, _node| node_count += 1,
                    );
                    kyoso_graph::for_each_edge::<FigmaNode, _>(
                        &edge_markers,
                        &figma_q,
                        |_, _edge| edge_count += 1,
                    );
                    (node_count, edge_count)
                },
            )
            .expect("run system");

        // 3 nodes (Frame + Rectangle + Text), 2 tree edges.
        assert_eq!(counts, (3, 2));
    }

    #[test]
    fn typed_iterators_per_variant() {
        let mut app = test_app();
        app.world_mut()
            .run_system_once(|mut commands: Commands| {
                Node::Frame(FrameData::default()).spawn(&mut commands, Transform::IDENTITY);
                Node::Rectangle(RectangleData::default()).spawn(&mut commands, Transform::IDENTITY);
                Node::Text(TextData::default()).spawn(&mut commands, Transform::IDENTITY);
            })
            .expect("spawn system runs");

        let counts = app
            .world_mut()
            .run_system_once(|q: FigmaNodeQuery| {
                (
                    q.frames().count(),
                    q.rectangles().count(),
                    q.texts().count(),
                    q.iter().count(),
                    q.count(NodeKind::Frame),
                )
            })
            .expect("read system runs");

        assert_eq!(counts, (1, 1, 1, 3, 1));
    }
}
