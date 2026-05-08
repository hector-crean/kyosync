//! Server-authoritative op log.
//!
//! In-memory implementation indexed by [`GlobalSeq`]. The server owns the
//! canonical instance; clients keep a derived "applied so far" cursor.
//! Persistent storage (sqlite, file-backed mmap, etc.) is layered on top
//! of this trait so the in-memory variant can stay fast and simple for
//! tests and short-lived sessions.
//!
//! Generic over the op-kind enum `K`. Each model binds its own enum
//! (e.g. `kyoso_graph_crdt::OpKind`) at the call site.

use crate::id::GlobalSeq;
use crate::op::{Diff, Op};

/// Read-only view of an op log.
pub trait OpLogRead<K> {
    /// Highest committed sequence number, or `0` if the log is empty.
    fn head(&self) -> GlobalSeq;

    /// Slice of ops with `from_seq < seq <= to_seq`. Inclusive of `to_seq`,
    /// exclusive of `from_seq`. Returns at most `to_seq - from_seq` ops.
    fn slice(&self, from_seq: GlobalSeq, to_seq: GlobalSeq) -> Vec<Op<K>>;

    /// Convenience: every op the peer hasn't seen yet, given its high-water
    /// mark `since`.
    fn diff_since(&self, since: GlobalSeq) -> Diff<K> {
        let head = self.head();
        Diff {
            from_seq: since,
            to_seq: head,
            ops: self.slice(since, head),
        }
    }
}

/// Mutable op log: append ops as the server receives them.
pub trait OpLogWrite<K>: OpLogRead<K> {
    /// Append `op` to the log; assigns the next sequence and returns the
    /// stamped op (with `seq` populated). Implementations are responsible
    /// for assigning `seq = head() + 1`.
    fn append(&mut self, op: Op<K>) -> Op<K>;
}

/// In-memory op log. Vector-backed; index `i` holds the op with
/// `seq = i + 1` (sequences are 1-indexed so `0` can mean "before the log
/// begins").
#[derive(Debug, Clone)]
pub struct InMemoryOpLog<K> {
    ops: Vec<Op<K>>,
}

impl<K> Default for InMemoryOpLog<K> {
    fn default() -> Self {
        Self { ops: Vec::new() }
    }
}

impl<K> InMemoryOpLog<K> {
    pub const fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

impl<K: Clone> OpLogRead<K> for InMemoryOpLog<K> {
    fn head(&self) -> GlobalSeq {
        self.ops.len() as GlobalSeq
    }

    fn slice(&self, from_seq: GlobalSeq, to_seq: GlobalSeq) -> Vec<Op<K>> {
        if from_seq >= to_seq {
            return Vec::new();
        }
        let start = from_seq as usize;
        let end = (to_seq as usize).min(self.ops.len());
        if start >= end {
            return Vec::new();
        }
        self.ops[start..end].to_vec()
    }
}

impl<K: Clone> OpLogWrite<K> for InMemoryOpLog<K> {
    fn append(&mut self, op: Op<K>) -> Op<K> {
        let seq = self.head() + 1;
        let stamped = op.with_seq(seq);
        self.ops.push(stamped.clone());
        stamped
    }
}
