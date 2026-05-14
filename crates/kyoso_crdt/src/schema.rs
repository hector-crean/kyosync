//! Wire-driven schema dispatch.
//!
//! [`Crdt::apply`](crate::Crdt::apply) takes a *typed* delta — the static
//! schema knows which CRDT lives at each field. The wire format, by
//! contrast, carries [`WireDelta`] — a single enum that needs to be
//! routed to the right embedded CRDT given a [`Path`].
//!
//! [`SchemaApply`] is the bridge: implementations walk a path through
//! the schema's fields, convert the [`WireDelta`] to the typed delta of
//! the leaf CRDT, and call [`Crdt::apply`] on it.
//!
//! Hand-rolled implementations are possible but tedious. The
//! `kyoso_crdt_derive` crate's `#[derive(Crdt)]` proc-macro generates
//! [`Lattice`](crate::Lattice), [`Crdt`](crate::Crdt) and [`SchemaApply`]
//! impls in one shot from a struct definition.

use crate::context::CausalContext;
use crate::delta::{Path, PathSegment, WireDelta};
use crate::lattice::DeltaError;
use crate::opaque::OpaqueValue;

/// Convert a typed schema [`Delta`](crate::Crdt::Delta) to the wire
/// shape the transport actually carries: a [`Path`] addressing the
/// mutated field plus a [`WireDelta`] payload.
///
/// Implemented by the `derive(Crdt)` macro for schema structs. Hand-
/// rolled implementations are straightforward — match on the delta
/// variant, return the field's path and `delta.into()`.
pub trait IntoWireOp {
    fn into_wire_op(self) -> (Path, WireDelta);
}

/// A schema struct whose properties can be addressed by [`Path`] and
/// mutated via [`WireDelta`] from the wire.
///
/// Implementations walk `path` through the struct's named fields. The
/// head segment must be a [`PathSegment::Field`] for static schemas;
/// dynamic-keyed nested CRDTs (e.g. `CausalMap`) consume any remaining
/// segments via their own dispatch.
pub trait SchemaApply {
    /// Apply `delta` at the position addressed by `path`.
    ///
    /// Returns [`DeltaError::UnknownPath`] if no field matches the head
    /// segment, or [`DeltaError::TypeMismatch`] if `delta`'s variant
    /// doesn't match the destination CRDT's expected delta type.
    fn apply_wire(
        &mut self,
        path: &Path,
        delta: WireDelta,
        ctx: &CausalContext,
    ) -> Result<(), DeltaError>;

    /// Install fully-merged opaque state at the position addressed by
    /// `path`. Called during snapshot hydration on the client; bypasses
    /// the delta dispatch since `field` already represents post-merge
    /// state from the server.
    ///
    /// Returns [`DeltaError::TypeMismatch`] if the `OpaqueValue` variant
    /// doesn't match the primitive CRDT at this path.
    fn install_state(
        &mut self,
        path: &Path,
        field: OpaqueValue,
    ) -> Result<(), DeltaError>;
}

/// Helper: return the head field name and the path tail.
///
/// Convenience for hand-rolled `SchemaApply` impls and the derive macro.
/// Returns [`DeltaError::Invalid`] for empty paths and
/// [`DeltaError::Invalid`] for non-Field heads (static schemas reject
/// dynamic-key segments at the top level).
pub fn split_field_head(path: &Path) -> Result<(&str, Path), DeltaError> {
    let (head, tail) = path.split_first().ok_or_else(|| DeltaError::Invalid {
        reason: "schema apply requires non-empty path".to_string(),
    })?;
    let name = match head {
        PathSegment::Field(s) => s.as_str(),
        PathSegment::Key(_) => {
            return Err(DeltaError::Invalid {
                reason: "static schema requires Field path segment at the head".to_string(),
            })
        }
    };
    Ok((name, tail))
}
