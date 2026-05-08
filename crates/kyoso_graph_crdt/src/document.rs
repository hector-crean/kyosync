//! Schema-aware graph document wrapper.
//!
//! [`Document<S>`] is the typed companion to [`crate::CrdtBackend`]: it
//! carries a static [`NodeSchema`](kyoso_crdt::Crdt) instance per node,
//! dispatches incoming [`WireDelta`](kyoso_crdt::WireDelta)s to the right
//! field via [`SchemaApply`](kyoso_crdt::SchemaApply), and produces typed
//! mutations that travel as [`OpKind::SetNodeProperty`] on the wire.
//!
//! Phase H ships this as an *additional* layer over [`crate::CrdtBackend`]
//! — existing reflection-driven sync paths in `kyoso_sync` continue to
//! work; new code that wants compile-time-checked schemas adopts
//! `Document<S>` directly. A future phase rewires `kyoso_sync` itself
//! to drop the reflection path.
//!
//! ## Wire-format mapping
//!
//! Each typed mutation flows:
//!
//! ```text
//!   user mutation  ─►  S::Mutation  ─► S::Delta  ─► (Path, WireDelta)
//!                                                        │
//!                                                        ▼
//!                                          OpKind::SetNodeProperty {
//!                                              target: node_id,
//!                                              key:    path[0],
//!                                              delta:  wire,
//!                                          }
//! ```
//!
//! Inbound is the reverse: the op's `key` becomes the leading path
//! segment, the schema's `apply_wire` routes to the field.

use std::collections::HashMap;

use kyoso_crdt::ApplyError;
use kyoso_crdt::context::{CausalContext, CausalState};
use kyoso_crdt::id::{CrdtId, GlobalSeq, IdGenerator, PeerId};
use kyoso_crdt::lattice::Crdt;
use kyoso_crdt::op::Op;
use kyoso_crdt::schema::{IntoWireOp, SchemaApply};

use crate::op::OpKind;

/// Per-node bookkeeping inside [`Document`].
struct NodeRecord<S> {
    tombstoned: bool,
    schema: S,
}

/// Schema-aware replicated document.
///
/// Holds typed schema state per node and produces wire-format ops via
/// [`Document::mutate_node`]. Inbound ops are dispatched through the
/// schema's [`SchemaApply`] impl.
pub struct Document<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    id_gen: IdGenerator,
    nodes: HashMap<CrdtId, NodeRecord<S>>,
    pending: Vec<Op<OpKind>>,
    applied_seq: GlobalSeq,
    causal: CausalState,
}

// Phase H notes — design choice for echo handling:
//
// The originator submits an op, the server stamps it, and the *same op*
// is broadcast back to the originator with `seq = Some(N)`. We could
// either (a) pre-apply locally during `mutate_node` for instant local
// state visibility, or (b) wait for the echo and apply only on
// confirmation.
//
// Pre-apply is tempting for UX but breaks for non-idempotent CRDTs
// (PN-Counter doubles, Sequence inserts twice) AND breaks LWW stamp
// ordering when a remote op interleaves with our own pending op (the
// pre-applied stamp uses `seq = None` which compares as "always loses
// to confirmed ops"). Both pathologies turned up in the composition
// tests during initial development.
//
// Phase H takes option (b): no pre-apply. Local mutations become
// visible after one server roundtrip. Apps that need optimistic UI
// feedback should keep their own short-lived shadow state (Bevy ECS
// components are a natural fit) and sync it lazily once Document
// confirms. Phase G's typed primitives (`LwwRegister`, `OrSet`, etc.)
// are independently testable in-process without going through Document.

impl<S> Default for Document<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    fn default() -> Self {
        Self::with_peer(0)
    }
}

impl<S> Document<S>
where
    S: Crdt + SchemaApply + Default,
    S::Delta: IntoWireOp,
{
    #[must_use]
    pub fn with_peer(peer: PeerId) -> Self {
        Self {
            id_gen: IdGenerator::new(peer),
            nodes: HashMap::new(),
            pending: Vec::new(),
            applied_seq: 0,
            causal: CausalState::new(),
        }
    }

    pub fn set_peer(&mut self, peer: PeerId) {
        self.id_gen = IdGenerator::new(peer);
    }

    #[must_use]
    pub fn applied_seq(&self) -> GlobalSeq {
        self.applied_seq
    }

    /// Mint a new node id and queue an [`OpKind::AddNode`] for upstream
    /// delivery.
    ///
    /// The node record is pre-inserted locally with [`S::default`] so
    /// the originator can target it via [`Self::mutate_node`]
    /// immediately. Pre-applying the *creation* is safe because
    /// `or_insert` makes the eventual server echo idempotent — but
    /// pre-applying *mutations* is not safe (PN-Counter would double-
    /// count, LWW stamps would interleave wrong with concurrent ops),
    /// so property visibility still waits for the server roundtrip.
    pub fn add_node(&mut self) -> CrdtId {
        let id = self.id_gen.next();
        self.nodes.insert(
            id,
            NodeRecord {
                tombstoned: false,
                schema: S::default(),
            },
        );
        self.pending.push(Op::new(id, OpKind::AddNode));
        id
    }

    /// Read the schema state of a live node.
    #[must_use]
    pub fn node(&self, id: CrdtId) -> Option<&S> {
        let rec = self.nodes.get(&id)?;
        if rec.tombstoned {
            return None;
        }
        Some(&rec.schema)
    }

    /// Queue a typed mutation against a node for upstream delivery.
    ///
    /// The local schema state is *not* updated here; visibility waits
    /// for the server echo via [`Self::apply_remote`]. To compute the
    /// wire-format delta we run the mutation against a throwaway
    /// schema replica of the current state, derive the
    /// `(path, WireDelta)`, and discard the throwaway.
    pub fn mutate_node(&mut self, id: CrdtId, mutation: S::Mutation) {
        let op_id = self.id_gen.next();
        self.mutate_node_with_id(id, op_id, mutation);
    }

    /// Same as [`Self::mutate_node`] but uses a caller-provided
    /// [`CrdtId`] for the op rather than minting one from the
    /// document's internal generator. Used by Bevy plugins where a
    /// shared `ClientSyncEngine` owns the id-generation state and the
    /// document is one of several op producers feeding into it.
    pub fn mutate_node_with_id(&mut self, id: CrdtId, op_id: CrdtId, mutation: S::Mutation) {
        let Some(rec) = self.nodes.get(&id) else {
            return;
        };
        if rec.tombstoned {
            return;
        }
        let mut throwaway = rec.schema.clone();
        let mut ctx = CausalContext::new(op_id, None, &mut self.causal);
        let typed_delta = throwaway.mutate(mutation, &mut ctx);
        let (path, wire_delta) = typed_delta.into_wire_op();
        self.pending.push(Op::new(
            op_id,
            OpKind::SetNodeProperty {
                target: id,
                path,
                delta: wire_delta,
            },
        ));
    }

    /// Pre-insert a node entry without emitting an `OpKind::AddNode`.
    /// Used when an external authority (the server, or another op
    /// producer) is responsible for the AddNode op; this method
    /// records the schema-state slot so subsequent calls to
    /// [`Self::mutate_node_with_id`] / [`Self::node`] work.
    pub fn ensure_node(&mut self, id: CrdtId) {
        self.nodes.entry(id).or_insert_with(|| NodeRecord {
            tombstoned: false,
            schema: S::default(),
        });
    }

    /// Queue a tombstone for upstream delivery. Visibility (the node
    /// disappearing from [`Self::node`]) waits for server confirmation.
    pub fn remove_node(&mut self, id: CrdtId) {
        let op_id = self.id_gen.next();
        self.pending
            .push(Op::new(op_id, OpKind::RemoveNode { target: id }));
    }

    /// Apply a [`OpKind::SetNodeProperty`] or
    /// [`OpKind::SetRefEdgeProperty`] op to the schema state.
    /// Returns `Ok(())` for other op kinds (no-op).
    ///
    /// `Document<S>::nodes` is keyed by `CrdtId`; node and edge ids
    /// share that namespace, so the same schema slot machinery works
    /// for either — the plugin layer keeps node-Documents and
    /// edge-Documents separate, never mixing the two id spaces in one
    /// instance.
    ///
    /// Unlike [`Self::apply_remote`], this method does *not* track
    /// `applied_seq` — the caller is expected to coordinate ordering
    /// via an external authority (a `ClientSyncEngine` or an
    /// op-log replay loop). Designed for the `kyoso_sync`
    /// `SchemaSyncedNodeComponentPlugin` /
    /// `SchemaSyncedEdgeComponentPlugin`, where many `Document<S>`
    /// instances share one global ordering source.
    pub fn apply_property_op(
        &mut self,
        op: &Op<OpKind>,
    ) -> Result<(), kyoso_crdt::lattice::DeltaError> {
        let (target, path, delta) = match &op.kind {
            OpKind::SetNodeProperty { target, path, delta } => (target, path, delta),
            OpKind::SetRefEdgeProperty { target, path, delta } => (target, path, delta),
            _ => return Ok(()),
        };
        // Auto-create the slot if it doesn't exist yet. This is the
        // common case on the inbound side when an op for schema `S`
        // arrives for an entity whose local replica didn't have any
        // `S`-shaped component yet — projection will then queue an
        // `InsertSchemaProjected` to make the component appear.
        let rec = self.nodes.entry(*target).or_insert_with(|| NodeRecord {
            tombstoned: false,
            schema: S::default(),
        });
        if rec.tombstoned {
            return Ok(());
        }
        let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
        rec.schema.apply_wire(path, delta.clone(), &ctx)
    }

    /// Apply a server-confirmed op. Idempotent (out-of-order protected
    /// by `applied_seq`). The originator's own ops flow through this
    /// same path — there is no echo special-casing because nothing has
    /// been pre-applied locally.
    pub fn apply_remote(&mut self, op: &Op<OpKind>) -> Result<(), ApplyError> {
        let seq = op.seq.ok_or(ApplyError::Unconfirmed)?;
        if seq <= self.applied_seq {
            return Ok(());
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
                    schema: S::default(),
                });
            }
            OpKind::RemoveNode { target } => {
                if let Some(rec) = self.nodes.get_mut(target) {
                    rec.tombstoned = true;
                }
            }
            OpKind::SetNodeProperty { target, path, delta } => {
                if let Some(rec) = self.nodes.get_mut(target) {
                    if !rec.tombstoned {
                        let ctx = CausalContext::new(op.id, op.seq, &mut self.causal);
                        // Errors here are protocol-level mismatches;
                        // the ApplyError taxonomy doesn't carry a
                        // delta-error variant, so we surface them as a
                        // soft no-op for now and rely on the schema
                        // layer's typed tests to catch real issues.
                        let _ = rec.schema.apply_wire(path, delta.clone(), &ctx);
                    }
                }
            }
            // Phase H minimal scope: tree edges + ref edges + edge
            // properties are still handled by callers using the lower-
            // level [`crate::CrdtBackend`] directly. Document<S> focuses
            // on the node-property typed flow.
            OpKind::Move { .. }
            | OpKind::AddRefEdge { .. }
            | OpKind::RemoveRefEdge { .. }
            | OpKind::SetRefEdgeProperty { .. } => {}
        }

        self.applied_seq = seq;
        Ok(())
    }

    /// Drain pending ops for upstream delivery.
    pub fn drain_pending(&mut self) -> Vec<Op<OpKind>> {
        std::mem::take(&mut self.pending)
    }
}
