//! Graph topology implementation.
//!
//! Implements the [`Topology`] trait for the graph domain, handling:
//! - Nodes in a tree hierarchy (tree_parent + order_key)
//! - Reference edges (typed relationships between nodes)
//! - Move operations with cycle detection
//!
//! Property mutations (SetNodeProperty, SetRefEdgeProperty) are routed
//! to the schema layer and don't appear in structural operations.

use std::collections::HashMap;

use kyoso_crdt::context::CausalContext;
use kyoso_crdt::id::CrdtId;
use kyoso_crdt::topology::{PropertyOp, Topology};

use crate::edge_category::EdgeCategory;
use crate::op::OpKind;

/// Structural topology for graphs: nodes in tree + reference edges.
#[derive(Clone, Debug, Default)]
pub struct GraphTopology {
    /// Per-node structural state (tree position, tombstone).
    nodes: HashMap<CrdtId, NodeStructure>,
    /// Per-edge structural state (endpoints, category, tombstone).
    edges: HashMap<CrdtId, EdgeStructure>,
    /// Map `op_id → target node id` for in-flight `Move` ops.
    ///
    /// Used by `is_pending_move_target` to let detection systems suppress
    /// re-emitting a Move while one is already in flight. This lives in
    /// topology (not backend) because the Move logic needs it for cycle
    /// detection context.
    pending_moves: HashMap<CrdtId, CrdtId>,
}

/// Structural metadata for a single node.
#[derive(Clone, Debug)]
struct NodeStructure {
    tombstoned: bool,
    order_key: Option<String>,
    tree_parent: Option<CrdtId>,
}

/// Structural metadata for a single edge.
#[derive(Clone, Debug)]
struct EdgeStructure {
    from: CrdtId,
    to: CrdtId,
    category: EdgeCategory,
    tombstoned: bool,
}

/// Snapshot format for graph topology.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSnap>,
    pub edges: Vec<EdgeSnap>,
}

/// Per-node snapshot entry.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NodeSnap {
    pub id: CrdtId,
    pub order_key: Option<String>,
    pub tree_parent: Option<CrdtId>,
}

/// Per-edge snapshot entry.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EdgeSnap {
    pub id: CrdtId,
    pub from: CrdtId,
    pub to: CrdtId,
    pub category: EdgeCategory,
}

impl GraphTopology {
    /// True iff there's at least one locally-issued `Move` op queued or
    /// in flight for `target`.
    #[must_use]
    pub fn is_pending_move_target(&self, target: CrdtId) -> bool {
        self.pending_moves.values().any(|t| *t == target)
    }

    /// Track a Move op as in-flight. Used by domain methods.
    pub fn track_pending_move(&mut self, op_id: CrdtId, target: CrdtId) {
        self.pending_moves.insert(op_id, target);
    }

    /// Read a node's current tree parent (`None` for root or unknown).
    pub fn tree_parent(&self, id: CrdtId) -> Option<CrdtId> {
        let rec = self.nodes.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        rec.tree_parent
    }

    /// Read a live node's order key.
    pub fn node_order_key(&self, id: CrdtId) -> Option<&str> {
        let rec = self.nodes.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        rec.order_key.as_deref()
    }

    /// Check if a node exists and is live (not tombstoned).
    pub fn is_live_node(&self, id: CrdtId) -> bool {
        self.nodes
            .get(&id)
            .map(|rec| !rec.tombstoned)
            .unwrap_or(false)
    }

    /// Read the from/to endpoints of a live edge.
    pub fn edge_endpoints(&self, id: CrdtId) -> Option<(CrdtId, CrdtId)> {
        let rec = self.edges.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        Some((rec.from, rec.to))
    }

    /// Read a live edge's category.
    pub fn edge_category(&self, id: CrdtId) -> Option<&EdgeCategory> {
        let rec = self.edges.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        Some(&rec.category)
    }

    /// Count of live (non-tombstoned) nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.values().filter(|r| !r.tombstoned).count()
    }

    /// Count of live (non-tombstoned) edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.values().filter(|r| !r.tombstoned).count()
    }

    /// Iterate live edges with `from == n`.
    pub fn outgoing_edge_ids(&self, n: CrdtId) -> impl Iterator<Item = CrdtId> + '_ {
        self.edges.iter().filter_map(move |(id, rec)| {
            if rec.tombstoned || rec.from != n {
                None
            } else {
                Some(*id)
            }
        })
    }

    /// Iterate live edges with `to == n`.
    pub fn incoming_edge_ids(&self, n: CrdtId) -> impl Iterator<Item = CrdtId> + '_ {
        self.edges.iter().filter_map(move |(id, rec)| {
            if rec.tombstoned || rec.to != n {
                None
            } else {
                Some(*id)
            }
        })
    }

    /// True iff making `proposed_parent` the new parent of `target`
    /// would form a cycle.
    pub fn would_create_cycle(&self, target: CrdtId, proposed_parent: CrdtId) -> bool {
        if target == proposed_parent {
            return true;
        }
        let mut cursor = Some(proposed_parent);
        while let Some(id) = cursor {
            if id == target {
                return true;
            }
            cursor = self.nodes.get(&id).and_then(|rec| rec.tree_parent);
        }
        false
    }
}

impl Topology for GraphTopology {
    type OpKind = OpKind;
    type SnapshotState = GraphSnapshot;

    fn apply_structural_op(&mut self, op: &Self::OpKind, ctx: &CausalContext) {
        match op {
            OpKind::AddNode => {
                self.nodes.entry(ctx.op_id).or_insert(NodeStructure {
                    tombstoned: false,
                    order_key: None,
                    tree_parent: None,
                });
            }
            OpKind::RemoveNode { target } => {
                if let Some(rec) = self.nodes.get_mut(target) {
                    rec.tombstoned = true;
                    // Cascade-tombstone incident edges.
                    for edge in self.edges.values_mut() {
                        if edge.from == *target || edge.to == *target {
                            edge.tombstoned = true;
                        }
                    }
                }
            }
            OpKind::AddRefEdge {
                category,
                from,
                to,
            } => {
                // Un-tombstone on echo (see backend.rs:490-520 for rationale)
                self.edges
                    .entry(ctx.op_id)
                    .and_modify(|rec| {
                        rec.tombstoned = false;
                        rec.from = *from;
                        rec.to = *to;
                        rec.category = category.clone();
                    })
                    .or_insert_with(|| EdgeStructure {
                        from: *from,
                        to: *to,
                        category: category.clone(),
                        tombstoned: false,
                    });
            }
            OpKind::RemoveRefEdge { target } => {
                if let Some(rec) = self.edges.get_mut(target) {
                    rec.tombstoned = true;
                }
            }
            OpKind::Move {
                target,
                new_parent,
                position,
            } => {
                // Cycle check: if proposed parent would create a cycle, no-op
                if let Some(parent_id) = new_parent {
                    if self.would_create_cycle(*target, *parent_id) {
                        self.pending_moves.remove(&ctx.op_id);
                        return;
                    }
                }
                if let Some(rec) = self.nodes.get_mut(target) {
                    rec.tree_parent = *new_parent;
                    rec.order_key = Some(position.clone());
                }
                self.pending_moves.remove(&ctx.op_id);
            }
            // Property ops are routed to schema layer, not here
            OpKind::SetNodeProperty { .. } | OpKind::SetRefEdgeProperty { .. } => {
                // Unreachable: extract_property_op filters these out
            }
        }
    }

    fn snapshot(&self) -> Self::SnapshotState {
        let mut nodes: Vec<NodeSnap> = self
            .nodes
            .iter()
            .filter(|(_, rec)| !rec.tombstoned)
            .map(|(id, rec)| NodeSnap {
                id: *id,
                order_key: rec.order_key.clone(),
                tree_parent: rec.tree_parent,
            })
            .collect();
        nodes.sort_by_key(|n| n.id);

        let mut edges: Vec<EdgeSnap> = self
            .edges
            .iter()
            .filter(|(_, rec)| !rec.tombstoned)
            .map(|(id, rec)| EdgeSnap {
                id: *id,
                from: rec.from,
                to: rec.to,
                category: rec.category.clone(),
            })
            .collect();
        edges.sort_by_key(|e| e.id);

        GraphSnapshot { nodes, edges }
    }

    fn restore(&mut self, snap: Self::SnapshotState) {
        self.nodes.clear();
        self.edges.clear();
        self.pending_moves.clear();

        for n in snap.nodes {
            self.nodes.insert(
                n.id,
                NodeStructure {
                    tombstoned: false,
                    order_key: n.order_key,
                    tree_parent: n.tree_parent,
                },
            );
        }

        for e in snap.edges {
            self.edges.insert(
                e.id,
                EdgeStructure {
                    from: e.from,
                    to: e.to,
                    category: e.category,
                    tombstoned: false,
                },
            );
        }
    }

    fn extract_property_op(op: &Self::OpKind) -> Option<PropertyOp> {
        match op {
            OpKind::SetNodeProperty { target, path, delta } => Some(PropertyOp {
                target: *target,
                path: path.clone(),
                delta: delta.clone(),
            }),
            OpKind::SetRefEdgeProperty { target, path, delta } => Some(PropertyOp {
                target: *target,
                path: path.clone(),
                delta: delta.clone(),
            }),
            _ => None,
        }
    }

    fn extract_new_entity_id(op: &Self::OpKind, ctx: &CausalContext) -> Option<CrdtId> {
        match op {
            OpKind::AddNode | OpKind::AddRefEdge { .. } => Some(ctx.op_id),
            _ => None,
        }
    }

    fn op_kind_label(op: &Self::OpKind) -> &'static str {
        match op {
            OpKind::AddNode => "AddNode",
            OpKind::RemoveNode { .. } => "RemoveNode",
            OpKind::AddRefEdge { .. } => "AddRefEdge",
            OpKind::RemoveRefEdge { .. } => "RemoveRefEdge",
            OpKind::Move { .. } => "Move",
            OpKind::SetNodeProperty { .. } => "SetNodeProperty",
            OpKind::SetRefEdgeProperty { .. } => "SetRefEdgeProperty",
        }
    }
}
