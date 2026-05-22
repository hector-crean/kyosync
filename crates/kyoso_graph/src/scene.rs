//! Scene graph abstraction for hierarchical documents.
//!
//! Built on top of the generic graph core but enforces tree constraints:
//! - Nodes have at most one parent (via `TreeEdge`)
//! - Tree is acyclic
//! - Children are ordered by `OrderKey`

use bevy::{
    ecs::{query::{QueryData, QueryFilter}, system::SystemParam},
    prelude::*,
};

use crate::components::{EdgeFrom, EdgeTo, OutgoingEdges};
use crate::queries::GraphQuery;
use crate::tree::{OrderKey, TreeEdge, TreeParent};
use crate::Graph;

/// Scene graph abstraction for hierarchical documents.
///
/// This is a specialized view over the generic graph that enforces tree
/// semantics. All traversal methods respect `OrderKey` ordering and follow
/// parent-child relationships marked with `TreeEdge`.
#[derive(SystemParam)]
pub struct SceneGraph<'w, 's, N, E, NF = (), EF = ()>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    /// Underlying graph query
    pub graph: GraphQuery<'w, 's, N, E, NF, EF>,
    /// Tree parent component query
    pub tree_parent_q: Query<'w, 's, &'static TreeParent>,
    /// Tree edge marker query
    pub tree_edge_q: Query<'w, 's, &'static TreeEdge>,
    /// Order key query for child ordering
    pub order_key_q: Query<'w, 's, &'static OrderKey>,
    /// Edge endpoint queries for tree edge lookup
    pub edge_endpoints_q: Query<'w, 's, (&'static EdgeFrom, &'static EdgeTo)>,
    /// Outgoing edges for finding children
    pub outgoing_q: Query<'w, 's, &'static OutgoingEdges>,
}

impl<'w, 's, N, E, NF, EF> SceneGraph<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    // ========================================================================
    // Tree hierarchy queries
    // ========================================================================

    /// Get the parent of a node (if any).
    ///
    /// Returns `None` for root nodes.
    pub fn parent(&self, node: Entity) -> Option<Entity> {
        self.tree_parent_q
            .get(node)
            .ok()
            .and_then(|tp| tp.0)
    }

    /// Get all children of a node, ordered by `OrderKey`.
    ///
    /// Returns an empty vector if the node has no children.
    pub fn children(&self, parent: Entity) -> Vec<Entity> {
        let Ok(outgoing) = self.outgoing_q.get(parent) else {
            return Vec::new();
        };

        let mut children: Vec<(Entity, OrderKey)> = outgoing
            .iter()
            .filter_map(|edge| {
                // Only tree edges
                self.tree_edge_q.get(edge).ok()?;

                // Get target node
                let (_from, to) = self.edge_endpoints_q.get(edge).ok()?;
                let child = to.0;

                // Get order key
                let key = self.order_key_q.get(child).ok()?.clone();

                Some((child, key))
            })
            .collect();

        // Sort by OrderKey
        children.sort_by(|(_, a), (_, b)| a.cmp(b));

        children.into_iter().map(|(entity, _)| entity).collect()
    }

    /// Get all children with their order keys.
    pub fn children_with_keys(&self, parent: Entity) -> Vec<(Entity, OrderKey)> {
        let Ok(outgoing) = self.outgoing_q.get(parent) else {
            return Vec::new();
        };

        let mut children: Vec<(Entity, OrderKey)> = outgoing
            .iter()
            .filter_map(|edge| {
                self.tree_edge_q.get(edge).ok()?;
                let (_from, to) = self.edge_endpoints_q.get(edge).ok()?;
                let child = to.0;
                let key = self.order_key_q.get(child).ok()?.clone();
                Some((child, key))
            })
            .collect();

        children.sort_by(|(_, a), (_, b)| a.cmp(b));
        children
    }

    /// Find all root nodes (nodes with no tree parent).
    pub fn roots(&self) -> Vec<Entity> {
        self.graph
            .nodes_iter()
            .filter_map(|(entity, _, _, _)| {
                // Check if node has no parent
                let has_no_parent = self.tree_parent_q
                    .get(entity)
                    .ok()
                    .map(|tp| tp.0.is_none())
                    .unwrap_or(true);

                if has_no_parent {
                    Some(entity)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the depth of a node (distance from nearest root).
    ///
    /// Returns 0 for roots, 1 for their children, etc.
    pub fn depth(&self, mut node: Entity) -> usize {
        let mut depth = 0;
        while let Some(parent) = self.parent(node) {
            depth += 1;
            node = parent;
        }
        depth
    }

    /// Get the full path from this node to its root.
    ///
    /// Returns `[node, parent, grandparent, ..., root]`.
    pub fn path_to_root(&self, mut node: Entity) -> Vec<Entity> {
        let mut path = vec![node];
        while let Some(parent) = self.parent(node) {
            path.push(parent);
            node = parent;
        }
        path
    }

    /// Check if a node is a root (has no parent).
    pub fn is_root(&self, node: Entity) -> bool {
        self.parent(node).is_none()
    }

    /// Check if a node is a leaf (has no children).
    pub fn is_leaf(&self, node: Entity) -> bool {
        self.children(node).is_empty()
    }

    // ========================================================================
    // Statistics
    // ========================================================================

    /// Count total nodes in the scene.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Count root nodes.
    pub fn root_count(&self) -> usize {
        self.roots().len()
    }

    /// Get the maximum depth of the scene.
    pub fn max_depth(&self) -> usize {
        self.graph
            .nodes_iter()
            .map(|(entity, _, _, _)| self.depth(entity))
            .max()
            .unwrap_or(0)
    }

    // ========================================================================
    // Tree walks
    // ========================================================================

    /// Pre-order DFS over the subtree rooted at `root`, yielding
    /// `(entity, depth)` for each visited entity. `depth` is relative
    /// to `root` (i.e. `root` itself yields depth `0`).
    ///
    /// Children are visited in `OrderKey` order, so the traversal is
    /// deterministic across runs given the same tree.
    ///
    /// The walk is built eagerly into a `Vec` and returned as an
    /// iterator. Simpler and friendlier to the borrow checker than a
    /// streaming iterator that holds a mutable stack across `&self`
    /// borrows; for the realistic Figma-document scale it's not a
    /// hot path.
    pub fn walk_dfs_with_depth(&self, root: Entity) -> impl Iterator<Item = (Entity, usize)> {
        let mut out: Vec<(Entity, usize)> = Vec::new();
        let mut stack: Vec<(Entity, usize)> = vec![(root, 0)];
        while let Some((entity, depth)) = stack.pop() {
            out.push((entity, depth));
            // Push children in reverse so the next pop yields the first child.
            for child in self.children(entity).into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        out.into_iter()
    }
}

// ============================================================================
// Materialize — typed-graph traversal that yields the graph's enum
// ============================================================================

/// Adapter for "given a node entity, give me the typed-graph's owned
/// node form".
///
/// Each typed graph implements this on a `SystemParam` that knows how
/// to dispatch per-variant queries (e.g.
/// [`kyoso_figma::FigmaNodeQuery`](../../../kyoso_figma/node/struct.FigmaNodeQuery.html)).
/// Compose with [`SceneGraph::walk_dfs_with_depth`] for typed subtree
/// traversal, or with [`for_each_node`] for a flat marker-driven sweep.
pub trait Materialize<G: Graph> {
    /// Materialize the node at `entity` as the graph's owned enum
    /// (`G::Node`), or `None` if `entity` isn't a node of this graph.
    fn materialize_any(&self, entity: Entity) -> Option<G::Node>;
}

/// Edge counterpart to [`Materialize`]. Given an edge entity, produce
/// the typed graph's owned edge form (`G::Edge`).
///
/// For graphs without typed edges (the common case today, including
/// Figma), `G::Edge = ()` and this trait functions as a presence check
/// — `Some(())` if the entity is an edge of this graph, `None` otherwise.
/// For graphs with typed edges (e.g. an `EdgeCategory`-based design),
/// the impl dispatches per-edge-variant just like `Materialize` does
/// for nodes.
pub trait MaterializeEdge<G: Graph> {
    fn materialize_any_edge(&self, entity: Entity) -> Option<G::Edge>;
}

/// Iterate every entity matching `G::NodeMarker`, materialize through
/// `mat`, and invoke `f(entity, node)` for each successful conversion.
/// Skips entities whose materialization returns `None` (e.g. partially-
/// synced or component-inconsistent nodes).
pub fn for_each_node<G, M>(
    markers: &bevy::ecs::system::Query<Entity, bevy::ecs::query::With<G::NodeMarker>>,
    mat: &M,
    mut f: impl FnMut(Entity, G::Node),
) where
    G: Graph,
    M: Materialize<G>,
{
    for entity in markers.iter() {
        if let Some(node) = mat.materialize_any(entity) {
            f(entity, node);
        }
    }
}

/// Edge counterpart to [`for_each_node`]. Iterates every entity matching
/// `G::EdgeMarker`, materializes through `mat`, and invokes
/// `f(entity, edge)`.
pub fn for_each_edge<G, M>(
    markers: &bevy::ecs::system::Query<Entity, bevy::ecs::query::With<G::EdgeMarker>>,
    mat: &M,
    mut f: impl FnMut(Entity, G::Edge),
) where
    G: Graph,
    M: MaterializeEdge<G>,
{
    for entity in markers.iter() {
        if let Some(edge) = mat.materialize_any_edge(entity) {
            f(entity, edge);
        }
    }
}
