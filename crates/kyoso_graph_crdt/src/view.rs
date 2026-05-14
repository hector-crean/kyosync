//! Read-only graph-topology abstraction.
//!
//! [`GraphView`] is the bridge between the two graph stores in the
//! kyoso workspace:
//!
//! - [`crate::GraphTopology`] — `HashMap<CrdtId, …>`, headless. Runs
//!   on the server, inside chaos sim peers, inside proptest cases.
//! - `kyoso_graph::ecs_view::EcsGraphView` — over `bevy_ecs` queries,
//!   uses Bevy relationships (`EdgeFrom` / `EdgeTo` /
//!   `OutgoingEdges` / `IncomingEdges` / `TreeParent`). Runs on every
//!   client that mounts `GraphSyncPlugin`.
//!
//! Both stores describe the same domain (nodes in a tree + reference
//! edges); only the backing storage differs. By projecting both behind
//! one trait, the traversal algorithms below — cycle detection,
//! reachability, ancestry — exist exactly once and stay in lockstep.
//!
//! The trait is intentionally minimal: only what the algorithms need.
//! Domain-specific accessors (`order_key`, edge `category`, …) stay on
//! the concrete types where they belong.

use std::hash::Hash;

/// Read-only view over a graph topology.
///
/// "Live" means non-tombstoned (CRDT side) or non-despawned (ECS
/// side); implementations must hide deleted nodes/edges from every
/// method on this trait.
pub trait GraphView {
    /// Stable identity for nodes. `CrdtId` for the headless topology;
    /// `Entity` for the ECS view.
    type NodeId: Copy + Eq + Hash;
    /// Stable identity for edges. Same backing type as `NodeId` in
    /// practice but kept distinct so callers can't accidentally pass
    /// an edge id where a node id is expected.
    type EdgeId: Copy + Eq + Hash;

    /// `true` iff `id` names a live node.
    fn is_live_node(&self, id: Self::NodeId) -> bool;

    /// `true` iff `id` names a live edge.
    fn is_live_edge(&self, id: Self::EdgeId) -> bool;

    /// Tree parent of a live node, or `None` for a root / unknown id /
    /// tombstoned node.
    fn tree_parent(&self, id: Self::NodeId) -> Option<Self::NodeId>;

    /// Iterate `(edge_id, neighbor_node_id)` for every live edge with
    /// `from == id`. Implementations must skip tombstoned edges and
    /// edges whose `to` endpoint is tombstoned.
    fn outgoing(
        &self,
        id: Self::NodeId,
    ) -> impl Iterator<Item = (Self::EdgeId, Self::NodeId)> + '_;

    /// Iterate `(edge_id, source_node_id)` for every live edge with
    /// `to == id`.
    fn incoming(
        &self,
        id: Self::NodeId,
    ) -> impl Iterator<Item = (Self::EdgeId, Self::NodeId)> + '_;

    /// Iterate every live node id. Order is unspecified.
    fn live_nodes(&self) -> impl Iterator<Item = Self::NodeId> + '_;
}

// ---------------------------------------------------------------------------
// Free-function algorithms — generic over any `GraphView`.
//
// These are the canonical implementations. The HashMap-backed
// `GraphTopology::would_create_cycle` keeps its inherent method as a
// thin caller into `would_create_cycle(self, …)` so chaos-sim hot
// paths don't have to import the trait.
// ---------------------------------------------------------------------------

/// `true` iff making `proposed_parent` the new parent of `target`
/// would form a cycle in the tree.
///
/// Walks `tree_parent` from `proposed_parent` upward; if the walk
/// ever lands on `target`, the move would cycle. Linear in the depth
/// of the proposed-parent's ancestry — bounded by the tree's height.
pub fn would_create_cycle<G: GraphView>(
    view: &G,
    target: G::NodeId,
    proposed_parent: G::NodeId,
) -> bool {
    if target == proposed_parent {
        return true;
    }
    let mut cursor = Some(proposed_parent);
    while let Some(id) = cursor {
        if id == target {
            return true;
        }
        cursor = view.tree_parent(id);
    }
    false
}

/// Collect `start`'s ancestors via the `tree_parent` chain. Excludes
/// `start` itself; ordered root-distance ascending (immediate parent
/// first).
pub fn ancestors<G: GraphView>(view: &G, start: G::NodeId) -> Vec<G::NodeId> {
    let mut out = Vec::new();
    let mut cursor = view.tree_parent(start);
    while let Some(id) = cursor {
        // Cycle guard. A well-formed topology has no parent cycles, but
        // a snapshot under construction or a buggy apply can produce
        // one; we'd rather return a truncated list than spin forever.
        if out.contains(&id) || id == start {
            break;
        }
        out.push(id);
        cursor = view.tree_parent(id);
    }
    out
}

/// Collect all nodes reachable from `root` via the tree (children,
/// grandchildren, …). Output includes `root`. Order is BFS over the
/// tree fanning out via `outgoing` filtered by parent relationship —
/// `kyoso_graph::tree::TreeEdge` is the only way to express "child of"
/// in ECS, so we approximate by checking which outgoing neighbors
/// claim `n` as their `tree_parent`. Linear in subtree size.
pub fn descendants<G: GraphView>(view: &G, root: G::NodeId) -> Vec<G::NodeId> {
    let mut out = vec![root];
    let mut frontier = vec![root];
    while let Some(n) = frontier.pop() {
        for candidate in view.live_nodes() {
            if view.tree_parent(candidate) == Some(n) && !out.contains(&candidate) {
                out.push(candidate);
                frontier.push(candidate);
            }
        }
    }
    out
}

/// Collect the undirected connected component containing `start`,
/// walking `outgoing` and `incoming` edges. Output includes `start`.
/// BFS; bounded by the number of live nodes.
pub fn connected_component_undirected<G: GraphView>(
    view: &G,
    start: G::NodeId,
) -> Vec<G::NodeId> {
    let mut out = vec![start];
    let mut frontier = vec![start];
    while let Some(n) = frontier.pop() {
        for (_, neighbor) in view.outgoing(n) {
            if !out.contains(&neighbor) {
                out.push(neighbor);
                frontier.push(neighbor);
            }
        }
        for (_, neighbor) in view.incoming(n) {
            if !out.contains(&neighbor) {
                out.push(neighbor);
                frontier.push(neighbor);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// `GraphView` for the headless `GraphTopology` (`HashMap`-backed).
// ---------------------------------------------------------------------------

use kyoso_crdt::CrdtId;

use crate::topology::GraphTopology;

impl GraphView for GraphTopology {
    type NodeId = CrdtId;
    type EdgeId = CrdtId;

    fn is_live_node(&self, id: Self::NodeId) -> bool {
        GraphTopology::is_live_node(self, id)
    }

    fn is_live_edge(&self, id: Self::EdgeId) -> bool {
        GraphTopology::is_live_edge(self, id)
    }

    fn tree_parent(&self, id: Self::NodeId) -> Option<Self::NodeId> {
        GraphTopology::tree_parent(self, id)
    }

    fn outgoing(
        &self,
        id: Self::NodeId,
    ) -> impl Iterator<Item = (Self::EdgeId, Self::NodeId)> + '_ {
        // Filter out edges whose `to` endpoint is tombstoned — the
        // trait's contract is "live edges to live neighbors only."
        self.outgoing_edge_ids(id).filter_map(move |edge_id| {
            let (_from, to) = self.edge_endpoints(edge_id)?;
            self.is_live_node(to).then_some((edge_id, to))
        })
    }

    fn incoming(
        &self,
        id: Self::NodeId,
    ) -> impl Iterator<Item = (Self::EdgeId, Self::NodeId)> + '_ {
        self.incoming_edge_ids(id).filter_map(move |edge_id| {
            let (from, _to) = self.edge_endpoints(edge_id)?;
            self.is_live_node(from).then_some((edge_id, from))
        })
    }

    fn live_nodes(&self) -> impl Iterator<Item = Self::NodeId> + '_ {
        self.live_node_ids()
    }
}
