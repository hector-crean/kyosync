//! [`kyoso_graph_crdt::GraphView`] over the Bevy-ECS graph store.
//!
//! Walks the same Bevy relationships the rest of `kyoso_graph` uses:
//!
//! - [`crate::components::EdgeFrom`] / [`crate::components::EdgeTo`] —
//!   per-edge endpoint components (an edge is its own entity).
//! - [`crate::components::OutgoingEdges`] /
//!   [`crate::components::IncomingEdges`] — auto-maintained reverse
//!   indices via Bevy's relationship machinery.
//! - `ChildOf` (Bevy native) — node's parent in the tree. Roots have
//!   no `ChildOf` component.
//! - A caller-supplied node marker `NM` — identifies which entities
//!   are "nodes" of this view's graph. Necessary because roots don't
//!   have any structural marker on their own (no `ChildOf`, possibly
//!   no `Children`), so we need a separate signal.
//!
//! "Live" on this side means "the entity exists and carries the
//! marker component"; despawned entities aren't queryable, which
//! matches the tombstoned-hidden contract on the CRDT side.
//!
//! Construct one inside a Bevy system via [`SystemParam`] and pass
//! `&view` to the generic algorithms in
//! [`kyoso_graph_crdt::view`] — cycle detection, reachability,
//! connected component. The point is that those algorithms are
//! identical to the ones running headless on the server.

use bevy::ecs::{query::QueryData, system::SystemParam};
use bevy::prelude::*;
use kyoso_graph_crdt::view::GraphView;

use crate::components::{EdgeFrom, EdgeTo, IncomingEdges, OutgoingEdges};

/// `SystemParam` that exposes the ECS graph store through
/// [`GraphView`]. Parameterised over a node marker `NM` so the
/// "what entities count as nodes" question is settled at the call site.
///
/// Unlike [`crate::queries::GraphQuery`] this view is intentionally
/// untyped on the edge side — no `Edge` generic — because the
/// algorithms behind [`GraphView`] don't need to peek at domain
/// components. If you need typed access alongside the view, hold both
/// as separate `SystemParam`s in the same system.
#[derive(SystemParam)]
pub struct EcsGraphView<'w, 's, NM: Component> {
    /// Per-edge `(EdgeFrom, EdgeTo)`. The query filter is empty so the
    /// view sees every edge entity in the world; `outgoing` /
    /// `incoming` filter out edges whose endpoints are despawned.
    edges: Query<'w, 's, EdgeEndpointQuery>,
    /// `OutgoingEdges` is auto-attached by Bevy's relationship
    /// machinery to any entity that's the target of an `EdgeFrom`.
    outgoing_index: Query<'w, 's, &'static OutgoingEdges>,
    /// Mirror of `outgoing_index` for incoming.
    incoming_index: Query<'w, 's, &'static IncomingEdges>,
    /// Per-node parent. Missing component → root.
    parents: Query<'w, 's, &'static ChildOf>,
    /// All entities carrying the node marker `NM`. The caller pins
    /// `NM` at the use site (e.g. `EcsGraphView<SceneNode>` for the
    /// kyoso scene tree, `EcsGraphView<TestNode>` for the cross-view
    /// regression test).
    all_nodes: Query<'w, 's, Entity, With<NM>>,
}

/// Endpoint data for one edge entity. Pulled out as `QueryData` so
/// the view's iterators are nameable.
#[derive(QueryData)]
struct EdgeEndpointQuery {
    entity: Entity,
    from: &'static EdgeFrom,
    to: &'static EdgeTo,
}

impl<'w, 's, NM: Component> GraphView for EcsGraphView<'w, 's, NM> {
    type NodeId = Entity;
    type EdgeId = Entity;

    fn is_live_node(&self, id: Self::NodeId) -> bool {
        // A node is "live" iff it has the `NM` marker. An entity
        // that's been despawned will simply fail `get`, which is the
        // correct "not live" answer.
        self.all_nodes.get(id).is_ok()
    }

    fn is_live_edge(&self, id: Self::EdgeId) -> bool {
        self.edges.get(id).is_ok()
    }

    fn tree_parent(&self, id: Self::NodeId) -> Option<Self::NodeId> {
        self.parents.get(id).ok().map(|c| c.0)
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
