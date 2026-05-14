//! Client-side graph sync engine: id-generation, op queue, applied-seq
//! tracking plus the topology bookkeeping that detection systems need
//! for echo prevention and field-level diffs.
//!
//! Wraps a [`GraphBackend<EmptySchema>`] internally — structure-only
//! backend for ECS sync (typed properties live in per-component
//! [`crate::schema_sync::SchemaDoc`] resources, not here).
//!
//! ## Why this exists separately from `GraphBackend`
//!
//! `GraphBackend` is a model-agnostic CRDT engine in `kyoso_graph_crdt`
//! — it has no Bevy types and no `Resource` derive. `ClientSyncEngine`
//! is the Bevy `Resource` wrapper that lets [`crate::GraphSyncPlugin`]
//! systems share one mutable handle to the engine across the inbound,
//! outbound, and detection phases of each frame.
//!
//! Server-side mirrors stay as plain `GraphBackend<…>` (no Bevy ECS,
//! no presence concept).

use bevy::prelude::*;
use kyoso_crdt::{ApplyError, CrdtId, EmptySchema, GlobalSeq, IdGen, OpaqueRecord, PeerId};
use kyoso_graph_crdt::{EdgeCategory, GraphBackend, GraphTopology, OpKind};

type Op = kyoso_crdt::Op<OpKind>;
/// In-memory snapshot of the structural graph backend. Resolves to the
/// generic [`kyoso_crdt::Snapshot`] over [`GraphTopology`] + [`EmptySchema`]
/// since the engine owns structure only — typed properties live in
/// per-schema [`crate::schema_sync::SchemaDoc`] resources.
pub type EngineSnapshot = kyoso_crdt::Snapshot<GraphTopology, EmptySchema>;

/// Wire-format snapshot as produced by the server. Carries the same
/// structural topology as [`EngineSnapshot`] **plus** opaque per-entity
/// CRDT state ([`OpaqueRecord`]) so late joiners can hydrate
/// typed [`crate::schema_sync::SchemaDoc`] resources from the snapshot
/// rather than replaying every property op from sequence 0.
pub type ServerSnapshot = kyoso_crdt::Snapshot<GraphTopology, OpaqueRecord>;

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

    pub fn restore(&mut self, snap: EngineSnapshot) {
        self.inner.restore(snap);
    }

    #[must_use]
    pub fn snapshot(&self) -> EngineSnapshot {
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

    /// Mint a fresh op-id from the engine's id generator.
    ///
    /// `pub(crate)` because the standard outbound path is
    /// `add_node` / `add_edge` / `move_node` / etc.; the only caller
    /// inside the crate is [`crate::schema_sync::detect_typed_changes`],
    /// which has to mint ids and queue ops by hand because the typed
    /// schema layer doesn't know the structural-op flavor at compile
    /// time. External callers should not bypass the structural API.
    pub(crate) fn mint_id(&mut self) -> CrdtId {
        self.inner.mint_id()
    }

    /// Push a pre-built [`Op`] onto the pending queue.
    ///
    /// `pub(crate)` — same rationale as [`Self::mint_id`].
    pub(crate) fn enqueue(&mut self, op: Op) {
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
}
