//! Concrete CRDT primitives: registers, sets, counters, sequences, maps.
//!
//! Each type implements [`Lattice`](crate::Lattice) (the algebraic side)
//! and [`Crdt`](crate::Crdt) (the operational side), and provides
//! `From<TypedDelta> for WireDelta` plus `TryFrom<WireDelta> for TypedDelta`
//! impls so the schema layer can route on-the-wire deltas to the right
//! embedded instance.
//!
//! ## Catalog (Phase C)
//!
//! - [`LwwRegister`] — single value with last-writer-wins semantics.
//! - [`OrSet`] — observed-remove (add-wins) set.
//! - [`PnCounter`] — counter with `+1` and `-1` deltas per replica.
//! - [`CausalMap`] — string-keyed map of CRDT values; the central
//!   composition combinator.
//! - [`Sequence`] — **naive `Vec`-backed stub**, suitable for
//!   single-writer fields. Concurrent edits will lose data; the real
//!   Fugue / yjsmod implementation is a future phase.

pub mod lww;
pub mod or_set;
pub mod pn_counter;
pub mod causal_map;
pub mod sequence;

pub use causal_map::{CausalMap, MapDelta, MapMut};
pub use lww::{LwwDelta, LwwMut, LwwRegister};
pub use or_set::{OrSet, OrSetDelta, OrSetMut};
pub use pn_counter::{PnCounter, PnDelta, PnMut};
pub use sequence::{Sequence, SequenceDelta, SequenceMut};
