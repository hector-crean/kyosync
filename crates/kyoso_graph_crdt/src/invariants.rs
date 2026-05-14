//! Topology-shape invariants — universal post-conditions that should
//! hold on any well-behaved [`GraphTopology`], regardless of the
//! workload that built it.
//!
//! Used by:
//! - the chaos sim, to assert convergence + structural sanity on the
//!   canonical replica after each run;
//! - the topology-probe harness, to assert nothing breaks while
//!   building large reference graphs;
//! - the scenarios runner, to surface structural violations on each
//!   peer at every checkpoint.
//!
//! Invariants intentionally cover *structure only* (tree shape, edge
//! liveness, id uniqueness) — property-level invariants belong with
//! the per-schema test suites since they depend on user types.

use serde::{Deserialize, Serialize};

use crate::topology::GraphTopology;
use kyoso_crdt::CrdtId;

/// One structural violation. Two-line shape so reports stay grep-able.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantViolation {
    pub kind: ViolationKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    /// A node's `tree_parent` chain cycles back to itself.
    TreeCycle,
    /// An edge's `from` or `to` points to a tombstoned node (cascade
    /// tombstone failed).
    OrphanEdge,
    /// `would_create_cycle` says a parent reassignment is safe but the
    /// resulting walk loops anyway. Internal-consistency check.
    CycleCheckInconsistent,
}

/// Run every invariant against `topology`. Empty `Vec` means clean.
pub fn check_topology(topology: &GraphTopology) -> Vec<InvariantViolation> {
    let mut out = Vec::new();
    check_no_tree_cycles(topology, &mut out);
    check_no_orphan_edges(topology, &mut out);
    out
}

/// Walk `tree_parent` from every live node; flag any node whose walk
/// re-visits itself before reaching `None` (a root).
fn check_no_tree_cycles(topology: &GraphTopology, out: &mut Vec<InvariantViolation>) {
    // Bounded walk depth — if we exceed `node_count + 1` steps the
    // walk is in a cycle by pigeonhole. Avoids visiting-set bookkeeping
    // per node.
    let max_depth = topology.node_count() + 1;
    for start in topology.live_node_ids() {
        let mut cursor = topology.tree_parent(start);
        let mut steps = 0usize;
        while let Some(parent) = cursor {
            if parent == start {
                out.push(InvariantViolation {
                    kind: ViolationKind::TreeCycle,
                    detail: format!(
                        "live node {start:?} reachable from itself via tree_parent walk"
                    ),
                });
                break;
            }
            steps += 1;
            if steps > max_depth {
                out.push(InvariantViolation {
                    kind: ViolationKind::TreeCycle,
                    detail: format!(
                        "tree_parent walk from {start:?} exceeded {max_depth} steps without \
                         terminating — unreachable from root"
                    ),
                });
                break;
            }
            cursor = topology.tree_parent(parent);
        }
    }
}

/// Every live edge's endpoints must reference live nodes.
fn check_no_orphan_edges(topology: &GraphTopology, out: &mut Vec<InvariantViolation>) {
    for edge_id in topology.live_edge_ids() {
        let Some((from, to)) = topology.edge_endpoints(edge_id) else {
            continue;
        };
        if !topology.is_live_node(from) {
            out.push(InvariantViolation {
                kind: ViolationKind::OrphanEdge,
                detail: format!(
                    "live edge {edge_id:?} has tombstoned from-endpoint {from:?}"
                ),
            });
        }
        if !topology.is_live_node(to) {
            out.push(InvariantViolation {
                kind: ViolationKind::OrphanEdge,
                detail: format!(
                    "live edge {edge_id:?} has tombstoned to-endpoint {to:?}"
                ),
            });
        }
    }
}

/// Sanity-check `would_create_cycle` against the actual `tree_parent`
/// walk. For every live (target, proposed_parent) pair the function
/// claims is safe, verify the walk really doesn't loop. Bounded to a
/// random sample of size `sample` because the full N² cross is
/// expensive on big topologies.
///
/// Returns at most `sample` violations. Caller controls cost.
pub fn cross_check_cycle_detection(
    topology: &GraphTopology,
    sample: usize,
) -> Vec<InvariantViolation> {
    let nodes: Vec<CrdtId> = topology.live_node_ids().collect();
    if nodes.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut checks = 0;
    'outer: for (i, target) in nodes.iter().enumerate() {
        for (j, parent) in nodes.iter().enumerate() {
            if i == j {
                continue;
            }
            if topology.would_create_cycle(*target, *parent) {
                continue;
            }
            // The function says safe; verify the walk from `parent`
            // never reaches `target` (a cycle's marker).
            let mut cursor = Some(*parent);
            let mut steps = 0usize;
            while let Some(id) = cursor {
                if id == *target {
                    out.push(InvariantViolation {
                        kind: ViolationKind::CycleCheckInconsistent,
                        detail: format!(
                            "would_create_cycle({target:?}, {parent:?}) = false but \
                             tree_parent walk loops back to {target:?}"
                        ),
                    });
                    break;
                }
                steps += 1;
                if steps > nodes.len() + 1 {
                    break;
                }
                cursor = topology.tree_parent(id);
            }
            checks += 1;
            if checks >= sample {
                break 'outer;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphBackend;
    use kyoso_crdt::EmptySchema;

    #[test]
    fn empty_topology_is_clean() {
        let backend = GraphBackend::<EmptySchema>::with_peer(1);
        let v = check_topology(backend.backend().topology());
        assert!(v.is_empty(), "{v:?}");
    }

    // Building meaningful invariant-violation fixtures requires
    // op-stream manipulation that isn't worth the surface here; the
    // chaos sim covers the "did invariants ever fail" case end-to-end.
}
