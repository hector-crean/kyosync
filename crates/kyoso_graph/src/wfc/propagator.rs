//! AC-3 arc-consistency propagator.
//!
//! When a node's domain shrinks (e.g. after collapse), this propagator
//! iterates neighbouring arcs and removes candidates that are no longer
//! supported by any remaining candidate on the other side.  The process
//! repeats until no further reductions occur or a contradiction is detected.

use std::collections::VecDeque;

use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::Directed;
use petgraph::visit::EdgeRef;

use crate::domain::{EdgeLike, NodeLike};
use crate::wfc::compatibility::CompatibilityTable;
use crate::wfc::domain_set::{Candidate, DomainSet};
use crate::wfc::error::WfcError;

/// Run AC-3 propagation starting from a set of *dirty* node indices.
///
/// `domains` is indexed by the *slot id* assigned by the WFC solver (which may
/// differ from `NodeIndex`). The caller provides mappings via
/// `slot_of_node` / `node_of_slot`.
///
/// Returns `Ok(())` if propagation reaches a fixed point without
/// contradictions, or `Err(WfcError::Contradiction)` if any domain becomes
/// empty.
pub fn propagate_ac3<N, E>(
    graph: &StableGraph<(), (), Directed>,
    domains: &mut Vec<DomainSet>,
    slot_of_node: &std::collections::HashMap<NodeIndex, usize>,
    _node_of_slot: &[NodeIndex],
    dirty: &[NodeIndex],
    catalog_nodes: &[N],
    catalog_edges: &[E],
    compat: &CompatibilityTable<N, E>,
) -> Result<(), WfcError>
where
    N: NodeLike,
    E: EdgeLike,
{
    let mut queue: VecDeque<(NodeIndex, NodeIndex)> = VecDeque::new();

    // Seed the work-list with all arcs touching dirty nodes (both directions).
    for &dirty_node in dirty {
        for edge in graph.edges(dirty_node) {
            let neighbour = edge.target();
            if neighbour != dirty_node {
                queue.push_back((neighbour, dirty_node));
            }
        }
        // Incoming edges too (undirected propagation).
        for edge in graph.edges_directed(dirty_node, petgraph::Direction::Incoming) {
            let neighbour = edge.source();
            if neighbour != dirty_node {
                queue.push_back((neighbour, dirty_node));
            }
        }
    }

    while let Some((from_node, support_node)) = queue.pop_front() {
        let Some(&from_slot) = slot_of_node.get(&from_node) else {
            continue;
        };
        let Some(&support_slot) = slot_of_node.get(&support_node) else {
            continue;
        };

        // For each candidate in `from_slot`, check if there exists at least
        // one supporting candidate in `support_slot` (i.e. one that is
        // compatible via some edge variant).
        let support_domain = domains[support_slot].clone();
        let changed = domains[from_slot].retain(|from_candidate| {
            has_support(
                from_candidate,
                &support_domain,
                catalog_nodes,
                catalog_edges,
                compat,
            )
        });

        if domains[from_slot].is_empty() {
            return Err(WfcError::Contradiction(from_node));
        }

        if changed {
            // `from_node`'s domain shrank -- add its neighbours back to the queue.
            for edge in graph.edges(from_node) {
                let n = edge.target();
                if n != support_node {
                    queue.push_back((n, from_node));
                }
            }
            for edge in graph.edges_directed(from_node, petgraph::Direction::Incoming) {
                let n = edge.source();
                if n != support_node {
                    queue.push_back((n, from_node));
                }
            }
        }
    }

    Ok(())
}

/// Check whether `from_candidate` has at least one compatible partner in
/// `support_domain`.
fn has_support<N: NodeLike, E: EdgeLike>(
    from_candidate: &Candidate,
    support_domain: &DomainSet,
    catalog_nodes: &[N],
    catalog_edges: &[E],
    compat: &CompatibilityTable<N, E>,
) -> bool {
    let Some(from_node) = catalog_nodes.get(from_candidate.node_idx) else {
        return false;
    };

    for support_candidate in support_domain.iter() {
        let Some(support_node) = catalog_nodes.get(support_candidate.node_idx) else {
            continue;
        };
        // The candidate's own edge variant must be compatible in the
        // from→support direction.
        if let Some(edge) = catalog_edges.get(from_candidate.edge_idx) {
            if compat.is_compatible(from_node, edge, support_node) {
                return true;
            }
        }
    }
    false
}
