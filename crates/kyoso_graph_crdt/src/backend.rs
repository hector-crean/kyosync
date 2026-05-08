//! [`CrdtBackend`] — the graph CRDT replica.
//!
//! Local mutations (`add_node`, `remove_edge`, …) generate
//! [`Op`]`<OpKind>`s that accumulate in [`CrdtBackend::pending_ops`]. The
//! transport layer (`kyoso_server`) drains these, ships them to the server,
//! and feeds confirmed ops back via [`CrdtBackend::apply_remote`].
//!
//! # Scope of replication (v1)
//!
//! - Topology: nodes, edges (add/remove)
//! - Tree shape: `OrderKey`, `TreeParent`
//!
//! Node and edge *attribute* components on Bevy entities are **not**
//! replicated by this backend. Wire those up separately if you need them
//! synced — typically via observer systems that emit custom ops.
//!
//! # Tombstones
//!
//! Removed nodes/edges are tombstoned rather than dropped, so late ops
//! (`AddEdge` referencing a removed node) can be detected and skipped
//! deterministically. Tombstone GC is the server's responsibility once
//! all peers have ACK'd past the removal.

use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;

use kyoso_crdt::ApplyError;
use kyoso_crdt::delta::{Path, PathSegment, WireDelta};
use kyoso_crdt::id::{CrdtId, GlobalSeq, IdGenerator, PeerId};
use kyoso_crdt::op::Op;

use crate::edge_category::EdgeCategory;
use crate::op::OpKind;
use crate::snapshot::{EdgeSnap, NodeSnap, Snapshot};

/// Per-node bookkeeping inside the CRDT backend.
struct NodeRecord {
    tombstoned: bool,
    order_key: Option<String>,
    tree_parent: Option<CrdtId>,
    /// Last-applied serialized value per property key. Empty map until
    /// the first `SetNodeProperty` op for this node.
    properties: HashMap<String, Vec<u8>>,
}

/// Per-edge bookkeeping inside the CRDT backend.
struct EdgeRecord {
    from: CrdtId,
    to: CrdtId,
    /// Reference-edge category. Defaults to [`EdgeCategory::Reference`]
    /// for edges created via the [`GraphBackend`] trait; explicit
    /// categories are set via [`CrdtBackend::add_ref_edge_with_category`].
    category: EdgeCategory,
    tombstoned: bool,
    properties: HashMap<String, Vec<u8>>,
}

/// CRDT-replicated graph storage. Implements
/// [`GraphBackend`](kyoso_graph::backend::GraphBackend) so it plugs into
/// `Graph<N, E, CrdtBackend<N, E>>` without changing any of the consuming
/// systems.
pub struct CrdtBackend<N, E>
where
    N: Debug + Send + Sync + 'static,
    E: Debug + Send + Sync + 'static,
{
    id_gen: IdGenerator,
    nodes: HashMap<CrdtId, NodeRecord>,
    edges: HashMap<CrdtId, EdgeRecord>,
    /// Locally-generated ops that have not yet been confirmed by the
    /// server. The transport layer drains these and ships them upstream.
    pending: Vec<Op<OpKind>>,
    /// Highest server-confirmed [`GlobalSeq`] applied to this replica.
    applied_seq: GlobalSeq,
    _phantom: PhantomData<(N, E)>,
}

impl<N, E> Default for CrdtBackend<N, E>
where
    N: Debug + Send + Sync + 'static,
    E: Debug + Send + Sync + 'static,
{
    fn default() -> Self {
        // Peer 0 is a placeholder; production code calls `set_peer` once
        // session auth assigns a real peer id.
        Self::with_peer(0)
    }
}

impl<N, E> CrdtBackend<N, E>
where
    N: Debug + Send + Sync + 'static,
    E: Debug + Send + Sync + 'static,
{
    pub fn with_peer(peer: PeerId) -> Self {
        Self {
            id_gen: IdGenerator::new(peer),
            nodes: HashMap::new(),
            edges: HashMap::new(),
            pending: Vec::new(),
            applied_seq: 0,
            _phantom: PhantomData,
        }
    }

    pub fn peer(&self) -> PeerId {
        self.id_gen.peer()
    }

    /// Re-key the id generator under a new peer id. Only meaningful before
    /// any mutations have been issued — existing pending ops keep their
    /// original peer.
    pub fn set_peer(&mut self, peer: PeerId) {
        self.id_gen = IdGenerator::new(peer);
    }

    pub fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq
    }

    /// Mint a fresh op-id from this backend's [`IdGenerator`]. Used by
    /// external producers (typed-schema plugins, custom op flows) that
    /// want to ride the same id-generation namespace as the backend
    /// itself.
    pub fn next_id(&mut self) -> CrdtId {
        self.id_gen.next()
    }

    /// Push a fully-formed [`Op`] onto the pending queue. The
    /// transport layer drains [`drain_pending`](Self::drain_pending)
    /// each tick.
    pub fn enqueue(&mut self, op: Op<OpKind>) {
        self.pending.push(op);
    }

    /// Read the live `OrderKey` for `id` (or `None` if the node is
    /// unknown / has no key set / is tombstoned). Used by the ECS sync
    /// layer to detect remote-driven OrderKey writes and avoid echoing
    /// them as local mutations.
    pub fn node_order_key(&self, id: CrdtId) -> Option<&str> {
        let rec = self.nodes.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        rec.order_key.as_deref()
    }

    /// Read the from/to endpoints of a live edge. `None` if unknown or
    /// tombstoned.
    pub fn edge_endpoints(&self, id: CrdtId) -> Option<(CrdtId, CrdtId)> {
        let rec = self.edges.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        Some((rec.from, rec.to))
    }

    /// Last-applied serialized value for a single node property. `None`
    /// if unknown, tombstoned, or no op has set this key.
    pub fn node_property(&self, id: CrdtId, key: &str) -> Option<&[u8]> {
        let rec = self.nodes.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        rec.properties.get(key).map(|v| v.as_slice())
    }

    /// Same shape as [`node_property`](Self::node_property) for edges.
    pub fn edge_property(&self, id: CrdtId, key: &str) -> Option<&[u8]> {
        let rec = self.edges.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        rec.properties.get(key).map(|v| v.as_slice())
    }

    /// Stamp `value` as the LWW state of `target.key` and emit a
    /// `SetNodeProperty` op. Consumers produce `value` via their own
    /// codec (Bevy `ReflectSerializer` + `postcard` is the convention).
    ///
    /// `key` is a single-segment field name; for nested paths use the
    /// schema layer ([`Document`](crate::document::Document)) directly.
    /// Internally wraps `value` as a [`WireDelta::LwwReplace`].
    pub fn set_node_property(&mut self, target: CrdtId, key: String, value: Vec<u8>) {
        if let Some(rec) = self.nodes.get_mut(&target) {
            rec.properties.insert(key.clone(), value.clone());
        }
        let op_id = self.id_gen.next();
        self.pending.push(Op::new(
            op_id,
            OpKind::SetNodeProperty {
                target,
                path: Path::field(key),
                delta: WireDelta::LwwReplace { value },
            },
        ));
    }

    /// Same shape as [`set_node_property`](Self::set_node_property) for edges.
    pub fn set_edge_property(&mut self, target: CrdtId, key: String, value: Vec<u8>) {
        if let Some(rec) = self.edges.get_mut(&target) {
            rec.properties.insert(key.clone(), value.clone());
        }
        let op_id = self.id_gen.next();
        self.pending.push(Op::new(
            op_id,
            OpKind::SetRefEdgeProperty {
                target,
                path: Path::field(key),
                delta: WireDelta::LwwReplace { value },
            },
        ));
    }

    /// Create a typed reference edge with explicit category. Returns the
    /// edge's [`CrdtId`]. The plain [`GraphBackend::add_edge`] path uses
    /// [`EdgeCategory::Reference`] as the default; this method is for
    /// callers that have a more specific category in mind (e.g. a
    /// `prototype_link` between two frames).
    pub fn add_ref_edge_with_category(
        &mut self,
        from: CrdtId,
        to: CrdtId,
        category: EdgeCategory,
    ) -> CrdtId {
        let id = self.id_gen.next();
        self.edges.insert(
            id,
            EdgeRecord {
                from,
                to,
                category: category.clone(),
                tombstoned: false,
                properties: HashMap::new(),
            },
        );
        self.pending.push(Op::new(
            id,
            OpKind::AddRefEdge {
                category,
                from,
                to,
            },
        ));
        id
    }

    /// Read a live edge's category. `None` if unknown or tombstoned.
    #[must_use]
    pub fn edge_category(&self, id: CrdtId) -> Option<&EdgeCategory> {
        let rec = self.edges.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        Some(&rec.category)
    }

    /// Materialise current state as a [`Snapshot`] (tombstone-free, at
    /// `applied_seq`). The server uses this to checkpoint rooms; clients
    /// receive snapshots in [`ServerMsg::Welcome`](kyoso_crdt::ServerMsg) and
    /// [`restore`](Self::restore) from them.
    pub fn snapshot(&self) -> Snapshot {
        let nodes = self
            .nodes
            .iter()
            .filter(|(_, rec)| !rec.tombstoned)
            .map(|(id, rec)| NodeSnap {
                id: *id,
                order_key: rec.order_key.clone(),
                tree_parent: rec.tree_parent,
                properties: rec.properties.clone(),
            })
            .collect();
        let edges = self
            .edges
            .iter()
            .filter(|(_, rec)| !rec.tombstoned)
            .map(|(id, rec)| EdgeSnap {
                id: *id,
                from: rec.from,
                to: rec.to,
                category: rec.category.clone(),
                properties: rec.properties.clone(),
            })
            .collect();
        Snapshot {
            at_seq: self.applied_seq,
            nodes,
            edges,
        }
    }

    /// Replace local state with `snap`. The id generator is bumped past
    /// any id this peer minted that appears in the snapshot, preventing
    /// collisions on next [`add_node`](Self::add_node).
    pub fn restore(&mut self, snap: Snapshot) {
        self.nodes.clear();
        self.edges.clear();
        self.applied_seq = snap.at_seq;

        let my_peer = self.id_gen.peer();
        let mut max_my_seq: Option<kyoso_crdt::id::LocalSeq> = None;
        let bump = |id: CrdtId, max: &mut Option<kyoso_crdt::id::LocalSeq>| {
            if id.peer == my_peer {
                *max = Some(max.map_or(id.seq, |s| s.max(id.seq)));
            }
        };
        for n in &snap.nodes {
            bump(n.id, &mut max_my_seq);
        }
        for e in &snap.edges {
            bump(e.id, &mut max_my_seq);
        }
        if let Some(seq) = max_my_seq {
            self.id_gen = IdGenerator::resume(my_peer, seq + 1);
        }

        for n in snap.nodes {
            self.nodes.insert(
                n.id,
                NodeRecord {
                    tombstoned: false,
                    order_key: n.order_key,
                    tree_parent: n.tree_parent,
                    properties: n.properties,
                },
            );
        }
        for e in snap.edges {
            self.edges.insert(
                e.id,
                EdgeRecord {
                    from: e.from,
                    to: e.to,
                    category: e.category,
                    tombstoned: false,
                    properties: e.properties,
                },
            );
        }
    }

    /// Atomic Kleppmann move. Sets `target`'s tree-parent + order-key
    /// in one op and emits an [`OpKind::Move`]. Local apply runs the
    /// same cycle check as [`apply_remote`](Self::apply_remote); if the
    /// move would create a cycle it is dropped (no-op) and no op is
    /// appended to `pending`.
    ///
    /// Returns `true` if the move was accepted locally. `false` means
    /// the proposed parentage cycles; the caller may choose to surface
    /// an error to the user.
    pub fn move_node(
        &mut self,
        target: CrdtId,
        new_parent: Option<CrdtId>,
        position: String,
    ) -> bool {
        if let Some(parent_id) = new_parent {
            if self.would_create_cycle(target, parent_id) {
                return false;
            }
        }
        if let Some(rec) = self.nodes.get_mut(&target) {
            rec.tree_parent = new_parent;
            rec.order_key = Some(position.clone());
        }
        let op_id = self.id_gen.next();
        self.pending.push(Op::new(
            op_id,
            OpKind::Move {
                target,
                new_parent,
                position,
            },
        ));
        true
    }

    /// Read a node's current tree parent (`None` for root or unknown).
    pub fn tree_parent(&self, id: CrdtId) -> Option<CrdtId> {
        let rec = self.nodes.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        rec.tree_parent
    }

    /// Drain locally-generated ops awaiting upstream confirmation. The
    /// transport layer calls this each tick.
    pub fn drain_pending(&mut self) -> Vec<Op<OpKind>> {
        std::mem::take(&mut self.pending)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Apply a server-confirmed op to local state.
    ///
    /// Idempotent: applying the same `seq` twice is a no-op. Out-of-order
    /// applies (a gap in `seq`) return [`ApplyError::OutOfOrder`].
    pub fn apply_remote(&mut self, op: &Op<OpKind>) -> Result<(), ApplyError> {
        let seq = op.seq.ok_or(ApplyError::Unconfirmed)?;
        if seq <= self.applied_seq {
            return Ok(()); // already applied
        }
        if seq != self.applied_seq + 1 {
            return Err(ApplyError::OutOfOrder {
                expected: self.applied_seq + 1,
                got: seq,
            });
        }

        match &op.kind {
            OpKind::AddNode => {
                self.nodes.entry(op.id).or_insert(NodeRecord {
                    tombstoned: false,
                    order_key: None,
                    tree_parent: None,
                    properties: HashMap::new(),
                });
            }
            OpKind::AddRefEdge {
                category,
                from,
                to,
            } => {
                self.edges.entry(op.id).or_insert(EdgeRecord {
                    from: *from,
                    to: *to,
                    category: category.clone(),
                    tombstoned: false,
                    properties: HashMap::new(),
                });
            }
            OpKind::SetNodeProperty { target, path, delta } => {
                if let Some(rec) = self.nodes.get_mut(target) {
                    if let Some(key) = path_to_legacy_key(path) {
                        apply_property_delta(&mut rec.properties, &key, delta);
                    }
                }
            }
            OpKind::SetRefEdgeProperty { target, path, delta } => {
                if let Some(rec) = self.edges.get_mut(target) {
                    if let Some(key) = path_to_legacy_key(path) {
                        apply_property_delta(&mut rec.properties, &key, delta);
                    }
                }
            }
            OpKind::RemoveNode { target } => {
                if let Some(rec) = self.nodes.get_mut(target) {
                    rec.tombstoned = true;
                    // Tombstone all incident edges deterministically.
                    for edge in self.edges.values_mut() {
                        if edge.from == *target || edge.to == *target {
                            edge.tombstoned = true;
                        }
                    }
                }
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
                // Cycle check: walk the proposed parent chain. If we
                // hit `target` we'd create a cycle — drop the op
                // entirely (still advance applied_seq, since this is
                // the deterministic decision every replica reaches).
                if let Some(parent_id) = new_parent {
                    if self.would_create_cycle(*target, *parent_id) {
                        self.applied_seq = seq;
                        return Ok(());
                    }
                }
                if let Some(rec) = self.nodes.get_mut(target) {
                    rec.tree_parent = *new_parent;
                    rec.order_key = Some(position.clone());
                }
            }
        }

        self.applied_seq = seq;
        Ok(())
    }

    /// Mint a new node, queue an [`OpKind::AddNode`] op, and return its id.
    pub fn add_node(&mut self) -> CrdtId {
        let id = self.id_gen.next();
        self.nodes.insert(
            id,
            NodeRecord {
                tombstoned: false,
                order_key: None,
                tree_parent: None,
                properties: HashMap::new(),
            },
        );
        self.pending.push(Op::new(id, OpKind::AddNode));
        id
    }

    /// Tombstone a node and queue an [`OpKind::RemoveNode`] op.
    /// Returns `false` if the node is unknown or already tombstoned.
    pub fn remove_node(&mut self, n: CrdtId) -> bool {
        let Some(rec) = self.nodes.get_mut(&n) else {
            return false;
        };
        if rec.tombstoned {
            return false;
        }
        rec.tombstoned = true;
        // Cascade-tombstone incident edges.
        for edge in self.edges.values_mut() {
            if (edge.from == n || edge.to == n) && !edge.tombstoned {
                edge.tombstoned = true;
            }
        }
        let op_id = self.id_gen.next();
        self.pending
            .push(Op::new(op_id, OpKind::RemoveNode { target: n }));
        true
    }

    /// Mint a new edge with [`EdgeCategory::Reference`], queue an
    /// [`OpKind::AddRefEdge`] op, and return the edge's id. For typed
    /// categories use [`Self::add_ref_edge_with_category`].
    pub fn add_edge(&mut self, from: CrdtId, to: CrdtId) -> CrdtId {
        self.add_ref_edge_with_category(from, to, EdgeCategory::Reference)
    }

    /// Tombstone an edge and queue an [`OpKind::RemoveRefEdge`] op.
    /// Returns `false` if the edge is unknown or already tombstoned.
    pub fn remove_edge(&mut self, e: CrdtId) -> bool {
        let Some(rec) = self.edges.get_mut(&e) else {
            return false;
        };
        if rec.tombstoned {
            return false;
        }
        rec.tombstoned = true;
        let op_id = self.id_gen.next();
        self.pending
            .push(Op::new(op_id, OpKind::RemoveRefEdge { target: e }));
        true
    }

    /// True iff making `proposed_parent` the new parent of `target`
    /// would form a cycle. Walks the chain `proposed_parent -> ...`
    /// looking for `target`.
    fn would_create_cycle(&self, target: CrdtId, proposed_parent: CrdtId) -> bool {
        if target == proposed_parent {
            return true;
        }
        let mut cursor = Some(proposed_parent);
        while let Some(id) = cursor {
            if id == target {
                return true;
            }
            cursor = self
                .nodes
                .get(&id)
                .and_then(|rec| if rec.tombstoned { None } else { rec.tree_parent });
        }
        false
    }
}

/// Apply a [`WireDelta`] to a flat property bag.
///
/// `CrdtBackend` only knows the LWW path natively (it stores the latest
/// raw bytes per key). Richer CRDT semantics live in the schema layer
/// ([`crate::document::Document`]); other [`WireDelta`] variants are
/// accepted on the wire but ignored here.
fn apply_property_delta(props: &mut HashMap<String, Vec<u8>>, key: &str, delta: &WireDelta) {
    match delta {
        WireDelta::LwwReplace { value } => {
            props.insert(key.to_string(), value.clone());
        }
        WireDelta::OrSetAdd { .. }
        | WireDelta::OrSetRemove { .. }
        | WireDelta::PnCounterDelta { .. }
        | WireDelta::SequenceInsert { .. }
        | WireDelta::SequenceDelete { .. }
        | WireDelta::MapPut { .. }
        | WireDelta::MapRemove { .. } => {}
    }
}

/// Reduce a [`Path`] to a single legacy key string.
///
/// `CrdtBackend` predates multi-segment paths and stores LWW property
/// values keyed by string. Single-segment paths (the common case)
/// become that segment's name; multi-segment paths are joined with `/`
/// for ergonomic logging — but only the LWW dispatch in
/// [`apply_property_delta`] uses this. The schema layer is responsible
/// for actually walking the path; the backend's property bag is
/// LWW-only.
fn path_to_legacy_key(path: &Path) -> Option<String> {
    if path.0.is_empty() {
        return None;
    }
    Some(
        path.0
            .iter()
            .map(|s| match s {
                PathSegment::Field(n) | PathSegment::Key(n) => n.as_str(),
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}

// ---------------------------------------------------------------------------
// Inherent topology counters (formerly on the `GraphBackend` trait,
// removed in Part IV §IV.2 Step 5).
// ---------------------------------------------------------------------------

impl<N, E> CrdtBackend<N, E>
where
    N: Debug + Send + Sync + 'static,
    E: Debug + Send + Sync + 'static,
{
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
}

// ---------------------------------------------------------------------------
// CrdtModel impl — plugs CrdtBackend into the framework's model registry.
// ---------------------------------------------------------------------------

impl<N, E> kyoso_crdt::model::CrdtModel for CrdtBackend<N, E>
where
    N: Debug + Send + Sync + 'static,
    E: Debug + Send + Sync + 'static,
{
    type OpKind = OpKind;
    type State = Snapshot;

    fn set_peer(&mut self, peer: PeerId) {
        Self::set_peer(self, peer);
    }

    fn applied_seq(&self) -> GlobalSeq {
        Self::applied_seq(self)
    }

    fn apply_remote(&mut self, op: &Op<OpKind>) -> Result<(), ApplyError> {
        Self::apply_remote(self, op)
    }

    fn snapshot(&self) -> Snapshot {
        Self::snapshot(self)
    }

    fn restore(&mut self, snap: Snapshot) {
        Self::restore(self, snap);
    }

    fn drain_pending(&mut self) -> Vec<Op<OpKind>> {
        Self::drain_pending(self)
    }

    fn op_kind_label(op: &Op<OpKind>) -> &'static str {
        match &op.kind {
            OpKind::AddNode => "AddNode",
            OpKind::AddRefEdge { .. } => "AddRefEdge",
            OpKind::RemoveNode { .. } => "RemoveNode",
            OpKind::RemoveRefEdge { .. } => "RemoveRefEdge",
            OpKind::SetNodeProperty { .. } => "SetNodeProperty",
            OpKind::SetRefEdgeProperty { .. } => "SetRefEdgeProperty",
            OpKind::Move { .. } => "Move",
        }
    }
}
