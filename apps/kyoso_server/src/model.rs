//! Server-wide CRDT model selection.
//!
//! The server is *structurally* generic: it persists postcard blobs,
//! relays them in `GlobalSeq` order, and applies ops to a server-side
//! mirror only to compute snapshots for late-joining clients. None of
//! that depends on the specific shape of the CRDT — only the [`CrdtModel`]
//! trait.
//!
//! However, plumbing a `<M: CrdtModel>` type parameter through every
//! axum handler, every Postgres query helper, and every test would
//! mean ~300 LOC of mechanical propagation. Instead, this module
//! exposes a single [`ServerModel`] alias that the rest of the server
//! references. To swap CRDT models (e.g. for a future text-only
//! kyoso variant) you change this one line — every downstream type
//! follows.
//!
//! The aliases below resolve [`Op`], [`Diff`], [`Snapshot`],
//! [`ClientMsg`], and [`ServerMsg`] against `ServerModel`'s associated
//! types so handlers can refer to them without mentioning [`CrdtModel`].
//!
//! [`Op`]: kyoso_crdt::Op
//! [`Diff`]: kyoso_crdt::Diff
//! [`Snapshot`]: kyoso_crdt::Snapshot
//! [`ClientMsg`]: kyoso_crdt::ClientMsg
//! [`ServerMsg`]: kyoso_crdt::ServerMsg

use kyoso_crdt::{CrdtModel, OpaqueRecord};
use kyoso_graph_crdt::GraphBackend;

/// The CRDT model the server uses for all rooms. Change this one line
/// to retarget the server at a different model.
///
/// Uses [`OpaqueRecord`] as the per-entity property schema so the
/// server holds fully-merged typed-schema state (LWW values, OR-Set
/// adds + tombstones, PN counts, sequence elements) opaquely — by path,
/// without knowing the user-side schema types. Snapshots produced by
/// the server carry this state, so late joiners hydrate per-component
/// `SchemaDoc<C::Schema>` resources from the snapshot rather than
/// replaying every property op from sequence 0.
pub type ServerModel = GraphBackend<OpaqueRecord>;

/// Op type stored in the log + sent on the wire, resolved to the
/// concrete kind that [`ServerModel`] uses.
pub type Op = kyoso_crdt::Op<<ServerModel as CrdtModel>::OpKind>;

/// Op-log slice between two seqs.
pub type Diff = kyoso_crdt::Diff<<ServerModel as CrdtModel>::OpKind>;

/// Persisted snapshot of a room's converged state.
pub type Snapshot = <ServerModel as CrdtModel>::State;

/// Client → server frames.
pub type ClientMsg = kyoso_crdt::ClientMsg<<ServerModel as CrdtModel>::OpKind>;

/// Server → client frames.
pub type ServerMsg = kyoso_crdt::ServerMsg<
    <ServerModel as CrdtModel>::OpKind,
    <ServerModel as CrdtModel>::State,
>;
