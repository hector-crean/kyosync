//! Wire-format delta enum and path addressing.
//!
//! Every property mutation that travels on the wire is one [`WireDelta`].
//! Each concrete CRDT type ([`crate::types`]) defines a typed `Delta`
//! associated type that converts to and from `WireDelta` losslessly.
//!
//! The wire format is uniform across CRDT kinds: a single enum, postcard-
//! encoded. Apply-time dispatch uses [`Path`] to walk the schema down to
//! the embedded CRDT instance, then converts the wire variant into the
//! CRDT's typed delta and applies it.
//!
//! ```text
//!   wire bytes ──postcard──► WireDelta ──TryFrom──► T::Delta ──apply──► T
//!                              ▲                       ▲
//!                              │                       │
//!                              │  one shape on         │  type-safe inside
//!                              │  the wire             │  the CRDT impl
//! ```

use serde::{Deserialize, Serialize};

use crate::context::SubDot;

/// Path into a node or edge schema, used to dispatch a [`WireDelta`] to
/// the embedded CRDT it targets.
///
/// ## Examples
///
/// - `["name"]` — a top-level scalar property.
/// - `["style", "fill"]` — a nested map field.
/// - `["extras", "user-defined-key"]` — a dynamic-map escape hatch.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Path(pub Vec<PathSegment>);

impl Path {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Single-segment path of the static field name `name`.
    #[must_use]
    pub fn field(name: impl Into<String>) -> Self {
        Self(vec![PathSegment::Field(name.into())])
    }

    /// Append a segment, returning the extended path. Useful for building
    /// recursive deltas inside Map dispatch.
    #[must_use]
    pub fn push(mut self, segment: PathSegment) -> Self {
        self.0.push(segment);
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Split into the first segment and the remainder. Returns `None` if
    /// the path is empty.
    #[must_use]
    pub fn split_first(&self) -> Option<(&PathSegment, Self)> {
        let (head, tail) = self.0.split_first()?;
        Some((head, Self(tail.to_vec())))
    }
}

/// One segment of a [`Path`].
///
/// `Field` and `Key` are distinct on the wire — the schema layer
/// (Phase G) uses the variant to enforce that static fields are not
/// confused with dynamic map keys, even though both carry strings.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PathSegment {
    /// Statically-known field name on a schema struct.
    Field(String),
    /// Dynamic key in a [`crate::types::CausalMap`].
    Key(String),
}

/// Uniform on-the-wire representation of a property mutation.
///
/// One variant per CRDT kind. Concrete CRDT types in [`crate::types`]
/// produce and consume these via `From` / `TryFrom` impls; the schema
/// layer guarantees a delta variant only reaches a CRDT that knows how
/// to handle it (a [`DeltaError::TypeMismatch`](crate::DeltaError) is a
/// protocol bug, not a runtime expectation).
///
/// ## Why no timestamps / dots inside the delta
///
/// Each [`Op`](crate::op::Op) on the wire already carries its own
/// [`CrdtId`](crate::CrdtId) and (after server stamp) [`GlobalSeq`].
/// Every embedded delta inherits that identity at apply time via
/// [`CausalContext`](crate::CausalContext) — the LWW stamp, the OR-Set
/// add tag, and the PN-Counter replica are all *derived* on the receiving
/// side from the outer op rather than transmitted redundantly. Only data
/// the outer op cannot supply (dynamic positions, observed-sets for
/// remove-style ops) lives inside the delta.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WireDelta {
    /// Last-Writer-Wins register replacement. The LWW stamp is derived
    /// from the outer op's `(seq, peer)` at apply time.
    LwwReplace { value: Vec<u8> },

    /// OR-Set add. The element's unique tag is the outer op's `CrdtId`
    /// (no separate tag field needed — adds are always 1-per-op).
    OrSetAdd { value: Vec<u8> },

    /// OR-Set remove. `observed` is the set of tags this op witnessed at
    /// mutation time — only those tags are removed; concurrent adds with
    /// fresher tags survive (add-wins).
    OrSetRemove { observed: Vec<SubDot> },

    /// PN-Counter delta. The replica id is derived from `op.id.peer`.
    PnCounterDelta { by: i64 },

    /// Sequence insert (RGA-flavored): a new element with id derived
    /// from the outer op (`SubDot { op: ctx.op_id, sub: 0 }`) is
    /// inserted immediately after `predecessor`. `predecessor = None`
    /// inserts at the head. Concurrent inserts sharing a predecessor
    /// are ordered by id descending (newest concurrent insert comes
    /// first in the visible sequence).
    SequenceInsert {
        predecessor: Option<SubDot>,
        value: Vec<u8>,
    },

    /// Sequence delete: tombstone the element identified by `target`.
    /// Multiple targets in a single op enable range-delete to ship as
    /// one wire message.
    SequenceDelete { targets: Vec<SubDot> },

    /// Map (`CausalMap<K, V>`) put: route `inner` to the value at `key`,
    /// creating the entry at the lattice bottom if absent.
    MapPut { key: PathSegment, inner: Box<WireDelta> },

    /// Map remove. `observed` carries the sub-dots of every concurrent
    /// add this op witnessed; only those entries are removed.
    MapRemove { key: PathSegment, observed: Vec<SubDot> },
}

impl WireDelta {
    /// Encode to wire bytes (postcard).
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Decode from wire bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
