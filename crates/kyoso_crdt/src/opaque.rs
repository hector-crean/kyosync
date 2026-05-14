//! Opaque, schema-agnostic CRDT state for server snapshots and replay.
//!
//! [`OpaqueSchemaState`] is what the server uses as its `Backend<T, S>`
//! schema parameter. It holds the fully-merged primitive CRDT state
//! per entity, keyed by [`Path`], without knowing the concrete user
//! schema type. Each leaf stores its value as `Vec<u8>` (the original
//! postcard-encoded payload from the wire op) so the server doesn't
//! need to know what `T` was inside the client's typed schema.
//!
//! The variants of [`OpaqueField`] mirror the primitive CRDT types in
//! [`crate::types`]. Each primitive's existing `Lattice` / `Crdt` impls
//! handle merging when `T = Vec<u8>` — bytes are kept opaque end-to-end.
//!
//! ## Lifecycle
//!
//! ```text
//!     wire op (SetNodeProperty)
//!         │
//!         ▼
//!     OpaqueSchemaState::apply_wire(path, delta, ctx)
//!         │
//!         ├─ resolve / create OpaqueField at path
//!         └─ dispatch on WireDelta variant → primitive's apply
//!
//!     snapshot()
//!         │
//!         └─ serialize HashMap<Path, OpaqueField> as part of the
//!            server's snapshot payload
//!
//!     hydrate (client side)
//!         │
//!         └─ for each (path, field): route through registered hydrator
//!            keyed by path head (= schema name) → SchemaApply::install_state
//! ```
//!
//! ## What this is NOT
//!
//! - Not a way for clients to compose typed schemas. Clients still use
//!   `derive(Crdt)` schemas and per-component `SchemaDoc<S>` resources.
//! - Not a way for the server to mutate typed state. The server only
//!   receives ops; it never originates them. Hence
//!   [`OpaqueSchemaState`]'s `Crdt::Mutation` is uninhabited.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::context::CausalContext;
use crate::delta::{Path, PathSegment, WireDelta};
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::schema::{IntoWireOp, SchemaApply};
use crate::types::{LwwDelta, LwwRegister, OrSet, OrSetDelta, PnCounter, PnDelta, Sequence, SequenceDelta};

/// One primitive CRDT's fully-merged state, holding values as opaque
/// bytes so the server doesn't need to know the user's `T`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OpaqueField {
    Lww(LwwRegister<Vec<u8>>),
    OrSet(OrSet<Vec<u8>>),
    PnCounter(PnCounter),
    Sequence(Sequence<Vec<u8>>),
}

impl Lattice for OpaqueField {
    fn bottom() -> Self {
        // No meaningful bottom — caller always constructs the correct
        // variant for the incoming wire op. We pick Lww arbitrarily so
        // bottom() is total; a type-mismatched join with this value is
        // a protocol bug surfaced by `OpaqueField::join`.
        OpaqueField::Lww(LwwRegister::empty())
    }

    fn join(&mut self, other: Self) {
        match (self, other) {
            (OpaqueField::Lww(a), OpaqueField::Lww(b)) => a.join(b),
            (OpaqueField::OrSet(a), OpaqueField::OrSet(b)) => a.join(b),
            (OpaqueField::PnCounter(a), OpaqueField::PnCounter(b)) => a.join(b),
            (OpaqueField::Sequence(a), OpaqueField::Sequence(b)) => a.join(b),
            // Variant mismatch — caller is mixing CRDT kinds at the same
            // path. This shouldn't happen with a well-formed schema; we
            // leave `self` untouched rather than panic.
            (_, _) => {}
        }
    }
}

/// Per-entity opaque CRDT state. The server's schema parameter.
///
/// Keyed by full [`Path`] (which already includes schema-name +
/// field-name + any map keys). Each entry is one primitive CRDT.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpaqueSchemaState {
    /// `BTreeMap` (not `HashMap`) so postcard encoding is
    /// deterministic across processes — needed for the
    /// snapshot-bytes-stable proptest to hold.
    pub fields: BTreeMap<Path, OpaqueField>,
}

impl OpaqueSchemaState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Iterate over `(path, field)` entries — used by client hydration.
    pub fn iter(&self) -> impl Iterator<Item = (&Path, &OpaqueField)> {
        self.fields.iter()
    }
}

impl Lattice for OpaqueSchemaState {
    fn bottom() -> Self {
        Self::default()
    }

    fn join(&mut self, other: Self) {
        for (path, field) in other.fields {
            match self.fields.get_mut(&path) {
                Some(existing) => existing.join(field),
                None => {
                    self.fields.insert(path, field);
                }
            }
        }
    }
}

/// Uninhabited mutation type — the server never originates ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OpaqueMutation {}

/// Uninhabited typed-delta — the server never produces a typed delta.
/// Wire deltas come in via [`SchemaApply::apply_wire`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OpaqueDelta {}

impl Crdt for OpaqueSchemaState {
    type Mutation = OpaqueMutation;
    type Delta = OpaqueDelta;

    fn apply(&mut self, _delta: &Self::Delta, _ctx: &CausalContext) -> Result<(), DeltaError> {
        // OpaqueDelta is uninhabited; this is unreachable.
        Ok(())
    }

    fn mutate(&mut self, _m: Self::Mutation, _ctx: &mut CausalContext) -> Self::Delta {
        unreachable!("OpaqueSchemaState has no mutations")
    }
}

impl IntoWireOp for OpaqueDelta {
    fn into_wire_op(self) -> (Path, WireDelta) {
        // OpaqueDelta is uninhabited; this is unreachable.
        match self {}
    }
}

impl SchemaApply for OpaqueSchemaState {
    /// Apply a wire delta at the full schema-relative path.
    ///
    /// The path already includes the schema-name head segment + field +
    /// any map keys — it's stored verbatim as the HashMap key. The
    /// incoming wire delta determines which `OpaqueField` variant lives
    /// (or gets created) at that path.
    fn apply_wire(
        &mut self,
        path: &Path,
        delta: WireDelta,
        ctx: &CausalContext,
    ) -> Result<(), DeltaError> {
        match delta {
            WireDelta::LwwReplace { value } => {
                let field = self
                    .fields
                    .entry(path.clone())
                    .or_insert_with(|| OpaqueField::Lww(LwwRegister::empty()));
                let OpaqueField::Lww(reg) = field else {
                    return Err(DeltaError::TypeMismatch {
                        reason: format!("LwwReplace at non-Lww path {path:?}"),
                    });
                };
                reg.apply(&LwwDelta { value }, ctx)
            }
            WireDelta::OrSetAdd { value } => {
                let field = self
                    .fields
                    .entry(path.clone())
                    .or_insert_with(|| OpaqueField::OrSet(OrSet::new()));
                let OpaqueField::OrSet(set) = field else {
                    return Err(DeltaError::TypeMismatch {
                        reason: format!("OrSetAdd at non-OrSet path {path:?}"),
                    });
                };
                set.apply(&OrSetDelta::Add { value }, ctx)
            }
            WireDelta::OrSetRemove { observed } => {
                let field = self
                    .fields
                    .entry(path.clone())
                    .or_insert_with(|| OpaqueField::OrSet(OrSet::new()));
                let OpaqueField::OrSet(set) = field else {
                    return Err(DeltaError::TypeMismatch {
                        reason: format!("OrSetRemove at non-OrSet path {path:?}"),
                    });
                };
                set.apply(&OrSetDelta::Remove { observed }, ctx)
            }
            WireDelta::PnCounterDelta { by } => {
                let field = self
                    .fields
                    .entry(path.clone())
                    .or_insert_with(|| OpaqueField::PnCounter(PnCounter::new()));
                let OpaqueField::PnCounter(counter) = field else {
                    return Err(DeltaError::TypeMismatch {
                        reason: format!("PnCounterDelta at non-PnCounter path {path:?}"),
                    });
                };
                counter.apply(&PnDelta { by }, ctx)
            }
            WireDelta::SequenceInsert { predecessor, value } => {
                let field = self
                    .fields
                    .entry(path.clone())
                    .or_insert_with(|| OpaqueField::Sequence(Sequence::new()));
                let OpaqueField::Sequence(seq) = field else {
                    return Err(DeltaError::TypeMismatch {
                        reason: format!("SequenceInsert at non-Sequence path {path:?}"),
                    });
                };
                seq.apply(&SequenceDelta::Insert { predecessor, value }, ctx)
            }
            WireDelta::SequenceDelete { targets } => {
                let field = self
                    .fields
                    .entry(path.clone())
                    .or_insert_with(|| OpaqueField::Sequence(Sequence::new()));
                let OpaqueField::Sequence(seq) = field else {
                    return Err(DeltaError::TypeMismatch {
                        reason: format!("SequenceDelete at non-Sequence path {path:?}"),
                    });
                };
                seq.apply(&SequenceDelta::Delete { targets }, ctx)
            }
            WireDelta::MapPut { key, inner } => {
                // MapPut shouldn't normally appear on the wire — the
                // outer schema's IntoWireOp encodes map navigation in
                // the path, not the delta. We tolerate it by extending
                // the path with the key and recursing on the inner.
                let mut extended = path.clone();
                extended.0.push(key);
                self.apply_wire(&extended, *inner, ctx)
            }
            WireDelta::MapRemove { key, .. } => {
                // MapRemove drops the entry at `path + [key]`, and any
                // descendants of that entry (recursive removal of every
                // primitive whose path starts with the dropped key).
                let mut drop_prefix = path.clone();
                let key_str = match &key {
                    PathSegment::Field(s) | PathSegment::Key(s) => s.as_str(),
                };
                if !key_str.is_empty() {
                    drop_prefix.0.push(key);
                }
                self.fields.retain(|p, _| !path_starts_with(p, &drop_prefix));
                Ok(())
            }
        }
    }

    fn install_state(&mut self, path: &Path, field: OpaqueField) -> Result<(), DeltaError> {
        // OpaqueSchemaState owns its own per-path storage; install is a
        // direct insert. Used only by `Backend::restore` symmetry; the
        // server side doesn't currently call this.
        self.fields.insert(path.clone(), field);
        Ok(())
    }
}

fn path_starts_with(p: &Path, prefix: &Path) -> bool {
    if p.0.len() < prefix.0.len() {
        return false;
    }
    p.0.iter().zip(prefix.0.iter()).all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CausalState;
    use crate::id::CrdtId;

    fn ctx_at(state: &mut CausalState, peer: u32, seq: u64) -> CausalContext<'_> {
        CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
    }

    fn p(segments: &[&str]) -> Path {
        Path(segments.iter().map(|s| PathSegment::Field((*s).into())).collect())
    }

    #[test]
    fn opaque_apply_lww_replace() {
        let mut s = OpaqueSchemaState::new();
        let mut state = CausalState::new();
        s.apply_wire(
            &p(&["Frame", "width"]),
            WireDelta::LwwReplace { value: vec![1, 2, 3] },
            &ctx_at(&mut state, 1, 1),
        )
        .unwrap();
        let OpaqueField::Lww(reg) = &s.fields[&p(&["Frame", "width"])] else {
            panic!("expected Lww");
        };
        assert_eq!(reg.get(), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn opaque_apply_pn_counter_accumulates() {
        let mut s = OpaqueSchemaState::new();
        let mut state = CausalState::new();
        s.apply_wire(
            &p(&["Counted", "edits"]),
            WireDelta::PnCounterDelta { by: 3 },
            &ctx_at(&mut state, 1, 1),
        )
        .unwrap();
        s.apply_wire(
            &p(&["Counted", "edits"]),
            WireDelta::PnCounterDelta { by: 4 },
            &ctx_at(&mut state, 1, 2),
        )
        .unwrap();
        let OpaqueField::PnCounter(c) = &s.fields[&p(&["Counted", "edits"])] else {
            panic!("expected PnCounter");
        };
        assert_eq!(c.value(), 7);
    }

    #[test]
    fn opaque_join_preserves_higher_lww_stamp() {
        let mut a = OpaqueSchemaState::new();
        let mut b = OpaqueSchemaState::new();
        let mut sa = CausalState::new();
        let mut sb = CausalState::new();
        a.apply_wire(
            &p(&["F", "x"]),
            WireDelta::LwwReplace { value: vec![10] },
            &ctx_at(&mut sa, 1, 5),
        )
        .unwrap();
        b.apply_wire(
            &p(&["F", "x"]),
            WireDelta::LwwReplace { value: vec![20] },
            &ctx_at(&mut sb, 2, 9),
        )
        .unwrap();
        a.join(b);
        let OpaqueField::Lww(reg) = &a.fields[&p(&["F", "x"])] else {
            panic!();
        };
        assert_eq!(reg.get(), Some(&vec![20]));
    }

    #[test]
    fn opaque_type_mismatch_is_error() {
        let mut s = OpaqueSchemaState::new();
        let mut state = CausalState::new();
        s.apply_wire(
            &p(&["F", "x"]),
            WireDelta::LwwReplace { value: vec![1] },
            &ctx_at(&mut state, 1, 1),
        )
        .unwrap();
        let err = s
            .apply_wire(
                &p(&["F", "x"]),
                WireDelta::PnCounterDelta { by: 1 },
                &ctx_at(&mut state, 1, 2),
            )
            .unwrap_err();
        assert!(matches!(err, DeltaError::TypeMismatch { .. }));
    }
}
