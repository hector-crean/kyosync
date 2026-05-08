//! Algebraic foundations for composable CRDTs.
//!
//! Two layered traits:
//!
//! - [`Lattice`] — the abstract object: a join-semilattice. Pure algebra,
//!   no notion of operations or wire format. State-based composition lives
//!   here (Pair, Map, recursive structures all inherit `Lattice`
//!   automatically once their components do).
//! - [`Crdt`] — adds an operation/delta surface on top of [`Lattice`].
//!   Each impl defines a typed [`Crdt::Mutation`] (the application's
//!   intent: "set name to X", "add tag Y") and a typed [`Crdt::Delta`]
//!   (what travels on the wire, idempotent and order-tolerant).
//!
//! Convergence is a theorem of the lattice axioms (associativity,
//! commutativity, idempotency of [`Lattice::join`]). Any concrete CRDT in
//! the [`crate::types`] module ships with property tests that exercise
//! those axioms directly.

use crate::context::CausalContext;

/// A join-semilattice with a bottom element.
///
/// ## Laws
///
/// For any `a`, `b`, `c`:
///
/// - **Associativity**: `(a ⊔ b) ⊔ c == a ⊔ (b ⊔ c)`
/// - **Commutativity**: `a ⊔ b == b ⊔ a`
/// - **Idempotency**: `a ⊔ a == a`
/// - **Bottom**: `Lattice::bottom() ⊔ a == a`
///
/// where `a ⊔ b` is `let mut acc = a.clone(); acc.join(b); acc`.
///
/// These three axioms make convergence safe under arbitrary message
/// reordering, duplication, and grouping — the entire reason CRDTs work.
pub trait Lattice: Clone + PartialEq {
    /// The least element. `bottom() ⊔ x == x` for every `x`.
    fn bottom() -> Self;

    /// Idempotent, commutative, associative join (least upper bound).
    /// Mutates `self` in place to the lattice join of `self` and `other`.
    fn join(&mut self, other: Self);

    /// `self ≤ other` in the lattice order: equivalent to
    /// `{ let mut o = other.clone(); o.join(self.clone()); o == *other }`.
    /// Default impl uses that definition; concrete types may override for
    /// efficiency.
    fn leq(&self, other: &Self) -> bool {
        let mut o = other.clone();
        o.join(self.clone());
        o == *other
    }
}

/// A CRDT — a [`Lattice`] equipped with a typed operation/delta API.
///
/// `Mutation` is the *intent* the application expresses (set, add, delete);
/// `Delta` is the *wire-shippable* idempotent record of one change. The
/// two coincide for some CRDTs (LWW: a mutation is just a "write this
/// value" delta) but diverge for others (OR-Set: the mutation is "add X",
/// the delta is "(X, fresh-dot)").
///
/// ## Apply contract
///
/// [`Self::apply`] is **idempotent**: applying the same `delta` twice is a
/// no-op. It is **commutative with concurrent deltas**: applying delta
/// `d1` then `d2` reaches the same state as applying `d2` then `d1` —
/// provided both refer to the same logical state (i.e. share a causal
/// context). Convergence relies on this.
///
/// Apply may legitimately fail when the delta references state that does
/// not exist locally yet (e.g. a `MapPut` for a missing key in a context
/// that hasn't been seeded). The caller should treat the error as a
/// signal to repair causal-history holes (request a catch-up), not as a
/// permanent failure.
pub trait Crdt: Lattice {
    /// High-level intent the application expresses.
    type Mutation;
    /// On-the-wire representation of one change. Convertible to/from
    /// [`crate::delta::WireDelta`] (Phase D) once that module lands; the
    /// Phase-A signature deliberately stays free of that dependency.
    type Delta: Clone + PartialEq;

    /// Apply a delta from a remote peer or replayed log. Idempotent.
    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError>;

    /// Generate a delta from a local mutation. The mutation is *also*
    /// applied to `self` so the caller observes its effect immediately;
    /// the returned delta is what the transport layer ships upstream.
    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta;
}

/// Reasons a [`Crdt::apply`] call can fail.
///
/// Distinct from [`crate::backend::ApplyError`], which models *ordering*
/// failures at the op-log level. `DeltaError` models *content* failures
/// at the delta dispatch level.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeltaError {
    /// The delta variant does not match the CRDT type at this position
    /// in the schema. Indicates a wire-format / schema-version mismatch.
    #[error("delta type does not match this CRDT: {reason}")]
    TypeMismatch { reason: String },

    /// The path resolves to a position that does not exist in the schema.
    #[error("unknown path segment: {segment}")]
    UnknownPath { segment: String },

    /// The delta payload is structurally invalid (e.g. negative position
    /// in a sequence delete, missing required field).
    #[error("invalid delta: {reason}")]
    Invalid { reason: String },

    /// The delta references state that has not been observed locally —
    /// usually a causal-history hole. The caller should request a
    /// catch-up rather than retry.
    #[error("missing causal predecessor: {reason}")]
    MissingPredecessor { reason: String },
}
