//! Graph-model operation kinds.
//!
//! Every graph mutation is encoded as one of these variants and wrapped in
//! [`kyoso_crdt::Op`]`<OpKind>`. Replaying ops in `GlobalSeq` order on any
//! replica yields the same converged graph state.
//!
//! Tree-shape mutations are a single atomic [`Move`](Self::Move) op
//! (Kleppmann 2022). Reparent + reorder is one op; cycle detection at apply
//! time guarantees the tree never enters an invalid state. The tree shape
//! itself is an annotation on [`AddNode`](Self::AddNode)-created nodes —
//! there is no separate "tree edge" concept at the CRDT layer.
//!
//! Reference edges (component instance → main, prototype links, etc.) are
//! first-class entities created by [`AddRefEdge`](Self::AddRefEdge) and
//! tombstoned by [`RemoveRefEdge`](Self::RemoveRefEdge). Each carries an
//! [`EdgeCategory`] that selects per-category conflict policy.

use serde::{Deserialize, Serialize};

use kyoso_crdt::delta::{Path, WireDelta};
use kyoso_crdt::id::CrdtId;

use crate::edge_category::EdgeCategory;

/// One CRDT operation against the graph.
///
/// Add ops use the operation's own [`CrdtId`] as the new node/edge id —
/// this keeps the wire payload small (no separate target field) and
/// makes the relationship between an op and the element it creates
/// explicit. Update / Set / Remove ops carry an explicit `target`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OpKind {
    /// Create a node with id = enclosing op's [`CrdtId`].
    AddNode,
    /// Tombstone the node referenced by `target`. Incident reference
    /// edges are tombstoned in cascade on apply.
    RemoveNode { target: CrdtId },
    /// Atomic Kleppmann move: set `target`'s tree-parent + position in
    /// one op. `new_parent` is `None` for a root. Apply rejects the op
    /// (no-ops it) when it would create a cycle. Server-mediated total
    /// order means all replicas reach the same accept/reject decision
    /// without rollback.
    Move {
        target: CrdtId,
        new_parent: Option<CrdtId>,
        position: String,
    },

    /// Create a typed reference edge with id = enclosing op's [`CrdtId`].
    /// `category` is fixed at creation; subsequent property edits use
    /// [`SetRefEdgeProperty`](Self::SetRefEdgeProperty).
    AddRefEdge {
        category: EdgeCategory,
        from: CrdtId,
        to: CrdtId,
    },
    /// Tombstone the reference edge referenced by `target`.
    RemoveRefEdge { target: CrdtId },

    /// Apply a [`WireDelta`] to a property of a node, addressed by
    /// [`Path`].
    ///
    /// Single-segment paths are the common case (top-level field on a
    /// derive(Crdt) schema struct); multi-segment paths recurse into
    /// nested schema structs or dynamic-keyed CausalMaps.
    SetNodeProperty {
        target: CrdtId,
        path: Path,
        delta: WireDelta,
    },
    /// Same shape as [`SetNodeProperty`](Self::SetNodeProperty) for
    /// reference edges.
    SetRefEdgeProperty {
        target: CrdtId,
        path: Path,
        delta: WireDelta,
    },
}
