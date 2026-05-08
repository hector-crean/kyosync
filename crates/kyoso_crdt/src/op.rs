//! Operation envelope.
//!
//! [`Op<K>`] wraps a model-specific op enum `K` with the identity
//! ([`CrdtId`]) and server-assigned ordering ([`GlobalSeq`]) common to
//! every CRDT model. [`Diff<K>`] is a contiguous slice of the server's
//! op log, used to ship history to late joiners.
//!
//! `K` is the per-model op enum (e.g. `kyoso_graph_crdt::OpKind` for the
//! graph model). The framework imposes no structure on `K` beyond what
//! serde requires for wire encoding.

use serde::{Deserialize, Serialize};

use crate::id::{CrdtId, GlobalSeq};

/// A complete operation: identity + payload + (once confirmed by the
/// server) global sequence.
///
/// `seq` is `None` while the op is pending acknowledgement and `Some`
/// after the server has placed it in the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op<K> {
    /// Unique ID of this op. Also the new element's ID for add-style ops
    /// in models that use that convention (e.g. graph `AddNode` /
    /// `AddRefEdge`).
    pub id: CrdtId,
    /// Server-assigned position in the global log. `None` when the op has
    /// been generated locally but not yet round-tripped through the server.
    pub seq: Option<GlobalSeq>,
    pub kind: K,
}

impl<K> Op<K> {
    pub const fn new(id: CrdtId, kind: K) -> Self {
        Self {
            id,
            seq: None,
            kind,
        }
    }

    pub fn with_seq(mut self, seq: GlobalSeq) -> Self {
        self.seq = Some(seq);
        self
    }
}

/// A contiguous slice of the server log.
///
/// `from_seq` is exclusive, `to_seq` is inclusive: a peer that has applied
/// up to `from_seq` and applies all `ops` will reach state `to_seq`. The
/// receiver checks `from_seq == its high-water mark`; a mismatch means
/// missing ops and triggers a re-sync.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diff<K> {
    pub from_seq: GlobalSeq,
    pub to_seq: GlobalSeq,
    pub ops: Vec<Op<K>>,
}

impl<K> Diff<K> {
    pub fn empty(at_seq: GlobalSeq) -> Self {
        Self {
            from_seq: at_seq,
            to_seq: at_seq,
            ops: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

impl<K: Serialize + serde::de::DeserializeOwned> Diff<K> {
    /// Encode this diff to the wire format (postcard, varint-encoded).
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Decode a diff from the wire format.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
