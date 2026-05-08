//! Compact snapshot of the converged graph state.
//!
//! A [`Snapshot`] is the materialised result of replaying every op up to
//! some [`GlobalSeq`]. Stored periodically by the server and shipped to
//! late joiners so they don't have to replay every op since the
//! beginning of time.
//!
//! The snapshot is **tombstone-free**: only live nodes and edges appear,
//! which is what makes log compaction safe — once a snapshot exists at
//! seq `N`, ops below `N` can be discarded as long as every peer has
//! ack'd past `N`.

use serde::{Deserialize, Serialize};

use kyoso_crdt::id::{CrdtId, GlobalSeq};

use crate::edge_category::EdgeCategory;

/// Snapshot of one room's graph state at sequence [`Snapshot::at_seq`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub at_seq: GlobalSeq,
    pub nodes: Vec<NodeSnap>,
    pub edges: Vec<EdgeSnap>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeSnap {
    pub id: CrdtId,
    pub order_key: Option<String>,
    pub tree_parent: Option<CrdtId>,
    /// Per-property LWW state at snapshot time. Late joiners apply
    /// each entry like a `SetNodeProperty` op.
    #[serde(default)]
    pub properties: std::collections::HashMap<String, Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeSnap {
    pub id: CrdtId,
    pub from: CrdtId,
    pub to: CrdtId,
    #[serde(default)]
    pub category: EdgeCategory,
    #[serde(default)]
    pub properties: std::collections::HashMap<String, Vec<u8>>,
}

impl Snapshot {
    pub fn empty(at_seq: GlobalSeq) -> Self {
        Self {
            at_seq,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}
