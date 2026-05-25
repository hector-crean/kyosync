//! The "view" SystemParams: bundles of `&World` plus an inner query
//! ([`TreeQuery`], [`GraphQuery`], or [`Scene`]) so the
//! [`TraversalQuery`] runner can do runtime-typed component reads.
//!
//! Three views, one per inner query:
//!
//! | View | Inner | Walks |
//! |---|---|---|
//! | [`WorldTreeView<F>`] | [`TreeQuery<F>`] | Hierarchy (`Children`/`ChildOf`, `OrderKey`-sorted) |
//! | [`WorldGraphView<N, E, NF, EF>`] | [`GraphQuery<N, E, NF, EF>`] | Entity-edges (`EdgeFrom`/`EdgeTo`) |
//! | [`WorldSceneView<N, E, NF, EF>`] | [`Scene<N, E, NF, EF>`] | Both — explicit `traverse_tree` / `traverse_graph` |
//!
//! All three carry `&World` so:
//!
//! - [`TraversalQuery::require::<T>`] / [`exclude::<T>`] can check
//!   `world.entity(e).contains::<T>()` for runtime-chosen `T`.
//! - The `step_with` closure receives an `EntityRef` borrowed from the
//!   live world for per-component-value decisions.
//! - [`NodeIdResolver`] resources are picked up via
//!   `world.get_resource::<R>()` for [`NodeRef::Replicated`] resolution.
//!
//! These views are exclusive-access (one `&World`) — they don't
//! parallelise with other systems. For in-system parallel-safe walks
//! that don't need runtime-typed reads, use the inner query directly
//! ([`TreeQuery`] / [`GraphQuery`] / [`Scene`]).
//!
//! [`TraversalQuery`]: super::TraversalQuery
//! [`TraversalQuery::require::<T>`]: super::TraversalQuery::require
//! [`exclude::<T>`]: super::TraversalQuery::exclude
//! [`NodeIdResolver`]: super::NodeIdResolver
//! [`NodeRef::Replicated`]: super::NodeRef::Replicated

use bevy::ecs::entity::Entity;
use bevy::ecs::query::{QueryData, QueryFilter};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::SystemParam;
use bevy::ecs::world::World;

use crate::queries::GraphQuery;
use crate::scene::Scene;
use crate::traverse::TraversalNode;
use crate::tree::TreeQuery;

use super::query::{TraversalQuery, WorldEntityRef};
use super::resolver::NodeIdResolver;
use super::runner::{component_names_for, run_traversal, run_traversal_with};

// ============================================================================
// WorldTreeView — &World + TreeQuery
// ============================================================================

/// Read-only view bundling `&World` with a [`TreeQuery<F>`].
///
/// The agent-traversal entry point for hierarchy walks. `F` defaults
/// to `()` (no filter) — the right shape for agent paths that should
/// surface bare overlay entities (via [`NodeRef::Local`]). Specialise
/// to e.g. `WorldTreeView<With<SceneNode>>` for a filtered binding-
/// layer view.
///
/// [`NodeRef::Local`]: super::NodeRef::Local
#[derive(SystemParam)]
pub struct WorldTreeView<'w, 's, F = ()>
where
    F: QueryFilter + 'static,
{
    pub(crate) world: &'w World,
    pub(crate) tree: TreeQuery<'w, 's, F>,
}

impl<'w, 's, F> WorldTreeView<'w, 's, F>
where
    F: QueryFilter + 'static,
{
    pub fn world(&self) -> &World {
        self.world
    }

    pub fn tree(&self) -> &TreeQuery<'w, 's, F> {
        &self.tree
    }

    /// Walk via the inner tree. Yields raw [`TraversalNode`] rows.
    pub fn traverse(&self, q: &TraversalQuery) -> Vec<TraversalNode> {
        run_traversal(&self.tree, self.world, q)
    }

    /// Walk + resolve each yielded entity to its durable id via `R`.
    pub fn traverse_with<R>(&self, q: &TraversalQuery) -> Vec<WorldEntityRef<R::Id>>
    where
        R: NodeIdResolver + Resource,
    {
        run_traversal_with::<_, R>(&self.tree, self.world, q)
    }

    /// Schemaless component-name dump for `entity`. Empty `Vec` if
    /// `entity` doesn't exist.
    pub fn component_names(&self, entity: Entity) -> Vec<String> {
        component_names_for(self.world, entity)
    }
}

// ============================================================================
// WorldGraphView — &World + GraphQuery
// ============================================================================

/// Read-only view bundling `&World` with a [`GraphQuery<N, E, NF, EF>`].
///
/// Use for entity-edge walks (`EdgeFrom`/`EdgeTo`) with runtime-typed
/// component filters. Typical caller: an agent tool that wants
/// "find all paths through Reference edges of length ≥ 2 starting
/// from this Frame, where the target has a `Selected` component."
#[derive(SystemParam)]
pub struct WorldGraphView<'w, 's, N, E, NF = (), EF = ()>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    pub(crate) world: &'w World,
    pub(crate) graph: GraphQuery<'w, 's, N, E, NF, EF>,
}

impl<'w, 's, N, E, NF, EF> WorldGraphView<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    pub fn world(&self) -> &World {
        self.world
    }

    pub fn graph(&self) -> &GraphQuery<'w, 's, N, E, NF, EF> {
        &self.graph
    }

    /// Walk via the inner entity-edge graph. Yields raw
    /// [`TraversalNode`] rows.
    pub fn traverse(&self, q: &TraversalQuery) -> Vec<TraversalNode> {
        run_traversal(&self.graph, self.world, q)
    }

    /// Walk + resolve each yielded entity to its durable id via `R`.
    pub fn traverse_with<R>(&self, q: &TraversalQuery) -> Vec<WorldEntityRef<R::Id>>
    where
        R: NodeIdResolver + Resource,
    {
        run_traversal_with::<_, R>(&self.graph, self.world, q)
    }

    pub fn component_names(&self, entity: Entity) -> Vec<String> {
        component_names_for(self.world, entity)
    }
}

// ============================================================================
// WorldSceneView — &World + Scene (combined)
// ============================================================================

/// Read-only view bundling `&World` with a [`Scene<N, E, NF, EF>`] —
/// both hierarchy and entity-edge graph in one SystemParam.
///
/// Use when a single agent operation crosses both layers, e.g. "for
/// each descendant of this Frame, list its outgoing Reference edges."
/// The methods are deliberately split into `traverse_tree*` and
/// `traverse_graph*` — no ambiguous default.
#[derive(SystemParam)]
pub struct WorldSceneView<'w, 's, N, E, NF = (), EF = ()>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    pub(crate) world: &'w World,
    pub(crate) scene: Scene<'w, 's, N, E, NF, EF>,
}

impl<'w, 's, N, E, NF, EF> WorldSceneView<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    pub fn world(&self) -> &World {
        self.world
    }

    pub fn scene(&self) -> &Scene<'w, 's, N, E, NF, EF> {
        &self.scene
    }

    /// Walk via the inner [`TreeQuery`].
    pub fn traverse_tree(&self, q: &TraversalQuery) -> Vec<TraversalNode> {
        run_traversal(&self.scene.tree, self.world, q)
    }

    /// Walk via the inner [`TreeQuery`] + id resolution.
    pub fn traverse_tree_with<R>(&self, q: &TraversalQuery) -> Vec<WorldEntityRef<R::Id>>
    where
        R: NodeIdResolver + Resource,
    {
        run_traversal_with::<_, R>(&self.scene.tree, self.world, q)
    }

    /// Walk via the inner [`GraphQuery`] (entity-edges).
    pub fn traverse_graph(&self, q: &TraversalQuery) -> Vec<TraversalNode> {
        run_traversal(&self.scene.graph, self.world, q)
    }

    /// Walk via the inner [`GraphQuery`] + id resolution.
    pub fn traverse_graph_with<R>(&self, q: &TraversalQuery) -> Vec<WorldEntityRef<R::Id>>
    where
        R: NodeIdResolver + Resource,
    {
        run_traversal_with::<_, R>(&self.scene.graph, self.world, q)
    }

    pub fn component_names(&self, entity: Entity) -> Vec<String> {
        component_names_for(self.world, entity)
    }
}
