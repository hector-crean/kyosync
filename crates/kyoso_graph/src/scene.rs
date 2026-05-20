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
}
