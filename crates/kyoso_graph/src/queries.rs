//! Graph query utilities for the unified graph model.
//!
//! This module provides:
//! - `GraphQuery` system param for graph traversal
//! - Query data types for nodes and edges
//! - Graph algorithms (BFS, DFS, connected components, etc.)

use bevy::{
    ecs::{query::{QueryData, QueryFilter}, system::SystemParam},
    prelude::*,
};
use std::collections::{HashMap, HashSet, VecDeque};

use super::components::{
    EdgeFrom, EdgeTo, IncomingEdges, OutgoingEdges,
};

// ============================================================================
// QUERY DATA TYPES
// ============================================================================

/// Query data for edge entities.
#[derive(QueryData)]
pub struct EdgeQueryData<Edge: Component> {
    pub entity: Entity,
    pub edge_from: &'static EdgeFrom,
    pub edge_to: &'static EdgeTo,
    pub edge: &'static Edge,
}

/// Query data for node entities.
#[derive(QueryData)]
pub struct NodeQueryData<Node: Component> {
    pub entity: Entity,
    pub node: &'static Node,
    pub incoming_edges: &'static IncomingEdges,
    pub outgoing_edges: &'static OutgoingEdges,
}

/// Query data for detecting changes in node connectivity.
#[derive(QueryData)]
pub struct NodeDeltaQueryData {
    pub entity: Entity,
    pub incoming_edges: &'static IncomingEdges,
    pub outgoing_edges: &'static OutgoingEdges,
}

// ============================================================================
// GRAPH QUERY SYSTEM PARAM
// ============================================================================

/// System parameter for graph queries and traversal.
///
/// Provides methods for navigating the graph, finding neighbors,
/// computing connected components, and other graph algorithms.
///
/// Generic parameters:
/// - `N`: Node query data (e.g., `&MyNode`, `(&Transform, &Size)`)
/// - `E`: Edge query data (e.g., `&MyEdge`, `&EdgeWeight`)
/// - `NF`: Node filter (e.g., `With<Selected>`, `Changed<Position>`), defaults to `()`
/// - `EF`: Edge filter (e.g., `With<Active>`), defaults to `()`
#[derive(SystemParam)]
pub struct GraphQuery<'w, 's, N, E, NF = (), EF = ()>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    pub edges_from: Query<'w, 's, &'static EdgeFrom>,
    pub edges_to: Query<'w, 's, &'static EdgeTo>,
    pub outgoing_index: Query<'w, 's, &'static OutgoingEdges>,
    pub incoming_index: Query<'w, 's, &'static IncomingEdges>,
    /// All edges matching the edge query + filter
    pub edges_q: Query<'w, 's, (Entity, &'static EdgeFrom, &'static EdgeTo, E), EF>,
    /// All nodes matching the node query + filter
    pub nodes_q: Query<
        'w,
        's,
        (
            Entity,
            Option<&'static OutgoingEdges>,
            Option<&'static IncomingEdges>,
            N,
        ),
        NF,
    >,
}

impl<'w, 's, N, E, NF, EF> GraphQuery<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    // ========================================================================
    // Basic iteration
    // ========================================================================

    /// Iterate all edge entities.
    pub fn edges_iter(&self) -> impl Iterator<Item = (Entity, EdgeFrom, EdgeTo, <E::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.edges_q.iter().map(|(e, from, to, edge)| (e, *from, *to, edge))
    }

    /// Iterate all node entities.
    pub fn nodes_iter(
        &self,
    ) -> impl Iterator<Item = (Entity, Option<&OutgoingEdges>, Option<&IncomingEdges>, <N::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.nodes_q.iter().map(|(e, outgoing, incoming, node)| (e, outgoing, incoming, node))
    }

    // ========================================================================
    // Edge queries
    // ========================================================================

    /// Iterate outgoing edge entities from a node.
    pub fn outgoing_edges(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.outgoing_index
            .relationship_sources::<OutgoingEdges>(node)
    }

    /// Iterate incoming edge entities to a node.
    pub fn incoming_edges(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.incoming_index
            .relationship_sources::<IncomingEdges>(node)
    }

    /// Get all edges connected to a node (both incoming and outgoing).
    pub fn connected_edges(&self, node: Entity) -> Vec<Entity> {
        let mut edges = Vec::new();
        edges.extend(self.outgoing_edges(node));
        edges.extend(self.incoming_edges(node));
        edges
    }

    /// Find the edge entity connecting `from -> to`, if present.
    pub fn find_edge(&self, from: Entity, to: Entity) -> Option<Entity> {
        self.outgoing_edges(from)
            .find(|&edge| self.edges_to.get(edge).ok().map(|et| et.0) == Some(to))
    }

    /// Out-degree (number of outgoing edges).
    pub fn out_degree(&self, node: Entity) -> usize {
        self.outgoing_edges(node).count()
    }

    /// In-degree (number of incoming edges).
    pub fn in_degree(&self, node: Entity) -> usize {
        self.incoming_edges(node).count()
    }

    // ========================================================================
    // Neighbor queries
    // ========================================================================

    /// Iterate neighbor node entities reachable via outgoing edges.
    pub fn neighbors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.outgoing_edges(node)
            .filter_map(|edge| self.edges_to.get(edge).ok().map(|edge_to| edge_to.0))
    }

    /// Iterate predecessor node entities (via incoming edges).
    pub fn predecessors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.incoming_edges(node)
            .filter_map(|edge| self.edges_from.get(edge).ok().map(|edge_from| edge_from.0))
    }

    /// Return neighbors treating the graph as undirected (successors ∪ predecessors).
    pub fn undirected_neighbors(&self, node: Entity) -> Vec<Entity> {
        let mut set: HashSet<Entity> = HashSet::new();
        for n in self.neighbors(node) {
            set.insert(n);
        }
        for p in self.predecessors(node) {
            set.insert(p);
        }
        set.into_iter().collect()
    }

    /// Iterate (edge, neighbor) pairs for outgoing edges.
    pub fn neighbors_with_edges(
        &self,
        node: Entity,
    ) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        self.outgoing_edges(node).filter_map(|edge| {
            self.edges_to
                .get(edge)
                .ok()
                .map(|edge_to| (edge, edge_to.0))
        })
    }

    /// Get all neighbors that should receive updates (undirected neighbors).
    pub fn affected_neighbors(&self, node: Entity) -> Vec<Entity> {
        self.undirected_neighbors(node)
    }

    // ========================================================================
    // Counting & membership
    // ========================================================================

    /// Total number of node entities in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes_q.iter().count()
    }

    /// Total number of edge entities in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges_q.iter().count()
    }

    /// Whether the graph has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes_q.is_empty()
    }

    /// Check if an entity is a node in the graph.
    pub fn has_node(&self, entity: Entity) -> bool {
        self.nodes_q.get(entity).is_ok()
    }

    /// Check if an entity is an edge in the graph.
    pub fn has_edge(&self, entity: Entity) -> bool {
        self.edges_q.get(entity).is_ok()
    }

    /// Total degree of a node (in-degree + out-degree).
    pub fn degree(&self, node: Entity) -> usize {
        self.out_degree(node) + self.in_degree(node)
    }

    // ========================================================================
    // Graph algorithms
    // ========================================================================

    /// Whether `to` is reachable from `start` (directed reachability).
    pub fn is_reachable(&self, start: Entity, to: Entity) -> bool {
        if start == to {
            return true;
        }
        self.bfs_path(start, to).is_some()
    }

    /// Breadth-first search from `start` to `goal`, returning the path if found.
    pub fn bfs_path(&self, start: Entity, goal: Entity) -> Option<Vec<Entity>> {
        if start == goal {
            return Some(vec![start]);
        }
        let mut queue = VecDeque::new();
        let mut visited: HashSet<Entity> = HashSet::new();
        let mut parent: HashMap<Entity, Entity> = HashMap::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(current) = queue.pop_front() {
            for neighbor in self.neighbors(current) {
                if !visited.contains(&neighbor) {
                    visited.insert(neighbor);
                    parent.insert(neighbor, current);
                    if neighbor == goal {
                        let mut path = vec![goal];
                        let mut node = goal;
                        while let Some(&p) = parent.get(&node) {
                            path.push(p);
                            if p == start {
                                break;
                            }
                            node = p;
                        }
                        path.reverse();
                        return Some(path);
                    }
                    queue.push_back(neighbor);
                }
            }
        }
        None
    }

    /// Collect the connected component containing `start` using BFS over outgoing edges.
    pub fn connected_component(&self, start: Entity) -> Vec<Entity> {
        let mut queue = VecDeque::new();
        let mut visited: HashSet<Entity> = HashSet::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(current) = queue.pop_front() {
            for neighbor in self.neighbors(current) {
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
        }
        visited.into_iter().collect()
    }

    /// Collect all nodes in the connected component containing `start` (undirected).
    pub fn connected_component_undirected(&self, start: Entity) -> Vec<Entity> {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(start);
        visited.insert(start);

        while let Some(current) = queue.pop_front() {
            for neighbor in self.undirected_neighbors(current) {
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
        }
        visited.into_iter().collect()
    }

    /// Get nodes in a BFS-limited subgraph with optional depth limit.
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
                    if d > max_depth {
                        continue;
                    }
                    result.push(current);
                    if d < max_depth {
                        for neighbor in self.undirected_neighbors(current) {
                            if visited.insert(neighbor) {
                                queue.push_back((neighbor, d + 1));
                            }
                        }
                    }
                }
                result
            }
        }
    }

    /// Collect all upstream (predecessor) nodes.
    pub fn upstream_nodes(&self, node: Entity) -> Vec<Entity> {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(node);
        visited.insert(node);

        while let Some(current) = queue.pop_front() {
            for predecessor in self.predecessors(current) {
                if visited.insert(predecessor) {
                    queue.push_back(predecessor);
                }
            }
        }
        visited.into_iter().filter(|&n| n != node).collect()
    }

    /// Collect all downstream (successor) nodes.
    pub fn downstream_nodes(&self, node: Entity) -> Vec<Entity> {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(node);
        visited.insert(node);

        while let Some(current) = queue.pop_front() {
            for neighbor in self.neighbors(current) {
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
        }
        visited.into_iter().filter(|&n| n != node).collect()
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

    /// Check if two nodes are in the same connected component (undirected).
    pub fn same_component(&self, a: Entity, b: Entity) -> bool {
        if a == b {
            return true;
        }
        let component = self.connected_component_undirected(a);
        component.contains(&b)
    }

    /// Iterator over nodes in BFS order (entities only).
    pub fn bfs_iter(&self, start: Entity) -> BfsIter<'_, N, E, NF, EF> {
        BfsIter::new(self, start)
    }

    /// Iterator over nodes in DFS order (entities only).
    pub fn dfs_iter(&self, start: Entity) -> DfsIter<'_, N, E, NF, EF> {
        DfsIter::new(self, start)
    }

    /// Iterator over nodes in BFS order with depth and parent tracking.
    pub fn bfs_iter_with_depth(&self, start: Entity) -> BfsIterWithDepth<'_, N, E, NF, EF> {
        BfsIterWithDepth::new(self, start)
    }

    /// Iterator over nodes in DFS order with depth and parent tracking.
    pub fn dfs_iter_with_depth(&self, start: Entity) -> DfsIterWithDepth<'_, N, E, NF, EF> {
        DfsIterWithDepth::new(self, start)
    }

    // ========================================================================
    // Component extraction
    // ========================================================================

    /// Get the node component data for an entity, if it matches the query.
    pub fn get_node(&self, entity: Entity) -> Option<<N::ReadOnly as QueryData>::Item<'_, '_>> {
        self.nodes_q.get(entity).ok().map(|(_, _, _, node)| node)
    }

    /// Get the edge component data for an entity, if it matches the query.
    pub fn get_edge(&self, entity: Entity) -> Option<<E::ReadOnly as QueryData>::Item<'_, '_>> {
        self.edges_q.get(entity).ok().map(|(_, _, _, edge)| edge)
    }

    /// Iterate neighbors with their component data.
    pub fn neighbors_with_data(&self, node: Entity) -> impl Iterator<Item = (Entity, <N::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.neighbors(node)
            .filter_map(|neighbor| {
                self.get_node(neighbor).map(|data| (neighbor, data))
            })
    }

    /// Iterate edges with full data (edge entity, from, to, edge component).
    pub fn edges_with_data(&self) -> impl Iterator<Item = (Entity, Entity, Entity, <E::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.edges_q.iter().map(|(e, from, to, edge)| (e, from.0, to.0, edge))
    }

    /// Iterate outgoing edges from a node with their data and target nodes.
    pub fn outgoing_edges_with_data(
        &self,
        node: Entity,
    ) -> impl Iterator<Item = (Entity, Entity, <E::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.outgoing_edges(node).filter_map(|edge_entity| {
            self.edges_q.get(edge_entity).ok().map(|(_, _, to, edge)| {
                (edge_entity, to.0, edge)
            })
        })
    }
}

// ============================================================================
// ITERATOR TYPES
// ============================================================================

pub struct BfsIter<'a, N: QueryData + 'static, E: QueryData + 'static, NF: QueryFilter + 'static, EF: QueryFilter + 'static> {
    graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>,
    queue: VecDeque<Entity>,
    visited: HashSet<Entity>,
}

impl<'a, N, E, NF, EF> BfsIter<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn new(graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>, start: Entity) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(start);
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
        }
    }
}

impl<'a, N, E, NF, EF> Iterator for BfsIter<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    type Item = Entity;
    fn next(&mut self) -> Option<Self::Item> {
        let current = self.queue.pop_front()?;
        for neighbor in self.graph.neighbors(current) {
            if self.visited.insert(neighbor) {
                self.queue.push_back(neighbor);
            }
        }
        Some(current)
    }
}

pub struct DfsIter<'a, N: QueryData + 'static, E: QueryData + 'static, NF: QueryFilter + 'static, EF: QueryFilter + 'static> {
    graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>,
    stack: Vec<Entity>,
    visited: HashSet<Entity>,
}

impl<'a, N, E, NF, EF> DfsIter<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn new(graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>, start: Entity) -> Self {
        Self {
            graph,
            stack: vec![start],
            visited: HashSet::new(),
        }
    }
}

impl<'a, N, E, NF, EF> Iterator for DfsIter<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    type Item = Entity;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(current) = self.stack.pop() {
            if self.visited.insert(current) {
                for neighbor in self.graph.neighbors(current) {
                    self.stack.push(neighbor);
                }
                return Some(current);
            }
        }
        None
    }
}

// ============================================================================
// TRAVERSAL NODE (with hierarchical metadata)
// ============================================================================

/// Traversal result that includes hierarchical metadata.
#[derive(Clone, Debug)]
pub struct TraversalNode {
    /// The entity being visited.
    pub entity: Entity,
    /// Depth from the start node (0 = start node).
    pub depth: usize,
    /// Parent entity in the traversal tree (None for start node).
    pub parent: Option<Entity>,
}

// ============================================================================
// BFS WITH DEPTH ITERATOR
// ============================================================================

/// BFS iterator that tracks depth and parent for each visited node.
pub struct BfsIterWithDepth<'a, N: QueryData + 'static, E: QueryData + 'static, NF: QueryFilter + 'static, EF: QueryFilter + 'static> {
    graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>,
    queue: VecDeque<(Entity, usize, Option<Entity>)>,
    visited: HashSet<Entity>,
}

impl<'a, N, E, NF, EF> BfsIterWithDepth<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn new(graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>, start: Entity) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back((start, 0, None));
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
        }
    }
}

impl<'a, N, E, NF, EF> Iterator for BfsIterWithDepth<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    type Item = TraversalNode;

    fn next(&mut self) -> Option<Self::Item> {
        let (current, depth, parent) = self.queue.pop_front()?;

        for neighbor in self.graph.neighbors(current) {
            if self.visited.insert(neighbor) {
                self.queue.push_back((neighbor, depth + 1, Some(current)));
            }
        }

        Some(TraversalNode {
            entity: current,
            depth,
            parent,
        })
    }
}

// ============================================================================
// DFS WITH DEPTH ITERATOR
// ============================================================================

/// DFS iterator that tracks depth and parent for each visited node.
pub struct DfsIterWithDepth<'a, N: QueryData + 'static, E: QueryData + 'static, NF: QueryFilter + 'static, EF: QueryFilter + 'static> {
    graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>,
    stack: Vec<(Entity, usize, Option<Entity>)>,
    visited: HashSet<Entity>,
}

impl<'a, N, E, NF, EF> DfsIterWithDepth<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn new(graph: &'a GraphQuery<'a, 'a, N, E, NF, EF>, start: Entity) -> Self {
        Self {
            graph,
            stack: vec![(start, 0, None)],
            visited: HashSet::new(),
        }
    }
}

impl<'a, N, E, NF, EF> Iterator for DfsIterWithDepth<'a, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    type Item = TraversalNode;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((current, depth, parent)) = self.stack.pop() {
            if self.visited.insert(current) {
                for neighbor in self.graph.neighbors(current) {
                    self.stack.push((neighbor, depth + 1, Some(current)));
                }

                return Some(TraversalNode {
                    entity: current,
                    depth,
                    parent,
                });
            }
        }
        None
    }
}

// ============================================================================
// QUERY EXTENSION TRAIT
// ============================================================================

/// Query helpers for graph navigation (tuple-based).
pub trait GraphQueryExt<'w, 's> {
    fn outgoing_edges_of(&self, node: Entity) -> Vec<Entity>;
    fn incoming_edges_of(&self, node: Entity) -> Vec<Entity>;
    fn neighbors_of(&self, node: Entity) -> Vec<Entity>;
}

impl<'w, 's> GraphQueryExt<'w, 's>
    for (
        Query<'w, 's, &'static OutgoingEdges>,
        Query<'w, 's, &'static EdgeTo>,
        Query<'w, 's, &'static IncomingEdges>,
    )
{
    fn outgoing_edges_of(&self, node: Entity) -> Vec<Entity> {
        let (outgoing, _to, _incoming) = self;
        outgoing
            .relationship_sources::<OutgoingEdges>(node)
            .collect()
    }

    fn incoming_edges_of(&self, node: Entity) -> Vec<Entity> {
        let (_outgoing, _to, incoming) = self;
        incoming
            .relationship_sources::<IncomingEdges>(node)
            .collect()
    }

    fn neighbors_of(&self, node: Entity) -> Vec<Entity> {
        let (outgoing, to, _incoming) = self;
        outgoing
            .relationship_sources::<OutgoingEdges>(node)
            .filter_map(|edge| to.get(edge).ok().map(|edge_to| edge_to.0))
            .collect()
    }
}

#[cfg(test)]
#[path = "queries_tests.rs"]
mod queries_tests;
