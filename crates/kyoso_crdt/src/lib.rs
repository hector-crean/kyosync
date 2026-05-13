//! CRDT replication framework.
//!
//! Provides the model-agnostic primitives that every CRDT data model
//! shares — identity ([`CrdtId`]), causal context ([`CausalContext`]),
//! the lattice trait ([`Lattice`] / [`Crdt`]), the wire format
//! ([`Path`] / [`WireDelta`]), the op envelope ([`Op<K>`]), the op log
//! ([`InMemoryOpLog`]), and the wire protocol
//! ([`ClientMsg`] / [`ServerMsg`]).
//!
//! Domain-specific data models live in their own crates and implement
//! [`CrdtModel`]. The graph model is in `kyoso_graph_crdt`; the
//! comments model in `kyoso_comments_crdt`.
//!
//! # Architecture
//!
//! Server-mediated, totally-ordered: every op flows
//! `client → server → all peers`. The server stamps a [`GlobalSeq`] and
//! the log is replayed in seq order on every replica. Awareness data
//! (cursors, selections) goes on a separate channel and isn't covered
//! here.
//!
//! ```text
//!   client A          server (stateful)         client B
//!  ─────────         ──────────────────        ─────────
//!  add_node()        ┌─────────────┐
//!  → pending: [op1]  │  OpLog      │
//!  send([op1]) ───►  │  …          │
//!                    │  append op1 │
//!                    │  seq = N+1  │
//!                    └─────┬───────┘
//!                          │ broadcast
//!                          ├──────────────────►  apply_remote(op1@N+1)
//!  apply_remote(op1@N+1) ◄─┘
//! ```

pub mod backend;
pub mod context;
pub mod delta;
pub mod envelope;
pub mod id;
pub mod lattice;
pub mod log;
pub mod model;
pub mod op;
pub mod protocol;
pub mod schema;
pub mod topology;
pub mod types;

pub use backend::{Backend, EmptySchema, Snapshot};
pub use context::{CausalContext, CausalState, Dot, SubDot};
pub use delta::{Path, PathSegment, WireDelta};
pub use envelope::{EnvelopeClientMsg, EnvelopeServerMsg, ModelGreeting, ModelId, Tier};
pub use id::{CrdtId, GlobalSeq, IdGen, IdGenerator, LocalSeq, PeerId};
pub use lattice::{Crdt, DeltaError, Lattice};
pub use log::{InMemoryOpLog, OpLogRead, OpLogWrite};
pub use model::{ApplyError, CrdtModel};
pub use op::{Diff, Op};
pub use protocol::{ClientMsg, RoomId, ServerMsg};
pub use schema::{IntoWireOp, SchemaApply};
pub use topology::{PropertyOp, Topology};

// Re-export the derive macro so users only need `use kyoso_crdt::Crdt`.
pub use kyoso_crdt_derive::Crdt as DeriveCrdt;

// ---------------------------------------------------------------------------
// Trait stubs originally defined in this crate. Kept for compatibility with
// any consumers that imported them; tighter integration with the op model
// is a future iteration.
// ---------------------------------------------------------------------------

pub trait DiffLike {}

pub trait Mergeable<D: DiffLike> {
    fn apply(&self, diff: D) -> Self;
    fn apply_inverse(&self, diff: D) -> Self;
}
