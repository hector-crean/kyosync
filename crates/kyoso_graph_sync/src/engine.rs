//! Client-side graph sync engine: id-generation, op queue, applied-seq
//! tracking plus the property/topology bookkeeping that detection
//! systems need for echo prevention and field-level diffs.
//!
//! Wraps a [`GraphBackend<EmptySchema>`] internally — structure-only
//! backend for ECS sync (properties are managed separately via schema
//! sync plugins).
//!
//! ## Why this exists separately from `GraphBackend`
//!
//! `GraphBackend` is a model-agnostic CRDT engine in `kyoso_graph_crdt`
//! — it has no Bevy types and no `Resource` derive. `ClientSyncEngine`
//! is the Bevy `Resource` wrapper that adds:
//!
//! - The `just_projected` set used by detection systems to suppress
//!   echo of remote ops the inbound projector just handled.
//! - A stable Bevy-side type that the [`crate::GraphSyncPlugin`]
//!   systems can reference.
//!
//! Server-side mirrors stay as plain `GraphBackend<EmptySchema>` (no
//! Bevy ECS, no presence concept).

use bevy::prelude::*;
use kyoso_crdt::{ApplyError, CrdtId, EmptySchema, GlobalSeq, IdGen, PeerId};
use kyoso_graph_crdt::{EdgeCategory, GraphBackend, OpKind, Snapshot};
use std::collections::HashSet;

type Op = kyoso_crdt::Op<OpKind>;

/// Client-side graph sync resource.
///
/// Constructed from a peer-level [`IdGen`] cloned from
/// [`kyoso_sync::PeerIdGen`]. Sharing the handle keeps the per-peer
/// `LocalSeq` counter unified across all CRDT models on the peer —
/// required for safe cross-model `CrdtId` references (a comment
/// anchored to a graph node, etc.).
#[derive(Resource)]
pub struct ClientSyncEngine {
    inner: GraphBackend<EmptySchema>,
    /// Op IDs the inbound projector spawned this frame. Detection
    /// systems skip these to suppress echoing remote ops back. Cleared
    /// at the end of each frame.
    just_projected: HashSet<CrdtId>,
}

impl Default for ClientSyncEngine {
    fn default() -> Self {
        Self::with_peer(0)
    }
}

impl ClientSyncEngine {
    #[must_use]
    pub fn with_peer(peer: PeerId) -> Self {
        Self::with_shared_ids(IdGen::new(peer))
    }

    /// Construct with a shared peer-level id source. Production code
    /// clones [`kyoso_sync::PeerIdGen::handle`] and passes it here so
    /// the graph backend mints from the same `LocalSeq` namespace as
    /// every other CRDT model on the peer.
    #[must_use]
    pub fn with_shared_ids(ids: IdGen) -> Self {
        Self {
            inner: GraphBackend::with_shared_ids(ids),
            just_projected: HashSet::new(),
        }
    }

    pub fn set_peer(&mut self, peer: PeerId) {
        self.inner.set_peer(peer);
    }

    /// Cloneable handle to the engine's id source.
    #[must_use]
    pub fn ids(&self) -> &IdGen {
        self.inner.ids()
    }

    #[must_use]
    pub fn peer(&self) -> PeerId {
        self.inner.peer()
    }

    // -----------------------------------------------------------------
    // Op generation (mints CrdtId, queues op for upstream delivery)
    // -----------------------------------------------------------------

    pub fn add_node(&mut self) -> CrdtId {
        self.inner.add_node()
    }

    pub fn add_edge(&mut self, from: CrdtId, to: CrdtId) -> CrdtId {
        self.inner.add_edge(from, to)
    }

    pub fn add_ref_edge_with_category(
        &mut self,
        from: CrdtId,
        to: CrdtId,
        category: EdgeCategory,
    ) -> CrdtId {
        self.inner.add_ref_edge_with_category(from, to, category)
    }

    pub fn remove_node(&mut self, n: CrdtId) -> bool {
        self.inner.remove_node(n)
    }

    pub fn remove_edge(&mut self, e: CrdtId) -> bool {
        self.inner.remove_edge(e)
    }

    pub fn move_node(
        &mut self,
        target: CrdtId,
        new_parent: Option<CrdtId>,
        position: String,
    ) -> bool {
        self.inner.move_node(target, new_parent, position)
    }

    // -----------------------------------------------------------------
    // Op application (server-confirmed ops apply through here)
    // -----------------------------------------------------------------

    pub fn apply_remote(&mut self, op: &Op) -> Result<(), ApplyError> {
        self.inner.apply_remote(op)
    }

    pub fn restore(&mut self, snap: Snapshot) {
        self.inner.restore(snap);
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.inner.snapshot()
    }

    // -----------------------------------------------------------------
    // Sync bookkeeping
    // -----------------------------------------------------------------

    #[must_use]
    pub fn applied_seq(&self) -> GlobalSeq {
        self.inner.applied_seq()
    }

    pub fn drain_pending(&mut self) -> Vec<Op> {
        self.inner.drain_pending()
    }

    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.inner.pending_len()
    }

    /// Mint a fresh op-id from the engine's id generator. Typed
    /// plugins use this to keep id-generation centralized.
    pub fn next_id(&mut self) -> CrdtId {
        self.inner.next_id()
    }

    /// Push a pre-built [`Op`] onto the pending queue. The outbound
    /// system drains the queue each tick and ships ops via the WS.
    pub fn enqueue(&mut self, op: Op) {
        self.inner.enqueue(op);
    }

    /// Live node count (excludes tombstones).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// Live edge count (excludes tombstones).
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    // -----------------------------------------------------------------
    // Read-side accessors used by detection systems for diff & echo
    // prevention.
    // -----------------------------------------------------------------

    #[must_use]
    pub fn tree_parent(&self, id: CrdtId) -> Option<CrdtId> {
        self.inner.tree_parent(id)
    }

    #[must_use]
    pub fn node_order_key(&self, id: CrdtId) -> Option<&str> {
        self.inner.node_order_key(id)
    }

    #[must_use]
    pub fn edge_endpoints(&self, id: CrdtId) -> Option<(CrdtId, CrdtId)> {
        self.inner.edge_endpoints(id)
    }

    #[must_use]
    pub fn edge_category(&self, id: CrdtId) -> Option<&EdgeCategory> {
        self.inner.edge_category(id)
    }

    pub fn outgoing_edge_ids(&self, n: CrdtId) -> impl Iterator<Item = CrdtId> + '_ {
        self.inner.outgoing_edge_ids(n)
    }

    pub fn incoming_edge_ids(&self, n: CrdtId) -> impl Iterator<Item = CrdtId> + '_ {
        self.inner.incoming_edge_ids(n)
    }

    // -----------------------------------------------------------------
    // Echo prevention
    // -----------------------------------------------------------------

    pub fn mark_just_projected(&mut self, id: CrdtId) {
        self.just_projected.insert(id);
    }

    #[must_use]
    pub fn is_just_projected(&self, id: CrdtId) -> bool {
        self.just_projected.contains(&id)
    }

    pub fn clear_just_projected(&mut self) {
        self.just_projected.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_mints_unique_ids() {
        let mut engine = ClientSyncEngine::with_peer(7);
        let a = engine.add_node();
        let b = engine.add_node();
        assert_eq!(a.peer, 7);
        assert_eq!(b.peer, 7);
        assert_ne!(a.seq, b.seq);
    }

    #[test]
    fn engine_drains_pending() {
        let mut engine = ClientSyncEngine::with_peer(1);
        let _id = engine.add_node();
        assert_eq!(engine.pending_len(), 1);
        let drained = engine.drain_pending();
        assert_eq!(drained.len(), 1);
        assert!(matches!(drained[0].kind, OpKind::AddNode));
        assert_eq!(engine.pending_len(), 0);
    }

    #[test]
    fn just_projected_is_per_frame() {
        let mut engine = ClientSyncEngine::with_peer(1);
        let id = CrdtId::new(2, 0);
        engine.mark_just_projected(id);
        assert!(engine.is_just_projected(id));
        engine.clear_just_projected();
        assert!(!engine.is_just_projected(id));
    }
}
