//! `SceneWorld` — binding-layer handle wrapping a Bevy `App` with a
//! cached [`SystemState`] for the traversal view.
//!
//! Designed for agent / MCP / wasm-bindgen / JS-FFI call sites: each
//! method takes `&mut self`, runs against the owned `App`, and returns
//! owned data (no borrow of the world leaks out). The traversal
//! `SystemState` is built once at construction and reused for every
//! call.
//!
//! ## Two flavours of read
//!
//! - **Structural** ([`SceneWorld::traverse`],
//!   [`SceneWorld::component_names`]) — delegate to
//!   [`WorldGraphView`] and don't need per-variant typing. Use these
//!   when you want raw tree shape + ad-hoc component-presence filters.
//!
//! - **Typed** ([`SceneWorld::iter_as`], [`SceneWorld::read_as`],
//!   [`SceneWorld::traverse_as`], [`SceneWorld::traverse_typed`]) —
//!   project each entity through a [`NodeVariant`] (single variant) or
//!   the [`NodeVariants`] tuple (closed sum across all of a graph's
//!   variants). These need `&mut World` for ad-hoc
//!   [`QueryState`] construction, which is fine here because we own
//!   the app.
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_core::{KyosoCorePlugin, SceneNode, SceneWorld};
//! use kyoso_core::traversal::{Order, TraversalQuery};
//!
//! let mut sw = SceneWorld::new();
//! // Optional: wire up replicated sync.
//! // sw.app_mut().add_plugins(KyosoCorePlugin { server_url, room });
//!
//! // …spawn / pump frames…
//!
//! let rows = sw.traverse_typed::<SceneNode>(
//!     &TraversalQuery::new().start_at(root).order(Order::Bfs),
//! );
//! for (row, node) in rows {
//!     // `row` carries the depth / parent / NodeRef metadata,
//!     // `node` is the closed-sum `kyoso_core::Node` (Frame / Rectangle / Text).
//! }
//! ```

use bevy::ecs::query::{QueryState, With};
use bevy::ecs::system::SystemState;
use bevy::prelude::*;
use kyoso_crdt::CrdtId;
use kyoso_graph::descriptor::{GraphMetadata, NodeDescriptor, SceneGraphDescriptor};
use kyoso_graph::traversal::{TraversalQuery, WorldEntityRef, WorldTreeView};
use kyoso_graph::tree::TreeQuery;
use kyoso_graph::variant::{NodeVariant, NodeVariants};
use kyoso_graph::Graph;
use kyoso_graph_sync::EntityCrdtIndex;

use crate::descriptor::scene_node_descriptor;
use crate::SceneNode;

/// Owned Bevy `App` plus a cached traversal-view [`SystemState`]. The
/// natural binding-layer handle: each method takes `&mut self`, runs
/// against the owned world, returns owned data.
pub struct SceneWorld {
    app: App,
    /// `SystemState` for materialising the traversal view. Built once
    /// at construction so per-call cost is just one
    /// [`SystemState::get`] (cache lookup of resolved system params).
    ///
    /// The cached view is [`WorldTreeView`] (unfiltered) — the agent
    /// path walks hierarchy and should surface bare overlays via
    /// [`kyoso_graph::traversal::NodeRef::Local`]. Callers that want
    /// a typed view (filtered, or graph-edge access) can build their
    /// own [`SystemState`] against [`kyoso_graph::traversal::WorldGraphView`]
    /// or [`kyoso_graph::traversal::WorldSceneView`].
    tree_state: SystemState<WorldTreeView<'static, 'static>>,
}

impl SceneWorld {
    /// Build a fresh `SceneWorld` with `MinimalPlugins` already added.
    /// Callers add `KyosoCorePlugin` (and any others) via
    /// [`Self::app_mut`].
    pub fn new() -> Self {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        let tree_state = SystemState::new(app.world_mut());
        Self { app, tree_state }
    }

    /// Wrap an existing `App`. Use this when the app is already
    /// configured (e.g. tests bringing their own plugin set, or
    /// integrating with an outer harness like `kyoso_client::AppPlugin`).
    pub fn from_app(mut app: App) -> Self {
        let tree_state = SystemState::new(app.world_mut());
        Self { app, tree_state }
    }

    /// Borrow the wrapped `App` immutably.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Borrow the wrapped `App` mutably. Use this to add plugins,
    /// register resources, etc.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Borrow the wrapped `World` immutably.
    pub fn world(&self) -> &World {
        self.app.world()
    }

    /// Borrow the wrapped `World` mutably.
    pub fn world_mut(&mut self) -> &mut World {
        self.app.world_mut()
    }

    /// Pump one frame of the wrapped `App`.
    pub fn update(&mut self) {
        self.app.update();
    }

    /// Materialise the cached traversal view. Each call re-runs the
    /// resolved-`SystemParam` plumbing (cheap — no component-id
    /// resolution, that already happened at construction).
    ///
    /// Panics if `SystemParam` validation fails. That can only happen
    /// if the underlying resources go missing, which the wrapper
    /// itself controls — so callers can treat this as infallible.
    pub fn tree_view(&mut self) -> WorldTreeView<'_, '_> {
        self.tree_state
            .get(self.app.world())
            .expect("SystemParam validation: WorldTreeView resources missing")
    }

    // ========================================================================
    // Structural reads — delegate to WorldTreeView
    // ========================================================================

    /// Run a [`TraversalQuery`] over the wrapped world, resolving
    /// each entity to its [`CrdtId`] via the [`EntityCrdtIndex`]
    /// resource (falling back to [`kyoso_graph::traversal::NodeRef::Local`]
    /// for unregistered entities). Walks the **tree** (hierarchy);
    /// for entity-edge graph walks, build your own
    /// [`kyoso_graph::traversal::WorldGraphView`] SystemState.
    pub fn traverse(&mut self, q: &TraversalQuery) -> Vec<WorldEntityRef<CrdtId>> {
        self.tree_view().traverse_with::<EntityCrdtIndex>(q)
    }

    /// Schemaless component-name dump for `entity`. See
    /// [`WorldTreeView::component_names`].
    pub fn component_names(&mut self, entity: Entity) -> Vec<String> {
        self.tree_view().component_names(entity)
    }

    /// Build a fully-typed [`SceneGraphDescriptor`] for the entire
    /// scene. Walks every tree root via [`TreeQuery`], materialises
    /// each entity through the [`SceneNode`] graph's
    /// [`NodeVariants`] tuple impl, and emits a `node_type` + serde
    /// `data` payload per node — the agent-facing JSON shape.
    pub fn scene_descriptor(&mut self) -> SceneGraphDescriptor {
        // Build the per-variant `QueryState` cache once (`&mut World`)
        // before we acquire the read-only tree view.
        let mut states = <<SceneNode as Graph>::Variants as NodeVariants>::build_states(
            self.app.world_mut(),
        );

        // Ad-hoc `SystemState<TreeQuery>` so we don't conflict with
        // the cached `WorldGraphView` SystemState (whose lifetime
        // pins the world borrow for the whole call).
        let mut tree_state = SystemState::<TreeQuery>::new(self.app.world_mut());
        let tree = tree_state
            .get(self.app.world())
            .expect("SystemParam validation: TreeQuery resources missing");

        let world = self.app.world();
        let roots = tree.roots();
        let node_count = tree.node_count();
        let root_count = roots.len();
        let max_depth = tree.max_depth();

        let root_descriptors: Vec<NodeDescriptor> = roots
            .into_iter()
            .filter_map(|r| build_scene_node_descriptor(&tree, world, &mut states, r))
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

    // ========================================================================
    // Typed reads — need &mut World for ad-hoc QueryState construction
    // ========================================================================

    /// Enumerate every entity matching variant `V`, returning
    /// `(entity, materialised V::Data)` pairs. Single-pass via a
    /// tuple `QueryData` over `(Entity, V::Query)`.
    ///
    /// One ad-hoc `QueryState` per call. Fine off the hot path; if
    /// you find this in a per-frame system, hand-write a SystemParam
    /// holding the `Query` instead.
    pub fn iter_as<V: NodeVariant>(&mut self) -> Vec<(Entity, V::Data)>
    where
        for<'a> bevy::ecs::query::ROQueryItem<'a, 'a, V::Query>: 'a,
    {
        let world = self.app.world_mut();
        let mut state: QueryState<V::Query, With<V>> = QueryState::new(world);
        // Two-pass: collect entities first (releases the V::Query borrow),
        // then materialise per-entity. Avoids leaking ROQueryItem lifetimes
        // out of the per-iter closure.
        let entities: Vec<Entity> = {
            let mut markers: QueryState<Entity, With<V>> = QueryState::new(world);
            let es = markers.iter(world).collect();
            es
        };
        entities
            .into_iter()
            .filter_map(|e| state.get(world, e).ok().map(|item| (e, V::materialize(item))))
            .collect()
    }

    /// Single-entity typed read. `None` if the entity doesn't satisfy
    /// `V::Query` (wrong variant, despawned, or missing required
    /// components).
    pub fn read_as<V: NodeVariant>(&mut self, entity: Entity) -> Option<V::Data> {
        let world = self.app.world_mut();
        let mut state: QueryState<V::Query, With<V>> = QueryState::new(world);
        state.get(world, entity).ok().map(V::materialize)
    }

    /// Walk + per-variant materialisation. Runs the structural walk
    /// (depth / filters / `step_with`), then projects each row through
    /// `V::materialize`, dropping rows that don't match `V`.
    pub fn traverse_as<V: NodeVariant>(
        &mut self,
        q: &TraversalQuery,
    ) -> Vec<(WorldEntityRef<CrdtId>, V::Data)> {
        let refs = self.traverse(q);
        let world = self.app.world_mut();
        let mut state: QueryState<V::Query, With<V>> = QueryState::new(world);
        refs.into_iter()
            .filter_map(|r| {
                state
                    .get(world, r.entity)
                    .ok()
                    .map(|item| (r, V::materialize(item)))
            })
            .collect()
    }

    /// Walk + closed-sum dispatch. Each yielded entity is materialised
    /// into the graph's owned sum type [`Graph::Node`] via the
    /// [`NodeVariants`] tuple impl. Entities that match no variant are
    /// dropped silently.
    ///
    /// The per-variant `QueryState` cache is built once at call entry
    /// and reused for every entity in the walk — amortised across the
    /// row set.
    pub fn traverse_typed<G: Graph>(
        &mut self,
        q: &TraversalQuery,
    ) -> Vec<(WorldEntityRef<CrdtId>, G::Node)> {
        let refs = self.traverse(q);
        let world = self.app.world_mut();
        let mut states = <G::Variants as NodeVariants>::build_states(world);
        refs.into_iter()
            .filter_map(|r| {
                <G::Variants as NodeVariants>::try_materialize(&mut states, world, r.entity)
                    .map(|node| (r, node))
            })
            .collect()
    }
}

impl Default for SceneWorld {
    fn default() -> Self {
        Self::new()
    }
}

/// Recursively build a [`NodeDescriptor`] for the subtree rooted at
/// `entity`, materialising via the [`SceneNode`] graph's
/// [`NodeVariants`] tuple. Returns `None` if `entity` doesn't match
/// any variant (e.g. bare overlay entity).
fn build_scene_node_descriptor(
    tree: &TreeQuery,
    world: &World,
    states: &mut <<SceneNode as Graph>::Variants as NodeVariants>::States,
    entity: Entity,
) -> Option<NodeDescriptor> {
    let node = <<SceneNode as Graph>::Variants as NodeVariants>::try_materialize(
        states, world, entity,
    )?;
    let depth = tree.depth(entity);
    let children: Vec<NodeDescriptor> = tree
        .children(entity)
        .into_iter()
        .filter_map(|c| build_scene_node_descriptor(tree, world, states, c))
        .collect();
    Some(scene_node_descriptor(entity, &node, depth, children))
}
