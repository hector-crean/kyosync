//! Causal context exposed to nested CRDTs during apply / mutate.
//!
//! Every CRDT op in kyoso has a stable identity ([`CrdtId`]) and (once the
//! server has stamped it) a linear position ([`GlobalSeq`]). Embedded
//! CRDTs — properties on a node, OR-Set add tags, etc. — frequently need
//! a *fresh, globally unique* identifier they can mint without coordinating
//! with peers. The outer op's [`CrdtId`] is the perfect prefix: any sub-id
//! formed as `(op_id, sub_counter)` is unique because the outer `op_id` is.
//!
//! [`CausalContext`] is the small, structured object that nested CRDTs read
//! from to obtain their fresh dots and (eventually, when partial replication
//! or branching land) to interrogate the document-wide observed-set.
//!
//! ## Read vs write
//!
//! - **Apply** (inbound): the context is read-only — embedded CRDTs already
//!   know their dots from the wire delta; they may inspect the outer op's
//!   identity but not allocate new dots.
//! - **Mutate** (outbound): the context is mutable — embedded CRDTs allocate
//!   sub-dots under the freshly-minted outer op.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::id::{CrdtId, GlobalSeq};

/// The identity of one CRDT operation. Same shape as [`CrdtId`]; this alias
/// exists to clarify intent at use sites.
pub type Dot = CrdtId;

/// A sub-dot under a parent op. The `u32` is allocated by
/// [`CausalContext::fresh_sub_dot`]; combined with the parent dot it is
/// globally unique without coordination because the parent dot already is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubDot {
    pub op: Dot,
    pub sub: u32,
}

impl SubDot {
    #[must_use]
    pub const fn new(op: Dot, sub: u32) -> Self {
        Self { op, sub }
    }
}

impl std::fmt::Display for SubDot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.op, self.sub)
    }
}

/// Document-level bookkeeping that backs [`CausalContext`].
///
/// Lives on the [`CrdtBackend`](crate::backend::CrdtBackend) (or the
/// equivalent server-side mirror) and persists across op applications.
/// The current shape is small; future support for branching or partial
/// replication grows fields here without touching the trait surface.
#[derive(Debug, Default, Clone)]
pub struct CausalState {
    /// Per-op sub-dot counters. Reset when the op is fully applied.
    sub_counters: HashMap<Dot, u32>,
}

impl CausalState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next_sub(&mut self, op: Dot) -> u32 {
        let entry = self.sub_counters.entry(op).or_insert(0);
        let v = *entry;
        *entry += 1;
        v
    }
}

/// Borrowed view of [`CausalState`] paired with the outer op's identity,
/// passed into [`Crdt::apply`](crate::lattice::Crdt::apply) and
/// [`Crdt::mutate`](crate::lattice::Crdt::mutate).
///
/// The mutable borrow is held for the duration of the apply / mutate call;
/// callers reconstruct a fresh `CausalContext` per op application.
pub struct CausalContext<'a> {
    /// Identity of the op currently being processed. Used as the parent
    /// for any [`SubDot`] minted during this call.
    pub op_id: Dot,
    /// Server-assigned linear position of the op. `None` while an op is
    /// still pending acknowledgement on the local side; `Some` for any op
    /// arriving from the server (inbound) or being replayed.
    pub seq: Option<GlobalSeq>,
    state: &'a mut CausalState,
}

impl<'a> CausalContext<'a> {
    pub fn new(op_id: Dot, seq: Option<GlobalSeq>, state: &'a mut CausalState) -> Self {
        Self { op_id, seq, state }
    }

    /// Allocate a fresh sub-dot under the current op. Used by CRDTs that
    /// need a unique tag per element (e.g. OR-Set add).
    pub fn fresh_sub_dot(&mut self) -> SubDot {
        let sub = self.state.next_sub(self.op_id);
        SubDot::new(self.op_id, sub)
    }
}
