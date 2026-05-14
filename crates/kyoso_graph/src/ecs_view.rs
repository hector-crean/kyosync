//! [`kyoso_graph_crdt::GraphView`] over the Bevy-ECS graph store.
//!
//! Walks the same Bevy relationships the rest of `kyoso_graph` uses:
//!
//! - [`crate::components::EdgeFrom`] / [`crate::components::EdgeTo`] —
//!   per-edge endpoint components (an edge is its own entity).
//! - [`crate::components::OutgoingEdges`] /
//!   [`crate::components::IncomingEdges`] — auto-maintained reverse
//!   indices via Bevy's relationship machinery.
//! - [`crate::tree::TreeParent`] — node's parent in the tree.
//!
//! "Live" on this side means "the entity exists and carries the
//! marker component"; despawned entities aren't queryable, which
//! matches the tombstoned-hidden contract on the CRDT side.
//!
//! Construct one inside a Bevy system via [`SystemParam`] and pass
//! `&view` to the generic algorithms in
//! [`kyoso_graph_crdt::view`] — cycle detection, reachability,
//! connected component. The point is that those algorithms are
//! identical to the ones running headless on the server, which is
//! what makes the cross-store cycle-check test (see
//! `tests/cross_view.rs`) meaningful.

use bevy::ecs::{query::QueryData, system::SystemParam};
use bevy::prelude::*;
use kyoso_graph_crdt::view::GraphView;

use crate::components::{EdgeFrom, EdgeTo, IncomingEdges, OutgoingEdges};
use crate::tree::TreeParent;

/// `SystemParam` that exposes the ECS graph store through
/// [`GraphView`]. The view borrows the world for the system's
/// duration; algorithms run synchronously inside the system body.
///
/// Unlike [`crate::queries::GraphQuery`] this view is intentionally
/// untyped — no `Node` / `Edge` generic — because the algorithms
/// behind [`GraphView`] don't need to peek at domain components. If
/// you need typed access alongside the view, hold both as separate
/// `SystemParam`s in the same system.
#[derive(SystemParam)]
pub struct EcsGraphView<'w, 's> {
    /// Per-edge `(EdgeFrom, EdgeTo)`. The query filter is empty so
    /// the view sees every edge entity in the world; `outgoing` /
    /// `incoming` filter out edges whose endpoints are despawned.
    edges: Query<'w, 's, EdgeEndpointQuery>,
    /// `OutgoingEdges` is auto-attached by Bevy's relationship
    /// machinery to any entity that's the target of an `EdgeFrom`.
    outgoing_index: Query<'w, 's, &'static OutgoingEdges>,
    /// Mirror of `outgoing_index` for incoming.
    incoming_index: Query<'w, 's, &'static IncomingEdges>,
    /// Per-node `TreeParent`. Missing component → root (no parent).
    parents: Query<'w, 's, &'static TreeParent>,
    /// All entities that have a `TreeParent`. Used by `live_nodes()`
    /// — we treat any entity with this component as a node, since
    /// the structural sync layer attaches `TreeParent` to every
    /// replicated node entity at spawn time.
    all_nodes: Query<'w, 's, Entity, With<TreeParent>>,
}

/// Endpoint data for one edge entity. Pulled out as `QueryData` so
/// the view's iterators are nameable.
#[derive(QueryData)]
struct EdgeEndpointQuery {
    entity: Entity,
    from: &'static EdgeFrom,
    to: &'static EdgeTo,
}

impl<'w, 's> GraphView for EcsGraphView<'w, 's> {
    type NodeId = Entity;
    type EdgeId = Entity;

    fn is_live_node(&self, id: Self::NodeId) -> bool {
        // A node is "live" iff it has `TreeParent`. We use `parents`
        // (which queries `&TreeParent`) instead of a dedicated marker
        // because the structural sync layer attaches `TreeParent` on
        // every replicated node — see
        // `kyoso_graph_sync::plugin::project_op` and
        // `project_snapshot`. An entity that's been despawned will
        // simply fail `get`, which is the correct "not live" answer.
        self.parents.get(id).is_ok()
    }

    fn is_live_edge(&self, id: Self::EdgeId) -> bool {
        self.edges.get(id).is_ok()
    }

    fn tree_parent(&self, id: Self::NodeId) -> Option<Self::NodeId> {
        self.parents.get(id).ok().and_then(|p| p.0)
    }

    fn outgoing(
        &self,
        id: Self::NodeId,
    ) -> impl Iterator<Item = (Self::EdgeId, Self::NodeId)> + '_ {
        self.outgoing_index
            .get(id)
            .into_iter()
            .flat_map(move |idx| idx.iter())
            .filter_map(move |edge_e| {
                let item = self.edges.get(edge_e).ok()?;
                // Skip edges whose `to` endpoint has been despawned —
                // the trait's "live edges to live neighbors only"
                // contract.
                self.is_live_node(item.to.0).then_some((edge_e, item.to.0))
            })
    }

    fn incoming(
        &self,
        id: Self::NodeId,
    ) -> impl Iterator<Item = (Self::EdgeId, Self::NodeId)> + '_ {
        self.incoming_index
            .get(id)
            .into_iter()
            .flat_map(move |idx| idx.iter())
            .filter_map(move |edge_e| {
                let item = self.edges.get(edge_e).ok()?;
                self.is_live_node(item.from.0)
                    .then_some((edge_e, item.from.0))
            })
    }

    fn live_nodes(&self) -> impl Iterator<Item = Self::NodeId> + '_ {
        self.all_nodes.iter()
    }
}
