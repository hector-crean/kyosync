//! Move-tree CRDT — replicated parent/position for a forest of nodes.
//!
//! Each operation is a *move*: "place `child` under `new_parent` at
//! sibling `position`". The replicated state is the grow-only **log**
//! of every move ever applied, keyed by the move op's server-assigned
//! [`GlobalSeq`]. The materialised forest ([`MoveTree::forest`]) is a
//! left fold of that log in `GlobalSeq` order, skipping any move that
//! would create a cycle.
//!
//! ## Why no undo/redo
//!
//! The classic Kleppmann move algorithm carries an undo/redo log so
//! replicas converge under *unordered* delivery. kyoso has a
//! linearising server: each client's typed-schema doc is updated only
//! by server-confirmed ops, applied strictly in `GlobalSeq` order.
//! Every replica therefore folds the *same* log in the *same* order,
//! and the cycle check is a deterministic function of
//! `(forest-so-far, move)` — so forward-only apply converges with no
//! undo/redo.
//!
//! Keying the log by `GlobalSeq` in a `BTreeMap` makes that explicit:
//! the order moves are *inserted* is irrelevant, the fold order is the
//! map's key order. Convergence therefore holds under arbitrary
//! *apply* order too (see `converges_under_reordered_apply`) — the
//! linearising server is what lets materialisation be incremental, not
//! a correctness crutch.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::context::CausalContext;
use crate::delta::{Path, WireDelta};
use crate::id::{CrdtId, GlobalSeq};
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::schema::IntoWireOp;

/// One logged move. `new_parent == None` places `child` as a root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MoveRecord {
    child: CrdtId,
    new_parent: Option<CrdtId>,
    position: String,
}

/// Materialised placement of one node — a fold result, not stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveNode {
    /// Parent node, or `None` if the node is a root.
    pub parent: Option<CrdtId>,
    /// Sibling-ordering key among the parent's children.
    pub position: String,
}

/// Replicated parent/position for a forest of nodes. See module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveTree {
    /// Every move ever applied, keyed by the move op's `GlobalSeq`.
    /// `BTreeMap` so the fold order is the linearisation order
    /// regardless of the order moves were inserted.
    log: BTreeMap<GlobalSeq, MoveRecord>,
}

impl MoveTree {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if no move has been applied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.log.is_empty()
    }

    /// Number of moves in the log.
    #[must_use]
    pub fn move_count(&self) -> usize {
        self.log.len()
    }

    /// Materialise the forest: fold the move log in `GlobalSeq` order,
    /// skipping cycle-forming moves. Maps every node that has been
    /// moved at least once to its current placement.
    #[must_use]
    pub fn forest(&self) -> HashMap<CrdtId, MoveNode> {
        let mut tree: HashMap<CrdtId, MoveNode> = HashMap::new();
        for rec in self.log.values() {
            // A move of `child` under `new_parent` forms a cycle iff
            // `child` is `new_parent` or one of its ancestors in the
            // forest built so far. `tree` is acyclic by induction —
            // we never insert a cycle-forming move — so the ancestor
            // walk terminates.
            if let Some(parent) = rec.new_parent {
                if is_ancestor_or_self(&tree, rec.child, parent) {
                    continue;
                }
            }
            tree.insert(
                rec.child,
                MoveNode {
                    parent: rec.new_parent,
                    position: rec.position.clone(),
                },
            );
        }
        tree
    }

    /// Current parent of `node` (`None` if `node` is a root or has
    /// never been moved). Convenience over [`Self::forest`]; prefer
    /// `forest()` when querying many nodes.
    #[must_use]
    pub fn parent_of(&self, node: CrdtId) -> Option<CrdtId> {
        self.forest().get(&node).and_then(|n| n.parent)
    }
}

/// Walk up from `node` via `tree`'s parent pointers; return `true` if
/// `ancestor` is reached (i.e. `ancestor` is `node` or an ancestor of
/// it). Requires `tree` acyclic — guaranteed by [`MoveTree::forest`].
fn is_ancestor_or_self(
    tree: &HashMap<CrdtId, MoveNode>,
    ancestor: CrdtId,
    node: CrdtId,
) -> bool {
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        if current == ancestor {
            return true;
        }
        cursor = tree.get(&current).and_then(|n| n.parent);
    }
    false
}

/// A single move. For [`MoveTree`] the outbound *mutation* and the
/// on-wire *delta* are the same shape — a move is fully self-describing
/// (no index-to-id translation, unlike `Sequence`), so one type serves
/// as both [`Crdt::Mutation`] and [`Crdt::Delta`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveTreeDelta {
    Move {
        child: CrdtId,
        new_parent: Option<CrdtId>,
        position: String,
    },
}

impl Lattice for MoveTree {
    fn bottom() -> Self {
        Self::new()
    }

    /// State-based join: union of the two move logs. Keys are
    /// server-unique `GlobalSeq`s, so a key present on both sides
    /// carries the identical move — union is idempotent and
    /// commutative.
    fn join(&mut self, other: Self) {
        for (seq, rec) in other.log {
            self.log.entry(seq).or_insert(rec);
        }
    }
}

impl Crdt for MoveTree {
    type Mutation = MoveTreeDelta;
    type Delta = MoveTreeDelta;

    /// Insert the move into the log under the op's `GlobalSeq`.
    ///
    /// Returns [`DeltaError::Invalid`] for an unconfirmed op
    /// (`ctx.seq == None`): a `MoveTree` only ever lives in a
    /// typed-schema doc, which is updated solely by server-confirmed
    /// ops.
    fn apply(&mut self, delta: &MoveTreeDelta, ctx: &CausalContext) -> Result<(), DeltaError> {
        let MoveTreeDelta::Move {
            child,
            new_parent,
            position,
        } = delta;
        let seq = ctx.seq.ok_or_else(|| DeltaError::Invalid {
            reason: "MoveTree::apply requires a server-confirmed op (GlobalSeq)".to_string(),
        })?;
        self.log.insert(
            seq,
            MoveRecord {
                child: *child,
                new_parent: *new_parent,
                position: position.clone(),
            },
        );
        Ok(())
    }

    /// Translate a move intent into its delta. The forest changes only
    /// via [`Self::apply`] on the server echo, so `mutate` records the
    /// move locally only when the context already carries a
    /// `GlobalSeq` (replay / tests); kyoso's outbound path uses the
    /// returned delta and discards the mutated copy.
    fn mutate(&mut self, m: MoveTreeDelta, ctx: &mut CausalContext) -> MoveTreeDelta {
        if let Some(seq) = ctx.seq {
            let MoveTreeDelta::Move {
                child,
                new_parent,
                position,
            } = &m;
            self.log.insert(
                seq,
                MoveRecord {
                    child: *child,
                    new_parent: *new_parent,
                    position: position.clone(),
                },
            );
        }
        m
    }
}

impl From<MoveTreeDelta> for WireDelta {
    fn from(d: MoveTreeDelta) -> Self {
        let MoveTreeDelta::Move {
            child,
            new_parent,
            position,
        } = d;
        WireDelta::TreeMove {
            child,
            new_parent,
            position,
        }
    }
}

impl TryFrom<WireDelta> for MoveTreeDelta {
    type Error = DeltaError;
    fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
        match w {
            WireDelta::TreeMove {
                child,
                new_parent,
                position,
            } => Ok(MoveTreeDelta::Move {
                child,
                new_parent,
                position,
            }),
            other => Err(DeltaError::TypeMismatch {
                reason: format!("expected TreeMove, got {other:?}"),
            }),
        }
    }
}

impl IntoWireOp for MoveTreeDelta {
    fn into_wire_op(self) -> (Path, WireDelta) {
        (Path::new(), self.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CausalState;

    /// A node id. Peer is irrelevant for these tests.
    fn node(n: u64) -> CrdtId {
        CrdtId::new(0, n)
    }

    /// A confirmed apply context at linearisation position `seq`.
    fn ctx(state: &mut CausalState, seq: u64) -> CausalContext<'_> {
        CausalContext::new(CrdtId::new(0, seq), Some(seq), state)
    }

    fn mv(child: CrdtId, parent: Option<CrdtId>, pos: &str) -> MoveTreeDelta {
        MoveTreeDelta::Move {
            child,
            new_parent: parent,
            position: pos.to_string(),
        }
    }

    /// Apply `(global_seq, move)` pairs to a fresh tree, in slice order.
    fn tree_from(moves: &[(u64, MoveTreeDelta)]) -> MoveTree {
        let mut state = CausalState::new();
        let mut tree = MoveTree::new();
        for (seq, delta) in moves {
            tree.apply(delta, &ctx(&mut state, *seq)).unwrap();
        }
        tree
    }

    #[test]
    fn single_move_places_node() {
        let tree = tree_from(&[(1, mv(node(1), None, "a"))]);
        assert_eq!(tree.parent_of(node(1)), None);
        assert_eq!(tree.move_count(), 1);
    }

    #[test]
    fn reparent_takes_latest() {
        // node(3): first under node(1), then re-parented under node(2).
        let tree = tree_from(&[
            (1, mv(node(1), None, "a")),
            (2, mv(node(2), None, "b")),
            (3, mv(node(3), Some(node(1)), "a")),
            (4, mv(node(3), Some(node(2)), "a")),
        ]);
        assert_eq!(tree.parent_of(node(3)), Some(node(2)));
    }

    #[test]
    fn cycle_forming_move_is_skipped() {
        // root ← A ← B, then "move A under B" would cycle → skipped.
        let tree = tree_from(&[
            (1, mv(node(1), None, "a")),          // A is a root
            (2, mv(node(2), Some(node(1)), "a")), // B under A
            (3, mv(node(1), Some(node(2)), "a")), // A under B → cycle
        ]);
        assert_eq!(tree.parent_of(node(1)), None, "A keeps its placement");
        assert_eq!(tree.parent_of(node(2)), Some(node(1)));
    }

    #[test]
    fn self_parent_is_skipped() {
        let tree = tree_from(&[
            (1, mv(node(1), None, "a")),
            (2, mv(node(1), Some(node(1)), "a")),
        ]);
        assert_eq!(tree.parent_of(node(1)), None);
    }

    #[test]
    fn converges_under_reordered_apply() {
        // Same move set — including a cycle-forming move (seq 3) —
        // applied in two different arrival orders.
        let moves = [
            (1u64, mv(node(1), None, "a")),
            (2, mv(node(2), Some(node(1)), "a")),
            (3, mv(node(1), Some(node(2)), "a")), // would cycle
            (4, mv(node(3), Some(node(2)), "b")),
        ];

        let in_order = tree_from(&moves);

        let mut shuffled: Vec<_> = moves.to_vec();
        shuffled.swap(0, 3);
        shuffled.swap(1, 2);
        let reordered = tree_from(&shuffled);

        // The log is BTreeMap-keyed by GlobalSeq, so the fold order —
        // hence the converged forest — is independent of apply order.
        assert_eq!(in_order.forest(), reordered.forest());
        // The cycle move (seq 3) is skipped in both.
        assert_eq!(in_order.parent_of(node(1)), None);
        assert_eq!(in_order.parent_of(node(3)), Some(node(2)));
    }

    #[test]
    fn join_is_log_union_and_idempotent() {
        let a = tree_from(&[
            (1, mv(node(1), None, "a")),
            (2, mv(node(2), Some(node(1)), "a")),
        ]);
        let b = tree_from(&[
            (1, mv(node(1), None, "a")),
            (3, mv(node(3), Some(node(1)), "b")),
        ]);
        let mut merged = a.clone();
        merged.join(b.clone());
        assert_eq!(merged.move_count(), 3);
        assert_eq!(merged.parent_of(node(2)), Some(node(1)));
        assert_eq!(merged.parent_of(node(3)), Some(node(1)));

        // Idempotent: joining a copy changes nothing.
        let mut twice = merged.clone();
        twice.join(merged.clone());
        assert_eq!(twice, merged);
    }

    #[test]
    fn mutate_returns_move_delta_and_records_it() {
        let mut state = CausalState::new();
        let mut tree = MoveTree::new();
        let m = mv(node(1), Some(node(2)), "a");
        let delta = tree.mutate(m.clone(), &mut ctx(&mut state, 5));
        assert_eq!(delta, m);
        assert_eq!(tree.move_count(), 1);
    }

    #[test]
    fn wire_round_trip() {
        let d = mv(node(7), Some(node(2)), "p1");
        let w: WireDelta = d.clone().into();
        let back: MoveTreeDelta = w.try_into().unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn try_from_wrong_variant_errors() {
        let w = WireDelta::PnCounterDelta { by: 1 };
        assert!(MoveTreeDelta::try_from(w).is_err());
    }

    #[test]
    fn apply_rejects_unconfirmed_op() {
        let mut state = CausalState::new();
        let mut tree = MoveTree::new();
        let unconfirmed = CausalContext::new(CrdtId::new(0, 1), None, &mut state);
        let err = tree.apply(&mv(node(1), None, "a"), &unconfirmed);
        assert!(matches!(err, Err(DeltaError::Invalid { .. })));
    }
}
