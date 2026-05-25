//! Shared [`TraversalQuery`] runners — generic over any
//! [`GraphTraverse`] implementor.
//!
//! Each `WorldXxxView` (`WorldTreeView`, `WorldGraphView`,
//! `WorldSceneView`) has its own `traverse` / `traverse_with` methods
//! that forward to these free functions, picking which inner
//! `GraphTraverse` (tree-children, entity-edges, …) drives the walk.

use bevy::ecs::resource::Resource;
use bevy::ecs::world::World;
use bevy::prelude::Entity;

use crate::traverse::{GraphTraverse, Step, TraversalNode};

use super::query::{Order, TraversalQuery, WorldEntityRef};
use super::resolver::{resolve_node_ref, NodeIdResolver};

/// Run a [`TraversalQuery`] over `graph` (any [`GraphTraverse`]
/// impl), reading runtime-typed component filters and the
/// `step_with` policy through `&World`.
///
/// Returns raw [`TraversalNode`] rows — no id resolution. Use
/// [`run_traversal_with`] when you want resolved [`WorldEntityRef`]
/// rows.
pub fn run_traversal<G>(
    graph: &G,
    world: &World,
    q: &TraversalQuery,
) -> Vec<TraversalNode>
where
    G: GraphTraverse,
{
    let Some(root) = q.root else {
        return Vec::new();
    };
    let max_depth = q.max_depth;

    // Policy: depth cap first (cheap, no entity touch); then the
    // caller's component-aware `step_with` closure, if any.
    let policy = |tn: &TraversalNode| -> Step {
        if let Some(max) = max_depth {
            if tn.depth >= max {
                return Step::Skip;
            }
        }
        if let Some(step) = &q.step {
            return step(tn, world.entity(tn.entity));
        }
        Step::Visit
    };

    let walk: Box<dyn Iterator<Item = TraversalNode> + '_> = match q.order {
        Order::Bfs => Box::new(graph.bfs_walk(root, policy)),
        Order::Dfs => Box::new(graph.dfs_walk(root, policy)),
    };

    walk.filter(|tn| {
        let er = world.entity(tn.entity);
        q.filters.iter().all(|f| f(er))
    })
    .collect()
}

/// Resolved variant of [`run_traversal`]: looks up `R: NodeIdResolver`
/// as a `Resource` from `world` and projects each row through
/// [`resolve_node_ref`] into a [`WorldEntityRef<R::Id>`].
pub fn run_traversal_with<G, R>(
    graph: &G,
    world: &World,
    q: &TraversalQuery,
) -> Vec<WorldEntityRef<R::Id>>
where
    G: GraphTraverse,
    R: NodeIdResolver + Resource,
{
    let resolver = world.get_resource::<R>();
    run_traversal(graph, world, q)
        .into_iter()
        .map(|tn| WorldEntityRef {
            id: resolve_node_ref::<R>(tn.entity, resolver),
            entity: tn.entity,
            depth: tn.depth,
            parent: tn.parent,
        })
        .collect()
}

/// Schemaless dump of every component on `entity`, returned as
/// fully-qualified Rust type names from the entity's archetype.
/// Returns an empty `Vec` if `entity` doesn't exist (no panic).
pub fn component_names_for(world: &World, entity: Entity) -> Vec<String> {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return Vec::new();
    };
    let components = world.components();
    entity_ref
        .archetype()
        .components()
        .iter()
        .filter_map(|cid| components.get_info(*cid))
        .map(|info| info.name().to_string())
        .collect()
}
