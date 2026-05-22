//! Graph query SystemParam and one-hop navigation primitives.
//!
//! [`GraphQuery`] is the single `SystemParam` your systems claim to walk
//! the ECS-backed graph. It exposes:
//! - Schema queries (`nodes_q`, `edges_q`, ...)
//! - One-hop neighbor / predecessor / edge lookups
//! - Membership and degree queries
//! - Component-data extraction (`get_node`, `get_edge`, ...)
//!
//! Multi-step traversal iterators live in [`crate::traverse`];
//! derived algorithms (paths, components, reachability) live in
//! [`crate::algo`].

use bevy::{
    ecs::{
        query::{QueryData, QueryFilter},
        system::SystemParam,
    },
    prelude::*,
};
use std::collections::HashSet;

use super::components::{EdgeFrom, EdgeTo, IncomingEdges, OutgoingEdges};

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
    pub fn edges_iter(
        &self,
    ) -> impl Iterator<
        Item = (
            Entity,
            EdgeFrom,
            EdgeTo,
            <E::ReadOnly as QueryData>::Item<'_, '_>,
        ),
    > + '_ {
        self.edges_q
            .iter()
            .map(|(e, from, to, edge)| (e, *from, *to, edge))
    }

    /// Iterate all node entities.
    pub fn nodes_iter(
        &self,
    ) -> impl Iterator<
        Item = (
            Entity,
            Option<&OutgoingEdges>,
            Option<&IncomingEdges>,
            <N::ReadOnly as QueryData>::Item<'_, '_>,
        ),
    > + '_ {
        self.nodes_q
            .iter()
            .map(|(e, outgoing, incoming, node)| (e, outgoing, incoming, node))
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
    pub fn neighbors_with_data(
        &self,
        node: Entity,
    ) -> impl Iterator<Item = (Entity, <N::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.neighbors(node)
            .filter_map(|neighbor| self.get_node(neighbor).map(|data| (neighbor, data)))
    }

    /// Iterate edges with full data (edge entity, from, to, edge component).
    pub fn edges_with_data(
        &self,
    ) -> impl Iterator<
        Item = (
            Entity,
            Entity,
            Entity,
            <E::ReadOnly as QueryData>::Item<'_, '_>,
        ),
    > + '_ {
        self.edges_q
            .iter()
            .map(|(e, from, to, edge)| (e, from.0, to.0, edge))
    }

    /// Iterate outgoing edges from a node with their data and target nodes.
    pub fn outgoing_edges_with_data(
        &self,
        node: Entity,
    ) -> impl Iterator<Item = (Entity, Entity, <E::ReadOnly as QueryData>::Item<'_, '_>)> + '_ {
        self.outgoing_edges(node).filter_map(|edge_entity| {
            self.edges_q
                .get(edge_entity)
                .ok()
                .map(|(_, _, to, edge)| (edge_entity, to.0, edge))
        })
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
