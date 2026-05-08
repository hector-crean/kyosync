//! Subgraph isomorphism matcher.
//!
//! Given a [`Pattern`] and a host graph (the petgraph mirror + per-entity
//! symbol/order lookup functions), finds all embeddings of the pattern in the
//! host graph.
//!
//! The implementation is a backtracking search suitable for small patterns
//! (typical functional groups have 2-6 nodes).

use std::collections::HashMap;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::visit::EdgeRef;
use petgraph::Directed;

use super::pattern::Pattern;

/// A single embedding: maps each pattern node index to a host graph
/// `NodeIndex`.
pub type Embedding = HashMap<NodeIndex, NodeIndex>;

/// Find all subgraph isomorphisms of `pattern` in `host`.
///
/// `node_symbol` returns the domain symbol for a host node (e.g. "C", "O").
/// `edge_order` returns the bond order for a host edge.
pub fn find_embeddings(
    pattern: &Pattern,
    host: &StableGraph<(), (), Directed>,
    node_symbol: &dyn Fn(NodeIndex) -> String,
    edge_order: &dyn Fn(NodeIndex, NodeIndex) -> Option<u8>,
) -> Vec<Embedding> {
    let pattern_nodes: Vec<NodeIndex> = pattern.graph.node_indices().collect();
    let host_nodes: Vec<NodeIndex> = host.node_indices().collect();

    let mut results = Vec::new();
    let mut mapping: Embedding = HashMap::new();
    let mut used: std::collections::HashSet<NodeIndex> = std::collections::HashSet::new();

    backtrack(
        &pattern_nodes,
        0,
        pattern,
        host,
        node_symbol,
        edge_order,
        &host_nodes,
        &mut mapping,
        &mut used,
        &mut results,
    );

    results
}

fn backtrack(
    pattern_nodes: &[NodeIndex],
    depth: usize,
    pattern: &Pattern,
    host: &StableGraph<(), (), Directed>,
    node_symbol: &dyn Fn(NodeIndex) -> String,
    edge_order: &dyn Fn(NodeIndex, NodeIndex) -> Option<u8>,
    host_nodes: &[NodeIndex],
    mapping: &mut Embedding,
    used: &mut std::collections::HashSet<NodeIndex>,
    results: &mut Vec<Embedding>,
) {
    if depth == pattern_nodes.len() {
        results.push(mapping.clone());
        return;
    }

    let p_node = pattern_nodes[depth];
    let p_data = &pattern.graph[p_node];

    for &h_node in host_nodes {
        if used.contains(&h_node) {
            continue;
        }

        let h_sym = node_symbol(h_node);
        if !p_data.matches(&h_sym) {
            continue;
        }

        // Check that all pattern edges from already-mapped nodes to p_node
        // (and vice versa) are present in the host with matching predicates.
        if !edges_consistent(
            pattern,
            p_node,
            h_node,
            mapping,
            host,
            node_symbol,
            edge_order,
        ) {
            continue;
        }

        mapping.insert(p_node, h_node);
        used.insert(h_node);

        backtrack(
            pattern_nodes,
            depth + 1,
            pattern,
            host,
            node_symbol,
            edge_order,
            host_nodes,
            mapping,
            used,
            results,
        );

        mapping.remove(&p_node);
        used.remove(&h_node);
    }
}

/// Verify that all pattern edges involving `p_node` and already-mapped
/// pattern neighbours are satisfied by the host graph.
fn edges_consistent(
    pattern: &Pattern,
    p_node: NodeIndex,
    h_node: NodeIndex,
    mapping: &Embedding,
    _host: &StableGraph<(), (), Directed>,
    node_symbol: &dyn Fn(NodeIndex) -> String,
    edge_order: &dyn Fn(NodeIndex, NodeIndex) -> Option<u8>,
) -> bool {
    // Outgoing edges from p_node.
    for edge in pattern.graph.edges(p_node) {
        let p_target = edge.target();
        if let Some(&h_target) = mapping.get(&p_target) {
            // The edge p_node -> p_target must map to h_node -> h_target.
            let order = match edge_order(h_node, h_target) {
                Some(o) => o,
                None => return false,
            };
            let from_sym = node_symbol(h_node);
            let to_sym = node_symbol(h_target);
            if !edge.weight().matches(order, &from_sym, &to_sym) {
                return false;
            }
        }
    }

    // Incoming edges to p_node.
    for edge in pattern
        .graph
        .edges_directed(p_node, petgraph::Direction::Incoming)
    {
        let p_source = edge.source();
        if let Some(&h_source) = mapping.get(&p_source) {
            let order = match edge_order(h_source, h_node) {
                Some(o) => o,
                None => return false,
            };
            let from_sym = node_symbol(h_source);
            let to_sym = node_symbol(h_node);
            if !edge.weight().matches(order, &from_sym, &to_sym) {
                return false;
            }
        }
    }

    true
}
