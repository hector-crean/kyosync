//! Agent-facing read surface for kyoso scenes.
//!
//! Wraps [`kyoso_core::SceneWorld`] with tool-shaped methods an AI
//! agent (or MCP server, JS-FFI handler, …) would call: describe the
//! scene, list nodes by variant, inspect an entity, walk a subtree,
//! match a pattern. All methods return owned data — no borrows escape
//! the wrapper.
//!
//! The actual machinery (traversal, pattern matching, typed reads)
//! lives in `kyoso_graph` / `kyoso_core`; this crate is the *thin
//! aggregator* that gives those features one ergonomic surface and
//! exercises them in integration tests.

use bevy::prelude::*;
use kyoso_core::{
    Frame, FrameData, Node, Rectangle, RectangleData, SceneEdge, SceneNode, SceneWorld,
    Text, TextData,
};
use kyoso_crdt::CrdtId;
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph::descriptor::SceneGraphDescriptor;
use kyoso_graph::pattern::{Pattern, PatternBuilder};
use kyoso_graph::subgraph::Match;
use kyoso_graph::traversal::{TraversalQuery, WorldEntityRef};
use kyoso_graph::traverse::GraphTraverseEdges;
use kyoso_graph::tree::OrderKey;
use kyoso_graph_sync::EntityCrdtIndex;

/// Owned [`SceneWorld`] plus tool-shaped methods.
///
/// Methods that read produce owned values (the borrow of the inner
/// world doesn't leak out). That's the shape an agent SDK / MCP server
/// / JS-FFI bridge wants: each invocation is self-contained, results
/// can be serialised and shipped across process boundaries.
pub struct SceneAgent {
    sw: SceneWorld,
}

impl SceneAgent {
    pub fn new() -> Self {
        Self { sw: SceneWorld::new() }
    }

    /// Wrap an existing [`SceneWorld`] (e.g. one that already has a
    /// CRDT sync plugin attached).
    pub fn from_scene_world(sw: SceneWorld) -> Self {
        Self { sw }
    }

    /// Underlying handle for direct access — escape hatch when you
    /// need a method this wrapper doesn't expose.
    pub fn scene_world(&mut self) -> &mut SceneWorld {
        &mut self.sw
    }

    // ========================================================================
    // Scene-shape introspection (LLM-friendly)
    // ========================================================================

    /// Full LLM-shaped JSON dump of the scene. Each row has the
    /// node's variant tag (`"frame"` / `"rectangle"` / `"text"`),
    /// depth, and serde-encoded `data`.
    pub fn describe(&mut self) -> SceneGraphDescriptor {
        self.sw.scene_descriptor()
    }

    /// Schemaless component-name dump for an entity. Useful when the
    /// agent wants to know "what does this entity carry?" without
    /// committing to a typed variant. Empty if the entity doesn't
    /// exist.
    pub fn inspect(&mut self, entity: Entity) -> EntityReport {
        let component_names = self.sw.component_names(entity);
        let node = self.sw.read_as::<Frame>(entity).map(Node::Frame).or_else(|| {
            self.sw
                .read_as::<Rectangle>(entity)
                .map(Node::Rectangle)
        }).or_else(|| self.sw.read_as::<Text>(entity).map(Node::Text));
        EntityReport {
            entity,
            node,
            component_names,
        }
    }

    // ========================================================================
    // Typed-variant listing
    // ========================================================================

    /// Every Frame in the scene with its owned bundle data.
    pub fn list_frames(&mut self) -> Vec<(Entity, FrameData)> {
        self.sw.iter_as::<Frame>()
    }

    /// Every Rectangle in the scene with its owned bundle data.
    pub fn list_rectangles(&mut self) -> Vec<(Entity, RectangleData)> {
        self.sw.iter_as::<Rectangle>()
    }

    /// Every Text node in the scene with its owned bundle data.
    pub fn list_texts(&mut self) -> Vec<(Entity, TextData)> {
        self.sw.iter_as::<Text>()
    }

    // ========================================================================
    // Subtree walks
    // ========================================================================

    /// Walk the subtree rooted at `root` and resolve each entity to
    /// its closed-sum [`Node`] form via the `(Frame, Rectangle, Text)`
    /// variant tuple. Yields `(WorldEntityRef<CrdtId>, Node)` rows.
    pub fn subtree_typed(
        &mut self,
        root: Entity,
        query: TraversalQuery,
    ) -> Vec<(WorldEntityRef<CrdtId>, Node)> {
        self.sw.traverse_typed::<SceneNode>(&query.start_at(root))
    }

    /// Walk the subtree rooted at `root`. Returns `WorldEntityRef`
    /// rows with `NodeRef::Replicated(CrdtId)` for entities bound in
    /// the sync index, `NodeRef::Local(u64)` otherwise.
    pub fn subtree(
        &mut self,
        root: Entity,
        query: TraversalQuery,
    ) -> Vec<WorldEntityRef<CrdtId>> {
        self.sw.traverse(&query.start_at(root))
    }

    // ========================================================================
    // Pattern matching (subgraph isomorphism)
    // ========================================================================

    /// Run a [`Pattern`] over the scene's entity-edge graph and return
    /// every binding. The pattern is built by the caller via
    /// [`SceneAgent::pattern_builder`] (or constructed externally).
    ///
    /// Implementation: builds a fresh
    /// [`SystemState<WorldGraphView<&SceneNode, &SceneEdge>>`] inside
    /// to walk the entity-edge graph, then runs `subgraph_matches`.
    pub fn find_matches(&mut self, pattern: &Pattern<'_>) -> Vec<Match> {
        use bevy::ecs::system::SystemState;
        use kyoso_graph::traversal::WorldGraphView;

        let world = self.sw.world_mut();
        let mut state: SystemState<WorldGraphView<&SceneNode, &SceneEdge>> =
            SystemState::new(world);
        let view = state
            .get(self.sw.world())
            .expect("SystemParam validation: WorldGraphView resources missing");
        view.graph().subgraph_matches(pattern).collect()
    }

    /// Convenience: a [`PatternBuilder`] ready to compose. Build the
    /// pattern, call [`PatternBuilder::build`], then hand the result
    /// to [`find_matches`](Self::find_matches).
    pub fn pattern_builder<'p>() -> PatternBuilder<'p> {
        PatternBuilder::new()
    }
}

impl Default for SceneAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`SceneAgent::inspect`] returns: the entity itself, its
/// closed-sum [`Node`] materialisation (if it matches a known
/// variant), and the schemaless component-name dump (always
/// populated for live entities).
#[derive(Clone, Debug)]
pub struct EntityReport {
    pub entity: Entity,
    pub node: Option<Node>,
    pub component_names: Vec<String>,
}

// ============================================================================
// Test/demo helpers — fixture builders so agent tests + the demo binary
// share scene-construction code.
// ============================================================================

/// Handles to the entities spawned by [`spawn_demo_scene`].
pub struct DemoSceneEntities {
    pub root: Entity,
    pub header: Entity,
    pub label: Entity,
    pub body: Entity,
    pub body_caption: Entity,
    /// Cross-frame edge entity from `label` → `body_caption`.
    pub label_to_caption: Entity,
}

/// Spawn a small, deliberately-shaped scene for tests + the demo:
///
/// ```text
/// root (Frame "Root")
/// ├── header (Frame "Header")
/// │   └── label (Text "Title")  ─────────────┐
/// └── body (Rectangle)                       │
///     └── body_caption (Text "Caption")  ◀───┘ (cross-frame edge)
/// ```
///
/// The cross-frame edge connects `label → body_caption` (an entity-edge
/// with `SceneEdge` marker — the kind of "weave" relation kyoso_client
/// uses). Hierarchy uses Bevy's `ChildOf` + `OrderKey`.
///
/// `root` is bound in [`EntityCrdtIndex`] under `CrdtId(1, 1)`,
/// `header` under `(1, 2)`, `body` under `(1, 3)`. The two text nodes
/// and the edge are deliberately NOT bound — they surface as
/// `NodeRef::Local` in traversal output.
pub fn spawn_demo_scene(sw: &mut SceneWorld) -> DemoSceneEntities {
    use bevy::ecs::system::RunSystemOnce;

    let ents = sw
        .world_mut()
        .run_system_once(|mut commands: Commands| {
            let root = commands
                .spawn((
                    FrameData {
                        frame: Frame { name: "Root".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                ))
                .id();
            let header = commands
                .spawn((
                    FrameData {
                        frame: Frame { name: "Header".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(root),
                    OrderKey("a".into()),
                ))
                .id();
            let label = commands
                .spawn((
                    TextData {
                        text: Text { content: "Title".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(header),
                    OrderKey("a".into()),
                ))
                .id();
            let body = commands
                .spawn((
                    RectangleData::default(),
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(root),
                    OrderKey("b".into()),
                ))
                .id();
            let body_caption = commands
                .spawn((
                    TextData {
                        text: Text { content: "Caption".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(body),
                    OrderKey("a".into()),
                ))
                .id();
            // Cross-frame entity-edge: label → body_caption.
            let label_to_caption = commands
                .spawn((EdgeFrom(label), EdgeTo(body_caption), SceneEdge))
                .id();
            DemoSceneEntities {
                root,
                header,
                label,
                body,
                body_caption,
                label_to_caption,
            }
        })
        .expect("spawn demo scene");

    // Bind a few entities in the sync index so the agent can show
    // both `NodeRef::Replicated` and `NodeRef::Local` rows.
    let mut index = EntityCrdtIndex::default();
    index.bind_node(ents.root, CrdtId::new(1, 1));
    index.bind_node(ents.header, CrdtId::new(1, 2));
    index.bind_node(ents.body, CrdtId::new(1, 3));
    sw.world_mut().insert_resource(index);

    ents
}
