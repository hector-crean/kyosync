//! Graph-specific backend wrapper.
//!
//! Provides `GraphBackend<S>` as a newtype wrapper around `Backend<GraphTopology, S>`
//! with domain-specific methods for graph operations.

use kyoso_crdt::delta::{Path, WireDelta};
use kyoso_crdt::id::{CrdtId, GlobalSeq, IdGen, PeerId};
use kyoso_crdt::lattice::Crdt;
use kyoso_crdt::model::{ApplyError, CrdtModel};
use kyoso_crdt::op::Op;
use kyoso_crdt::schema::{IntoWireOp, SchemaApply};
use kyoso_crdt::{Backend, Topology};

use crate::edge_category::EdgeCategory;
use crate::op::OpKind;
use crate::topology::GraphTopology;

/// Graph CRDT backend: `Backend<GraphTopology, S>` with domain methods.
///
/// Wraps the generic backend to provide graph-specific operations like
/// `add_node`, `remove_node`, `move_node`, etc.
pub struct GraphBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    inner: Backend<GraphTopology, S>,
}

impl<S> GraphBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    /// Construct with a fresh peer ID.
    pub fn with_peer(peer: PeerId) -> Self {
        Self {
            inner: Backend::with_peer(peer),
        }
    }

    /// Construct sharing an ID generator with other models.
    pub fn with_shared_ids(ids: IdGen) -> Self {
        Self {
            inner: Backend::with_shared_ids(ids),
        }
    }

    /// Access the underlying generic backend.
    pub fn backend(&self) -> &Backend<GraphTopology, S> {
        &self.inner
    }

    /// Access the underlying generic backend (mutable).
    pub fn backend_mut(&mut self) -> &mut Backend<GraphTopology, S> {
        &mut self.inner
    }

    /// Get this replica's peer ID.
    pub fn peer(&self) -> PeerId {
        self.inner.peer()
    }

    /// Set this replica's peer ID.
    pub fn set_peer(&mut self, peer: PeerId) {
        self.inner.set_peer(peer);
    }

    /// Get the highest applied sequence number.
    pub fn applied_seq(&self) -> GlobalSeq {
        self.inner.applied_seq()
    }

    /// Get a handle to the ID generator.
    pub fn ids(&self) -> &IdGen {
        self.inner.ids()
    }

    /// Mint a fresh op ID from the underlying ID generator.
    ///
    /// Used by typed-schema sync plugins that pre-mint an op ID, build
    /// the op themselves, and push it via [`Self::enqueue`].
    pub fn mint_id(&mut self) -> CrdtId {
        self.inner.mint_id()
    }

    /// Push a pre-built op onto the pending queue.
    ///
    /// Escape hatch for callers that construct the op themselves (e.g.
    /// typed-schema sync plugins building `SetNodeProperty` ops with
    /// pre-minted IDs). Most code should prefer the domain methods
    /// ([`Self::add_node`], [`Self::move_node`], etc.) which queue ops
    /// internally.
    pub fn enqueue(&mut self, op: Op<OpKind>) {
        self.inner.pending_mut().push(op);
    }

    /// Ensure a schema slot exists for the given entity ID.
    pub fn ensure_node(&mut self, id: CrdtId) {
        self.inner.ensure_schema(id);
    }

    /// Apply just the property portion of an op directly to the schema,
    /// bypassing `GlobalSeq` validation.
    ///
    /// Used by per-schema secondary stores (typed-schema sync plugins)
    /// that mirror property ops already applied to a primary backend.
    pub fn apply_property_op(
        &mut self,
        op: &Op<OpKind>,
    ) -> Result<(), kyoso_crdt::DeltaError> {
        self.inner.apply_property_op(op)
    }

    /// Mint a new node and queue an AddNode op. Returns the node's ID.
    ///
    /// Pre-inserts a default schema entry so subsequent `mutate_node()` calls
    /// work before the server echo arrives.
    pub fn add_node(&mut self) -> CrdtId {
        let id = self.inner.mint_id();
        // Pre-insert schema entry so mutate_node works before echo
        self.inner.ensure_schema(id);
        self.inner.pending_mut().push(Op::new(id, OpKind::AddNode));
        id
    }

    /// Tombstone a node and queue a RemoveNode op.
    /// Returns `false` if the node is unknown or already tombstoned.
    pub fn remove_node(&mut self, target: CrdtId) -> bool {
        // Check topology first (authoritative for applied nodes)
        let topology_says_live = self.inner.topology().is_live_node(target);
        // Also check schema (catches pre-inserted nodes from add_node)
        let schema_exists = self.inner.schema(target).is_some();

        if !topology_says_live && !schema_exists {
            return false;
        }
        self.inner.queue_op(OpKind::RemoveNode { target });
        true
    }

    /// Atomic Kleppmann move. Queues a Move op for server confirmation.
    ///
    /// Returns `false` if a cycle would be created, otherwise `true`.
    pub fn move_node(
        &mut self,
        target: CrdtId,
        new_parent: Option<CrdtId>,
        position: String,
    ) -> bool {
        // Cycle check before queueing
        if let Some(parent_id) = new_parent {
            if self.inner.topology().would_create_cycle(target, parent_id) {
                return false;
            }
        }
        let op_id = self.inner.queue_op(OpKind::Move {
            target,
            new_parent,
            position,
        });
        // Track this move so detection systems can suppress re-emitting
        self.inner.topology_mut().track_pending_move(op_id, target);
        true
    }

    /// Create a reference edge with the given category. Returns the edge's ID.
    pub fn add_ref_edge_with_category(
        &mut self,
        from: CrdtId,
        to: CrdtId,
        category: EdgeCategory,
    ) -> CrdtId {
        self.inner.queue_op(OpKind::AddRefEdge {
            category,
            from,
            to,
        })
    }

    /// Create a reference edge with default category. Returns the edge's ID.
    pub fn add_edge(&mut self, from: CrdtId, to: CrdtId) -> CrdtId {
        self.add_ref_edge_with_category(from, to, EdgeCategory::Reference)
    }

    /// Tombstone an edge and queue a RemoveRefEdge op.
    pub fn remove_edge(&mut self, target: CrdtId) -> bool {
        if self.inner.topology().edge_endpoints(target).is_none() {
            return false;
        }
        self.inner.queue_op(OpKind::RemoveRefEdge { target });
        true
    }

    /// Mutate a node's schema and queue a SetNodeProperty op.
    ///
    /// Does NOT pre-apply the mutation locally. The mutation becomes visible
    /// after the server echo arrives. This prevents double-apply and LWW
    /// stamp ordering issues (see document.rs:69-89 for rationale).
    pub fn mutate_node(&mut self, target: CrdtId, mutation: S::Mutation) -> Option<()>
    where
        S: Clone,
    {
        let schema = self.inner.schema(target)?;
        let mut throwaway = schema.clone();
        let op_id = self.inner.mint_id();
        let mut ctx = kyoso_crdt::context::CausalContext::new(
            op_id,
            None,
            self.inner.causal_state_mut(),
        );
        let delta = throwaway.mutate(mutation, &mut ctx);
        let (path, wire_delta) = delta.into_wire_op();

        // Queue SetNodeProperty op without pre-applying
        self.inner.pending_mut().push(Op::new(
            op_id,
            OpKind::SetNodeProperty {
                target,
                path,
                delta: wire_delta,
            },
        ));

        Some(())
    }

    /// Set a node property to a raw byte value (LWW).
    pub fn set_node_property(&mut self, target: CrdtId, key: String, value: Vec<u8>) {
        let op_id = self.inner.mint_id();
        self.inner.pending_mut().push(Op::new(
            op_id,
            OpKind::SetNodeProperty {
                target,
                path: Path::field(key),
                delta: WireDelta::LwwReplace { value },
            },
        ));
    }

    /// Set an edge property to a raw byte value (LWW).
    pub fn set_edge_property(&mut self, target: CrdtId, key: String, value: Vec<u8>) {
        let op_id = self.inner.mint_id();
        self.inner.pending_mut().push(Op::new(
            op_id,
            OpKind::SetRefEdgeProperty {
                target,
                path: Path::field(key),
                delta: WireDelta::LwwReplace { value },
            },
        ));
    }

    /// True iff there's a Move op in flight for `target`.
    #[must_use]
    pub fn is_pending_move_target(&self, target: CrdtId) -> bool {
        self.inner.topology().is_pending_move_target(target)
    }

    /// Read a node's tree parent.
    pub fn tree_parent(&self, id: CrdtId) -> Option<CrdtId> {
        self.inner.topology().tree_parent(id)
    }

    /// Read a node's order key.
    pub fn node_order_key(&self, id: CrdtId) -> Option<&str> {
        self.inner.topology().node_order_key(id)
    }

    /// Read an edge's endpoints.
    pub fn edge_endpoints(&self, id: CrdtId) -> Option<(CrdtId, CrdtId)> {
        self.inner.topology().edge_endpoints(id)
    }

    /// Read an edge's category.
    pub fn edge_category(&self, id: CrdtId) -> Option<&EdgeCategory> {
        self.inner.topology().edge_category(id)
    }

    /// Count of live nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.inner.topology().node_count()
    }

    /// Count of live edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.inner.topology().edge_count()
    }

    /// Iterate live outgoing edges.
    pub fn outgoing_edge_ids(&self, n: CrdtId) -> impl Iterator<Item = CrdtId> + '_ {
        self.inner.topology().outgoing_edge_ids(n)
    }

    /// Iterate live incoming edges.
    pub fn incoming_edge_ids(&self, n: CrdtId) -> impl Iterator<Item = CrdtId> + '_ {
        self.inner.topology().incoming_edge_ids(n)
    }

    /// Access a node's schema (read-only).
    ///
    /// Returns None if the node doesn't exist or is tombstoned.
    pub fn schema(&self, id: CrdtId) -> Option<&S> {
        if !self.inner.topology().is_live_node(id) {
            return None;
        }
        self.inner.schema(id)
    }

    /// Access a node's schema (mutable).
    ///
    /// Returns None if the node doesn't exist or is tombstoned.
    pub fn schema_mut(&mut self, id: CrdtId) -> Option<&mut S> {
        if !self.inner.topology().is_live_node(id) {
            return None;
        }
        self.inner.schema_mut(id)
    }

    /// Alias for `schema()` - access a node's properties.
    pub fn node(&self, id: CrdtId) -> Option<&S> {
        self.schema(id)
    }

    /// Alias for `schema_mut()` - access a node's properties (mutable).
    pub fn node_mut(&mut self, id: CrdtId) -> Option<&mut S> {
        self.schema_mut(id)
    }

    /// Apply a server-confirmed operation.
    pub fn apply_remote(&mut self, op: &Op<OpKind>) -> Result<(), ApplyError> {
        self.inner.apply_remote(op)
    }

    /// Snapshot the current state.
    pub fn snapshot(&self) -> kyoso_crdt::Snapshot<GraphTopology, S>
    where
        S: Send + Sync + Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.inner.snapshot()
    }

    /// Restore from a snapshot.
    pub fn restore(&mut self, snap: kyoso_crdt::Snapshot<GraphTopology, S>)
    where
        S: Send + Sync + Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.inner.restore(snap);
    }

    /// Drain pending ops for outbound transmission.
    pub fn drain_pending(&mut self) -> Vec<Op<OpKind>> {
        self.inner.drain_pending()
    }

    /// Get pending ops count.
    pub fn pending_len(&self) -> usize {
        self.inner.pending_len()
    }
}

// Implement Default
impl<S> Default for GraphBackend<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    fn default() -> Self {
        Self::with_peer(0)
    }
}

// Implement CrdtModel by delegating to inner backend
impl<S> CrdtModel for GraphBackend<S>
where
    S: Crdt + SchemaApply + Default + Send + Sync + Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    S::Delta: IntoWireOp,
{
    type OpKind = OpKind;
    type State = kyoso_crdt::Snapshot<GraphTopology, S>;

    fn set_peer(&mut self, peer: PeerId) {
        self.inner.set_peer(peer);
    }

    fn applied_seq(&self) -> GlobalSeq {
        self.inner.applied_seq()
    }

    fn apply_remote(&mut self, op: &Op<Self::OpKind>) -> Result<(), ApplyError> {
        self.inner.apply_remote(op)
    }

    fn snapshot(&self) -> Self::State {
        self.inner.snapshot()
    }

    fn restore(&mut self, snap: Self::State) {
        self.inner.restore(snap);
    }

    fn drain_pending(&mut self) -> Vec<Op<Self::OpKind>> {
        self.inner.drain_pending()
    }

    fn op_kind_label(op: &Op<Self::OpKind>) -> &'static str {
        GraphTopology::op_kind_label(&op.kind)
    }
}
