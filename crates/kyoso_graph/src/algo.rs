//! High-level graph algorithms built on top of the traversal iterators.
//!
//! These are terminal consumers — they walk the graph and return owned
//! results (`Vec<Entity>`, `Option<Vec<Entity>>`, `bool`, ...). For lazy
//! traversal use the iterators in [`crate::traverse`] directly.
//!
//! All directional operations go through [`GraphTraverse`] +
//! [`Reverse`], so there's a single BFS implementation in the crate
//! (in `traverse.rs`) and these are just shaped consumers of it.

use bevy::{
    ecs::query::{QueryData, QueryFilter},
    prelude::*,
};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::queries::GraphQuery;
use crate::traverse::{GraphTraverse, Reverse};

impl<'w, 's, N, E, NF, EF> GraphQuery<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    // ========================================================================
    // Reachability & pathfinding
    // ========================================================================

    /// Whether `to` is reachable from `start` (directed reachability).
    pub fn is_reachable(&self, start: Entity, to: Entity) -> bool {
        self.bfs_iter(start).any(|n| n == to)
    }

    /// Breadth-first search from `start` to `goal`, returning the path if found.
    ///
    /// Reconstructs the path using the `parent` field of
    /// [`BfsIterWithDepth`](crate::traverse::BfsIterWithDepth), so this
    /// is a single pass over BFS plus a `HashMap` lookup at the end.
    pub fn bfs_path(&self, start: Entity, goal: Entity) -> Option<Vec<Entity>> {
        if start == goal {
            return Some(vec![start]);
        }
        let mut parents: HashMap<Entity, Entity> = HashMap::new();
        for node in self.bfs_iter_with_depth(start) {
            if let Some(p) = node.parent {
                parents.insert(node.entity, p);
            }
            if node.entity == goal {
                let mut path = vec![goal];
                let mut cur = goal;
                while let Some(&p) = parents.get(&cur) {
                    path.push(p);
                    if p == start {
                        break;
                    }
                    cur = p;
                }
                path.reverse();
                return Some(path);
            }
        }
        None
    }

    // ========================================================================
    // Connected components
    // ========================================================================

    /// Collect the connected component containing `start` (directed — follows
    /// outgoing edges only).
    pub fn connected_component(&self, start: Entity) -> Vec<Entity> {
        self.bfs_iter(start).collect()
    }

    /// Collect all nodes in the connected component containing `start` (undirected).
    ///
    /// "Undirected" means following both successors and predecessors. The
    /// [`GraphTraverse`] trait only exposes one direction at a time, so
    /// this still hand-rolls the union — but using `successors` and
    /// `predecessors` from the trait, not the lower-level edge queries.
    pub fn connected_component_undirected(&self, start: Entity) -> Vec<Entity> {
        let mut queue = VecDeque::new();
        let mut visited: HashSet<Entity> = HashSet::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(current) = queue.pop_front() {
            for n in self.successors(current).chain(self.predecessors(current)) {
                if visited.insert(n) {
                    queue.push_back(n);
                }
            }
        }
        visited.into_iter().collect()
    }

    /// Check if two nodes are in the same connected component (undirected).
    pub fn same_component(&self, a: Entity, b: Entity) -> bool {
        if a == b {
            return true;
        }
        self.connected_component_undirected(a).contains(&b)
    }

    // ========================================================================
    // Directional collection
    // ========================================================================

    /// Collect all upstream (predecessor-reachable) nodes, excluding `node` itself.
    pub fn upstream_nodes(&self, node: Entity) -> Vec<Entity> {
        Reverse(self)
            .bfs_iter(node)
            .filter(|&n| n != node)
            .collect()
    }

    /// Collect all downstream (successor-reachable) nodes, excluding `node` itself.
    pub fn downstream_nodes(&self, node: Entity) -> Vec<Entity> {
        self.bfs_iter(node).filter(|&n| n != node).collect()
    }

    // ========================================================================
    // Subgraph extraction
    // ========================================================================

    /// Get nodes in a BFS-limited subgraph (undirected) with optional depth limit.
    ///
    /// `depth = None` is equivalent to [`connected_component_undirected`].
    pub fn affected_subgraph(&self, start: Entity, depth: Option<usize>) -> Vec<Entity> {
        match depth {
            None => self.connected_component_undirected(start),
            Some(max_depth) => {
                let mut result = Vec::new();
                let mut queue = VecDeque::new();
                let mut visited = HashSet::new();
                queue.push_back((start, 0));
                visited.insert(start);
                while let Some((current, d)) = queue.pop_front() {
                    result.push(current);
                    if d < max_depth {
                        for n in self.successors(current).chain(self.predecessors(current)) {
                            if visited.insert(n) {
                                queue.push_back((n, d + 1));
                            }
                        }
                    }
                }
                result
            }
        }
    }

    /// Batch collect neighbors for multiple nodes.
    pub fn collect_neighbors_batch(
        &self,
        nodes: impl Iterator<Item = Entity>,
    ) -> HashMap<Entity, Vec<Entity>> {
        nodes
            .map(|node| (node, self.affected_neighbors(node)))
            .collect()
    }

    // ========================================================================
    // Pattern matching
    // ========================================================================

    /// Find all directed paths of exactly `path_len` nodes starting at
    /// `start` where each position `i` matches `pred(i, node)`.
    ///
    /// Position `0` is matched against `start` itself; subsequent
    /// positions are matched against successors. Edges are followed in
    /// the forward direction only; for reverse traversal wrap with
    /// [`Reverse`](crate::traverse::Reverse).
    ///
    /// A single closure with an index argument lets the caller match
    /// distinct shapes at each position without juggling slices of
    /// trait objects:
    ///
    /// ```ignore
    /// // Find all `Frame -> Text -> Rectangle` triples reachable from `root`.
    /// let triples = q.find_paths_matching(root, 3, |i, e| match i {
    ///     0 => has::<Frame>(e),
    ///     1 => has::<Text>(e),
    ///     2 => has::<Rectangle>(e),
    ///     _ => unreachable!(),
    /// });
    /// ```
    ///
    /// For edge-typed filtering, use [`GraphTraverseEdges::edge_bfs_iter`]
    /// directly — this method matches nodes only.
    pub fn find_paths_matching<F>(
        &self,
        start: Entity,
        path_len: usize,
        pred: F,
    ) -> Vec<Vec<Entity>>
    where
        F: Fn(usize, Entity) -> bool,
    {
        let mut out = Vec::new();
        if path_len == 0 || !pred(0, start) {
            return out;
        }
        let mut current = vec![start];
        self.find_paths_recurse(&mut current, path_len, &pred, &mut out);
        out
    }

    fn find_paths_recurse<F>(
        &self,
        current: &mut Vec<Entity>,
        path_len: usize,
        pred: &F,
        out: &mut Vec<Vec<Entity>>,
    ) where
        F: Fn(usize, Entity) -> bool,
    {
        if current.len() == path_len {
            out.push(current.clone());
            return;
        }
        let last = *current.last().unwrap();
        let next_idx = current.len();
        for n in self.successors(last) {
            // Avoid cycles within a single path by skipping nodes already on it.
            if current.contains(&n) {
                continue;
            }
            if pred(next_idx, n) {
                current.push(n);
                self.find_paths_recurse(current, path_len, pred, out);
                current.pop();
            }
        }
    }
}
