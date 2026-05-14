//! Generic CRDT backend: `Backend<T, S>`.
//!
//! Composes domain-specific [`Topology`] (structural operations like
//! AddNode, Move, AddEdge) with domain-agnostic [`Crdt`] properties
//! (LWW, OR-Set, PN-Counter fields on entities).
//!
//! This is the foundation for all CRDT models in kyoso:
//! - Graph: `type GraphBackend<S> = Backend<GraphTopology, S>`
//! - Canvas: `type CanvasBackend<S> = Backend<CanvasTopology, S>`
//! - Document: `type DocumentBackend<S> = Backend<DocumentTopology, S>`
//!
//! Each domain implements [`Topology`] for its structure, then the
//! backend handles identity, op log, snapshot/restore, and schema
//! routing uniformly.

use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;

use crate::context::{CausalContext, CausalState};
use crate::id::{CrdtId, GlobalSeq, IdGen, PeerId};
use crate::lattice::{Crdt, DeltaError};
use crate::model::{ApplyError, CrdtModel};
use crate::op::Op;
use crate::schema::{IntoWireOp, SchemaApply};
use crate::topology::Topology;

/// Generic CRDT backend: structure + properties.
///
/// `T` is the domain-specific topology (graph, canvas, document).
/// `S` is the per-entity property schema (FrameSchema, StrokeSchema, etc.).
///
/// The backend owns:
/// - Identity generation ([`IdGen`] shared across models)
/// - Topology state (nodes, edges, tree, z-order, grid, etc.)
/// - Property schemas (one `S` per entity)
/// - Op log (pending ops awaiting ack)
/// - Causal context (for SubDot allocation)
///
/// Mutating methods queue ops in `pending` without pre-applying (echo-wait
/// pattern). Apply happens only when the server echo arrives with a stamped
/// `GlobalSeq`.
pub struct Backend<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    /// Shared ID source. Clone this handle to other CRDT models on the
    /// same peer so cross-model references stay collision-free.
    ids: IdGen,

    /// Domain-specific structural state (nodes, edges, tree, etc.)
    topology: T,

    /// Per-entity property schemas. The `CrdtId` key is the entity ID
    /// (node, edge, stroke, cell, etc.). The value is a typed CRDT schema
    /// holding LWW/OR-Set/PN-Counter fields.
    ///
    /// For entities that have no properties (e.g., server mirrors), use
    /// `EmptySchema` (zero fields).
    schemas: HashMap<CrdtId, S>,

    /// Locally-generated ops awaiting server confirmation. The outbound
    /// system drains these and ships them to the server. They are *not*
    /// pre-applied — the backend's authoritative state updates only on
    /// confirmed echo (see module docs for rationale).
    pending: Vec<Op<T::OpKind>>,

    /// Highest server-confirmed [`GlobalSeq`] applied to this replica.
    /// Used to validate apply order (ops must arrive with seq = applied_seq + 1).
    applied_seq: GlobalSeq,

    /// Causal context state for SubDot allocation. Embedded CRDTs
    /// (OR-Set, PN-Counter) allocate fresh SubDots from here during
    /// `mutate` and `apply`.
    causal: CausalState,

    _phantom: PhantomData<(T, S)>,
}

impl<T, S> Default for Backend<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    fn default() -> Self {
        Self::with_peer(0)
    }
}

impl<T, S> Backend<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    /// Construct with a fresh, owned [`IdGen`] handle.
    ///
    /// Convenient for single-model use (server-side mirrors, tests).
    /// For multi-model peers (graph + comments sharing one counter),
    /// use [`Self::with_shared_ids`].
    pub fn with_peer(peer: PeerId) -> Self {
        Self::with_shared_ids(IdGen::new(peer))
    }

    /// Construct sharing `ids` with other CRDT models on the same peer.
    ///
    /// Cloning `ids` and passing it to each backend is how cross-model
    /// references stay collision-free (graph node ID can safely be
    /// referenced by comment anchor ID).
    pub fn with_shared_ids(ids: IdGen) -> Self {
        Self {
            ids,
            topology: T::default(),
            schemas: HashMap::new(),
            pending: Vec::new(),
            applied_seq: 0,
            causal: CausalState::new(),
            _phantom: PhantomData,
        }
    }

    pub fn peer(&self) -> PeerId {
        self.ids.peer()
    }

    /// Re-key the ID generator under a new peer ID.
    ///
    /// Only meaningful before any mutations have been issued — existing
    /// pending ops keep their original peer. **Visible to every clone
    /// of the shared [`IdGen`]**: when this backend shares its handle
    /// with other models on the same peer, all of them see the new peer.
    pub fn set_peer(&mut self, peer: PeerId) {
        self.ids.set_peer(peer);
    }

    pub fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq
    }

    /// Cloneable handle to this backend's ID source.
    ///
    /// Hand a clone to other CRDT models on the same peer so their
    /// minted IDs share the per-peer `LocalSeq` namespace.
    pub fn ids(&self) -> &IdGen {
        &self.ids
    }

    /// Mint a fresh op ID from this backend's ID source.
    ///
    /// Used by external producers (typed-schema plugins, custom op
    /// flows) that want to ride the same ID-generation namespace as
    /// the backend's structural ops.
    pub fn mint_id(&mut self) -> CrdtId {
        self.ids.next()
    }

    /// Access the underlying topology (read-only).
    ///
    /// Useful for domain-specific queries (traverse tree, find edges,
    /// get z-order, etc.) that aren't part of the generic backend API.
    pub fn topology(&self) -> &T {
        &self.topology
    }

    /// Access a mutable reference to the topology.
    ///
    /// **Caution**: Direct topology mutations bypass the op log. Only
    /// use this for initialization or domain-specific helpers that
    /// queue their own ops.
    pub fn topology_mut(&mut self) -> &mut T {
        &mut self.topology
    }

    /// Access all schemas (read-only).
    pub fn schemas(&self) -> &HashMap<CrdtId, S> {
        &self.schemas
    }

    /// Access a specific entity's schema (read-only).
    ///
    /// Returns `None` if the entity doesn't exist or was tombstoned.
    pub fn schema(&self, id: CrdtId) -> Option<&S> {
        self.schemas.get(&id)
    }

    /// Access a specific entity's schema (mutable).
    ///
    /// Returns `None` if the entity doesn't exist. **Caution**: direct
    /// schema mutations bypass the op log. Only use this for
    /// initialization or when you're queuing your own SetProperty ops.
    pub fn schema_mut(&mut self, id: CrdtId) -> Option<&mut S> {
        self.schemas.get_mut(&id)
    }

    /// Ensure a schema entry exists for the given entity.
    ///
    /// Creates a default schema if one doesn't exist. Used by add_node
    /// to pre-insert the schema entry so subsequent mutate_node calls
    /// work before the server echo arrives.
    pub fn ensure_schema(&mut self, id: CrdtId) {
        self.schemas.entry(id).or_insert_with(S::default);
    }

    /// Access the pending ops queue (mutable).
    ///
    /// **Caution**: Directly pushing ops bypasses the normal queue_op
    /// flow. Only use this when implementing domain-specific mutation
    /// methods that need full control over op construction.
    pub fn pending_mut(&mut self) -> &mut Vec<Op<T::OpKind>> {
        &mut self.pending
    }

    /// Access the causal state (mutable).
    ///
    /// **Caution**: Only use this when implementing domain-specific
    /// mutation methods that need to create CausalContext instances
    /// for schema mutations.
    pub fn causal_state_mut(&mut self) -> &mut CausalState {
        &mut self.causal
    }

    /// Mutate a schema with a fresh op ID and context.
    ///
    /// This is a helper for domain-specific mutation methods that avoids
    /// borrow-checker issues when creating a CausalContext and accessing
    /// the schema simultaneously.
    ///
    /// Returns `(op_id, delta)` if the entity exists, `None` otherwise.
    pub fn mutate_schema_with_context(
        &mut self,
        target: CrdtId,
        mutation: S::Mutation,
    ) -> Option<(CrdtId, S::Delta)> {
        let schema = self.schemas.get_mut(&target)?;
        let op_id = self.ids.next();
        let mut ctx = CausalContext::new(op_id, None, &mut self.causal);
        let delta = schema.mutate(mutation, &mut ctx);
        Some((op_id, delta))
    }

    /// Queue an op for server confirmation.
    ///
    /// The op is added to `pending` but **not pre-applied** to the
    /// backend's authoritative state (except for property mutations, which
    /// can be pre-applied separately via schema methods). Structural ops
    /// only update state when the server echo arrives via
    /// [`apply_remote`](Self::apply_remote).
    ///
    /// Returns the minted op ID (useful for tracking in-flight ops).
    pub fn queue_op(&mut self, op_kind: T::OpKind) -> CrdtId {
        let id = self.ids.next();
        self.pending.push(Op {
            id,
            seq: None,
            kind: op_kind,
        });
        id
    }


    /// Drain pending ops for outbound transmission.
    ///
    /// The transport layer calls this each tick to ship locally-generated
    /// ops to the server. Once drained, `pending` is empty until the next
    /// local mutation.
    pub fn drain_pending(&mut self) -> Vec<Op<T::OpKind>> {
        std::mem::take(&mut self.pending)
    }

    /// Get the number of pending ops waiting to be drained.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Apply a server-confirmed op.
    ///
    /// Validates the op's sequence number matches `applied_seq + 1`,
    /// then dispatches to either:
    /// - Structural op → `topology.apply_structural_op`
    /// - Property op → `schemas[target].apply_wire`
    ///
    /// This is the **only** path that updates the backend's authoritative
    /// state. Local mutations queue in `pending` but don't update state
    /// until the echo arrives here.
    ///
    /// Returns `Err(ApplyError::OutOfOrder)` if the seq doesn't match.
    /// Returns `Err(ApplyError::Unconfirmed)` if `op.seq` is `None`.
    pub fn apply_remote(&mut self, op: &Op<T::OpKind>) -> Result<(), ApplyError> {
        let seq = op.seq.ok_or(ApplyError::Unconfirmed)?;

        // Idempotency: if we've already applied this seq, it's a no-op
        if seq <= self.applied_seq {
            return Ok(());
        }

        // Ordering: seq must be exactly applied_seq + 1
        if seq != self.applied_seq + 1 {
            return Err(ApplyError::OutOfOrder {
                expected: self.applied_seq + 1,
                got: seq,
            });
        }

        let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);

        // Route: property op or structural op?
        if let Some(prop_op) = T::extract_property_op(&op.kind) {
            // Property mutation: route to schema
            let schema = self
                .schemas
                .entry(prop_op.target)
                .or_insert_with(S::default);
            // Ignore schema apply errors - malformed deltas shouldn't happen
            // in well-formed ops, but if they do, we can safely skip them
            // without breaking convergence (the op is still "applied" in
            // terms of sequence number advancement).
            let _ = schema.apply_wire(&prop_op.path, prop_op.delta, &ctx);
        } else {
            // Structural op: route to topology
            self.topology.apply_structural_op(&op.kind, &ctx);

            // If this op creates a new entity, insert a default schema
            if let Some(new_id) = T::extract_new_entity_id(&op.kind, &ctx) {
                self.schemas.entry(new_id).or_insert_with(S::default);
            }
        }

        self.applied_seq = seq;
        Ok(())
    }

    /// Apply only the property portion of an op directly to the schema,
    /// bypassing `GlobalSeq` validation.
    ///
    /// Used by per-schema secondary stores (e.g. typed-schema sync
    /// plugins that mirror property ops already applied to a primary
    /// backend). Structural ops are silently ignored.
    pub fn apply_property_op(&mut self, op: &Op<T::OpKind>) -> Result<(), DeltaError> {
        let Some(prop_op) = T::extract_property_op(&op.kind) else {
            return Ok(());
        };
        let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
        let schema = self
            .schemas
            .entry(prop_op.target)
            .or_insert_with(S::default);
        schema.apply_wire(&prop_op.path, prop_op.delta, &ctx)
    }

    /// Snapshot the backend's current state.
    ///
    /// Returns a compacted representation of:
    /// - Structural topology (tree, edges, z-order, etc.) via `T::snapshot`
    /// - Property schemas per entity (only live entities)
    ///
    /// Tombstones are excluded. Late joiners hydrate from this snapshot
    /// plus ops since `at_seq`.
    pub fn snapshot(&self) -> Snapshot<T, S> {
        Snapshot {
            at_seq: self.applied_seq,
            topology: self.topology.snapshot(),
            // HashMap → BTreeMap so the encoded snapshot bytes are
            // deterministic across calls (HashMap iteration order
            // varies per process).
            schemas: self.schemas.iter().map(|(k, v)| (*k, v.clone())).collect(),
        }
    }

    /// Restore from a snapshot.
    ///
    /// Clears current state and replaces with the snapshot. The backend's
    /// `IdGen` is bumped past the highest local seq in the snapshot to
    /// prevent ID collisions with newly-minted IDs.
    pub fn restore(&mut self, snap: Snapshot<T, S>) {
        self.applied_seq = snap.at_seq;
        self.topology.restore(snap.topology);
        self.schemas = snap.schemas.into_iter().collect();

        // Bump IdGen past any IDs in the snapshot that belong to this peer
        let my_peer = self.ids.peer();
        let max_local_seq = self
            .schemas
            .keys()
            .filter(|id| id.peer == my_peer)
            .map(|id| id.seq)
            .max();
        if let Some(seq) = max_local_seq {
            self.ids.bump_to(seq + 1);
        }
    }
}

/// Snapshot format for `Backend<T, S>`.
///
/// Contains both structural state (topology) and property state (schemas).
/// Excludes tombstones and pending ops.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "T: Topology, S: serde::Serialize",
    deserialize = "T: Topology, S: serde::de::DeserializeOwned"
))]
pub struct Snapshot<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default,
{
    /// Sequence number this snapshot represents.
    pub at_seq: GlobalSeq,
    /// Structural topology state (tree, edges, z-order, etc.)
    pub topology: T::SnapshotState,
    /// Per-entity property schemas (only live entities). `BTreeMap`
    /// rather than `HashMap` so postcard encoding is deterministic —
    /// two snapshots over equal state encode to identical bytes,
    /// which is what enables snapshot equality by hash (e.g. for the
    /// agent harness's encode/decode round-trip proptest).
    pub schemas: std::collections::BTreeMap<CrdtId, S>,
}

// Manual Clone impl to avoid requiring T: Clone (we only need T::SnapshotState: Clone)
impl<T, S> Clone for Snapshot<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default + Clone,
{
    fn clone(&self) -> Self {
        Self {
            at_seq: self.at_seq,
            topology: self.topology.clone(),
            schemas: self.schemas.clone(),
        }
    }
}

// Manual PartialEq for chaos-sim convergence comparisons.
impl<T, S> PartialEq for Snapshot<T, S>
where
    T: Topology,
    T::SnapshotState: PartialEq,
    S: Crdt + SchemaApply + Default + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.at_seq == other.at_seq
            && self.topology == other.topology
            && self.schemas == other.schemas
    }
}

impl<T, S> Snapshot<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default,
{
    /// Encode this snapshot to postcard bytes.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error>
    where
        S: serde::Serialize,
    {
        postcard::to_allocvec(self)
    }

    /// Decode a snapshot from postcard bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error>
    where
        S: serde::de::DeserializeOwned,
    {
        postcard::from_bytes(bytes)
    }

    /// Check if this snapshot is empty (no entities).
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }
}

// Implement CrdtModel for Backend<T, S>
impl<T, S> CrdtModel for Backend<T, S>
where
    T: Topology,
    S: Crdt + SchemaApply + Default + Send + Sync + Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    S::Delta: IntoWireOp,
{
    type OpKind = T::OpKind;
    type State = Snapshot<T, S>;

    fn set_peer(&mut self, peer: PeerId) {
        self.set_peer(peer);
    }

    fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq()
    }

    fn apply_remote(&mut self, op: &Op<Self::OpKind>) -> Result<(), ApplyError> {
        Backend::apply_remote(self, op)
    }

    fn snapshot(&self) -> Self::State {
        Backend::snapshot(self)
    }

    fn restore(&mut self, snap: Self::State) {
        Backend::restore(self, snap);
    }

    fn drain_pending(&mut self) -> Vec<Op<Self::OpKind>> {
        Backend::drain_pending(self)
    }

    fn op_kind_label(op: &Op<Self::OpKind>) -> &'static str {
        T::op_kind_label(&op.kind)
    }
}

/// Empty schema for structure-only backends (server mirrors, tests).
///
/// Has zero fields, so no property mutations are possible. Use this
/// when you only need topology (nodes, edges, tree) without properties.
///
/// Example: `type StructuralGraph = Backend<GraphTopology, EmptySchema>;`
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmptySchema;

// Manual trait impls for EmptySchema (derive macro would generate nothing)
use crate::lattice::{Crdt as CrdtTrait, Lattice};

impl Lattice for EmptySchema {
    fn bottom() -> Self {
        Self
    }
    fn join(&mut self, _other: Self) {}
}

#[derive(Clone, Debug, PartialEq)]
pub enum EmptySchemaMut {}

#[derive(Clone, Debug, PartialEq)]
pub enum EmptySchemaDelta {}

impl CrdtTrait for EmptySchema {
    type Mutation = EmptySchemaMut;
    type Delta = EmptySchemaDelta;

    fn apply(&mut self, _delta: &Self::Delta, _ctx: &CausalContext) -> Result<(), DeltaError> {
        // Delta is uninhabited (no variants), so this is unreachable
        Ok(())
    }

    fn mutate(
        &mut self,
        _m: Self::Mutation,
        _ctx: &mut CausalContext,
    ) -> Self::Delta {
        // Mutation is uninhabited, unreachable
        unreachable!("EmptySchema has no mutations")
    }
}

impl SchemaApply for EmptySchema {
    fn apply_wire(
        &mut self,
        _path: &crate::delta::Path,
        _delta: crate::delta::WireDelta,
        _ctx: &CausalContext,
    ) -> Result<(), DeltaError> {
        // No fields to apply to
        Ok(())
    }

    fn install_state(
        &mut self,
        _path: &crate::delta::Path,
        _field: crate::opaque::OpaqueValue,
    ) -> Result<(), DeltaError> {
        // No fields to install
        Ok(())
    }
}

impl IntoWireOp for EmptySchemaDelta {
    fn into_wire_op(self) -> (crate::delta::Path, crate::delta::WireDelta) {
        // Uninhabited, unreachable
        unreachable!("EmptySchema has no deltas")
    }
}
