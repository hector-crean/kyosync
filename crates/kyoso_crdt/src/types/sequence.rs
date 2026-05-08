//! RGA-flavored sequence CRDT.
//!
//! Each element has a unique [`SubDot`] (the outer op's [`CrdtId`] with
//! `sub = 0`) and a `predecessor` reference. Visible order is computed
//! by an in-order DFS where, for each parent, children are emitted
//! newest-first (descending sub-dot ordering).
//!
//! ## Convergence properties
//!
//! - Concurrent inserts at *different* positions don't interact and
//!   converge trivially.
//! - Concurrent inserts at the *same* position (same predecessor) are
//!   ordered deterministically by `(op.peer, op.seq)` — the descending
//!   order on [`SubDot`] is the standard RGA tie-break that puts the
//!   newer concurrent insert first.
//! - Deletes are tombstones keyed by the target element's id;
//!   concurrent insert-after-delete still finds the (tombstoned)
//!   predecessor and inserts there.
//!
//! Caveat: RGA does *not* satisfy maximal non-interleaving (Fugue does).
//! Two peers concurrently typing different paragraphs at the same
//! caret position will interleave character-by-character. This is the
//! standard collaborative-editor pathology that motivated Fugue;
//! upgrading to a Fugue impl is a follow-up. RGA's failure mode is
//! cosmetic, not divergent — both peers still reach the same final
//! state.

use std::collections::HashMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::context::{CausalContext, SubDot};
use crate::delta::{Path, WireDelta};
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::schema::{IntoWireOp, SchemaApply};

/// Per-element bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Element<T> {
    value: T,
    tombstoned: bool,
    predecessor: Option<SubDot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sequence<T> {
    /// All elements ever inserted, indexed by id. Tombstones included.
    elements: HashMap<SubDot, Element<T>>,
}

impl<T> Sequence<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            elements: HashMap::new(),
        }
    }

    /// Live (non-tombstoned) values in visible order.
    pub fn iter(&self) -> Vec<&T> {
        let mut out: Vec<&T> = Vec::with_capacity(self.elements.len());
        self.walk_collect(None, &mut out);
        out
    }

    fn walk_collect<'a>(&'a self, parent: Option<SubDot>, out: &mut Vec<&'a T>) {
        let mut children: Vec<SubDot> = self
            .elements
            .iter()
            .filter(|(_, e)| e.predecessor == parent)
            .map(|(id, _)| *id)
            .collect();
        children.sort_by(|a, b| b.cmp(a));
        for child_id in children {
            let child = &self.elements[&child_id];
            if !child.tombstoned {
                out.push(&child.value);
            }
            self.walk_collect(Some(child_id), out);
        }
    }

    /// Live ids in visible order. Useful when you need to compute a
    /// predecessor for a typed insert at position `i`.
    #[must_use]
    pub fn live_ids(&self) -> Vec<SubDot> {
        let mut out = Vec::with_capacity(self.elements.len());
        self.walk_ids(None, &mut |id, elem| {
            if !elem.tombstoned {
                out.push(id);
            }
        });
        out
    }

    /// Materialize as an owned `Vec<T>` for assertions / display.
    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.iter().into_iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.elements
            .values()
            .filter(|e| !e.tombstoned)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        !self.elements.values().any(|e| !e.tombstoned)
    }

    fn walk_ids(&self, parent: Option<SubDot>, visit: &mut dyn FnMut(SubDot, &Element<T>)) {
        let mut children: Vec<SubDot> = self
            .elements
            .iter()
            .filter(|(_, e)| e.predecessor == parent)
            .map(|(id, _)| *id)
            .collect();
        children.sort_by(|a, b| b.cmp(a));
        for child_id in children {
            let child = &self.elements[&child_id];
            visit(child_id, child);
            self.walk_ids(Some(child_id), visit);
        }
    }

    /// Resolve a visible position `pos` (0-indexed) to its element id.
    /// `None` if `pos` is past the end of the live sequence.
    /// Used by [`SequenceMut::Insert`] / [`SequenceMut::Delete`] to
    /// translate user-facing indices into RGA ids.
    fn id_at_visible_pos(&self, pos: usize) -> Option<SubDot> {
        let live = self.live_ids();
        live.get(pos).copied()
    }
}

impl<T> Default for Sequence<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + PartialEq> Lattice for Sequence<T> {
    fn bottom() -> Self {
        Self::new()
    }

    /// State-based join: union of element maps. For elements present in
    /// both sides, prefer the tombstoned version (delete dominates the
    /// pre-delete copy). Idempotent and commutative.
    fn join(&mut self, other: Self) {
        for (id, elem) in other.elements {
            match self.elements.get_mut(&id) {
                None => {
                    self.elements.insert(id, elem);
                }
                Some(existing) => {
                    if elem.tombstoned {
                        existing.tombstoned = true;
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SequenceDelta<T> {
    Insert {
        predecessor: Option<SubDot>,
        value: T,
    },
    Delete {
        targets: Vec<SubDot>,
    },
}

/// User-facing mutation API. Positions are visible-sequence indices;
/// the mutation translates them into RGA ids on the spot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SequenceMut<T> {
    /// Insert `value` at visible position `pos`. `pos == 0` means
    /// before all live elements; `pos == len()` appends at the end.
    InsertAt { pos: usize, value: T },
    /// Delete `len` live elements starting at visible position `pos`.
    DeleteAt { pos: usize, len: usize },
}

impl<T> Crdt for Sequence<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Mutation = SequenceMut<T>;
    type Delta = SequenceDelta<T>;

    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError> {
        match delta {
            SequenceDelta::Insert { predecessor, value } => {
                let id = SubDot::new(ctx.op_id, 0);
                self.elements.insert(
                    id,
                    Element {
                        value: value.clone(),
                        tombstoned: false,
                        predecessor: *predecessor,
                    },
                );
                Ok(())
            }
            SequenceDelta::Delete { targets } => {
                for target in targets {
                    if let Some(elem) = self.elements.get_mut(target) {
                        elem.tombstoned = true;
                    }
                }
                Ok(())
            }
        }
    }

    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta {
        match m {
            SequenceMut::InsertAt { pos, value } => {
                // pos=0 → predecessor=None; pos=k → predecessor = id at visible position k-1.
                let predecessor = if pos == 0 {
                    None
                } else {
                    self.id_at_visible_pos(pos.saturating_sub(1))
                };
                let id = SubDot::new(ctx.op_id, 0);
                self.elements.insert(
                    id,
                    Element {
                        value: value.clone(),
                        tombstoned: false,
                        predecessor,
                    },
                );
                SequenceDelta::Insert {
                    predecessor,
                    value,
                }
            }
            SequenceMut::DeleteAt { pos, len } => {
                let live = self.live_ids();
                let end = pos.saturating_add(len).min(live.len());
                let targets: Vec<SubDot> = live[pos.min(live.len())..end].to_vec();
                for target in &targets {
                    if let Some(elem) = self.elements.get_mut(target) {
                        elem.tombstoned = true;
                    }
                }
                SequenceDelta::Delete { targets }
            }
        }
    }
}

impl<T: Serialize> From<SequenceDelta<T>> for WireDelta {
    fn from(d: SequenceDelta<T>) -> Self {
        match d {
            SequenceDelta::Insert { predecessor, value } => WireDelta::SequenceInsert {
                predecessor,
                value: postcard::to_allocvec(&value)
                    .expect("Sequence value must be serializable"),
            },
            SequenceDelta::Delete { targets } => WireDelta::SequenceDelete { targets },
        }
    }
}

impl<T: DeserializeOwned> TryFrom<WireDelta> for SequenceDelta<T> {
    type Error = DeltaError;
    fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
        match w {
            WireDelta::SequenceInsert { predecessor, value } => {
                let v = postcard::from_bytes(&value).map_err(|e| DeltaError::Invalid {
                    reason: format!("SequenceDelta::Insert decode: {e}"),
                })?;
                Ok(SequenceDelta::Insert {
                    predecessor,
                    value: v,
                })
            }
            WireDelta::SequenceDelete { targets } => Ok(SequenceDelta::Delete { targets }),
            other => Err(DeltaError::TypeMismatch {
                reason: format!("expected SequenceInsert / SequenceDelete, got {other:?}"),
            }),
        }
    }
}

impl<T> SchemaApply for Sequence<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn apply_wire(
        &mut self,
        path: &Path,
        delta: WireDelta,
        ctx: &CausalContext,
    ) -> Result<(), DeltaError> {
        if !path.is_empty() {
            return Err(DeltaError::Invalid {
                reason: format!(
                    "Sequence leaf got non-empty path tail: {} segments remaining",
                    path.len()
                ),
            });
        }
        let typed: SequenceDelta<T> = delta.try_into()?;
        self.apply(&typed, ctx)
    }
}

impl<T: Serialize> IntoWireOp for SequenceDelta<T> {
    fn into_wire_op(self) -> (Path, WireDelta) {
        (Path::new(), self.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CausalState;
    use crate::id::CrdtId;

    fn ctx_at(state: &mut CausalState, peer: u32, seq: u64) -> CausalContext<'_> {
        CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
    }

    /// Simulate two peers exchanging ops in a given total order.
    fn replay_to_peer<T: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static>(
        target: &mut Sequence<T>,
        ops: &[(CrdtId, SequenceDelta<T>)],
    ) {
        let mut state = CausalState::new();
        for (op_id, delta) in ops {
            let ctx = CausalContext::new(*op_id, Some(op_id.seq), &mut state);
            target.apply(delta, &ctx).unwrap();
        }
    }

    #[test]
    fn insert_at_head_then_tail() {
        let mut s = Sequence::<u32>::new();
        let mut state = CausalState::new();
        s.mutate(
            SequenceMut::InsertAt { pos: 0, value: 1 },
            &mut ctx_at(&mut state, 1, 1),
        );
        s.mutate(
            SequenceMut::InsertAt { pos: 1, value: 2 },
            &mut ctx_at(&mut state, 1, 2),
        );
        s.mutate(
            SequenceMut::InsertAt { pos: 2, value: 3 },
            &mut ctx_at(&mut state, 1, 3),
        );
        assert_eq!(s.to_vec(), vec![1, 2, 3]);
    }

    #[test]
    fn insert_in_middle() {
        let mut s = Sequence::<u32>::new();
        let mut state = CausalState::new();
        s.mutate(
            SequenceMut::InsertAt { pos: 0, value: 1 },
            &mut ctx_at(&mut state, 1, 1),
        );
        s.mutate(
            SequenceMut::InsertAt { pos: 1, value: 3 },
            &mut ctx_at(&mut state, 1, 2),
        );
        s.mutate(
            SequenceMut::InsertAt { pos: 1, value: 2 },
            &mut ctx_at(&mut state, 1, 3),
        );
        assert_eq!(s.to_vec(), vec![1, 2, 3]);
    }

    #[test]
    fn delete_range() {
        let mut s = Sequence::<u32>::new();
        let mut state = CausalState::new();
        for (i, v) in [10u32, 20, 30, 40].iter().enumerate() {
            s.mutate(
                SequenceMut::InsertAt {
                    pos: i,
                    value: *v,
                },
                &mut ctx_at(&mut state, 1, u64::try_from(i + 1).unwrap()),
            );
        }
        s.mutate(
            SequenceMut::DeleteAt { pos: 1, len: 2 },
            &mut ctx_at(&mut state, 1, 5),
        );
        assert_eq!(s.to_vec(), vec![10, 40]);
    }

    /// Two peers concurrently insert different values into an empty
    /// sequence at position 0. The merged result preserves both
    /// values in deterministic order.
    #[test]
    fn concurrent_inserts_at_head_converge() {
        // Peer 1 inserts "A" at pos 0 with op (1, 1).
        // Peer 2 inserts "B" at pos 0 with op (2, 1).
        let mut peer1 = Sequence::<String>::new();
        let mut state1 = CausalState::new();
        let d1 = peer1.mutate(
            SequenceMut::InsertAt {
                pos: 0,
                value: "A".to_string(),
            },
            &mut ctx_at(&mut state1, 1, 1),
        );

        let mut peer2 = Sequence::<String>::new();
        let mut state2 = CausalState::new();
        let d2 = peer2.mutate(
            SequenceMut::InsertAt {
                pos: 0,
                value: "B".to_string(),
            },
            &mut ctx_at(&mut state2, 2, 1),
        );

        // Cross-deliver in opposite orders.
        replay_to_peer(&mut peer1, &[(CrdtId::new(2, 1), d2.clone())]);
        replay_to_peer(&mut peer2, &[(CrdtId::new(1, 1), d1.clone())]);

        // Both peers should converge to the same sequence. RGA orders
        // siblings descending by id, so peer (2, 1) > peer (1, 1) →
        // "B" comes first.
        assert_eq!(peer1.to_vec(), peer2.to_vec());
        assert_eq!(peer1.to_vec(), vec!["B".to_string(), "A".to_string()]);
    }

    /// Two peers each insert into different positions of an existing
    /// sequence. After cross-delivery both peers see the same merge.
    #[test]
    fn concurrent_inserts_at_different_positions_converge() {
        let mut peer1 = Sequence::<u32>::new();
        let mut peer2 = Sequence::<u32>::new();
        let mut s1 = CausalState::new();
        let mut s2 = CausalState::new();

        // Both peers seed identically.
        let seed_a = peer1.mutate(
            SequenceMut::InsertAt { pos: 0, value: 10 },
            &mut ctx_at(&mut s1, 0, 1),
        );
        let seed_b = peer1.mutate(
            SequenceMut::InsertAt { pos: 1, value: 20 },
            &mut ctx_at(&mut s1, 0, 2),
        );
        replay_to_peer(
            &mut peer2,
            &[
                (CrdtId::new(0, 1), seed_a),
                (CrdtId::new(0, 2), seed_b),
            ],
        );
        assert_eq!(peer1.to_vec(), vec![10, 20]);
        assert_eq!(peer2.to_vec(), vec![10, 20]);

        // Peer1 inserts 15 at pos 1 (between 10 and 20).
        // Peer2 inserts 30 at pos 2 (after 20).
        // Concurrent — neither has seen the other.
        let d1 = peer1.mutate(
            SequenceMut::InsertAt { pos: 1, value: 15 },
            &mut ctx_at(&mut s1, 1, 3),
        );
        let d2 = peer2.mutate(
            SequenceMut::InsertAt { pos: 2, value: 30 },
            &mut ctx_at(&mut s2, 2, 3),
        );

        replay_to_peer(&mut peer1, &[(CrdtId::new(2, 3), d2)]);
        replay_to_peer(&mut peer2, &[(CrdtId::new(1, 3), d1)]);

        assert_eq!(peer1.to_vec(), peer2.to_vec());
        assert_eq!(peer1.to_vec(), vec![10, 15, 20, 30]);
    }

    /// Concurrent insert and delete of the same predecessor neighborhood
    /// converges deterministically.
    #[test]
    fn concurrent_insert_after_delete_converges() {
        let mut peer1 = Sequence::<u32>::new();
        let mut peer2 = Sequence::<u32>::new();
        let mut s1 = CausalState::new();
        let mut s2 = CausalState::new();

        let seed = peer1.mutate(
            SequenceMut::InsertAt { pos: 0, value: 1 },
            &mut ctx_at(&mut s1, 0, 1),
        );
        replay_to_peer(&mut peer2, &[(CrdtId::new(0, 1), seed)]);
        assert_eq!(peer1.to_vec(), vec![1]);
        assert_eq!(peer2.to_vec(), vec![1]);

        // Peer1 deletes 1; peer2 concurrently inserts 2 after 1.
        let del = peer1.mutate(
            SequenceMut::DeleteAt { pos: 0, len: 1 },
            &mut ctx_at(&mut s1, 1, 2),
        );
        let ins = peer2.mutate(
            SequenceMut::InsertAt { pos: 1, value: 2 },
            &mut ctx_at(&mut s2, 2, 2),
        );

        replay_to_peer(&mut peer1, &[(CrdtId::new(2, 2), ins)]);
        replay_to_peer(&mut peer2, &[(CrdtId::new(1, 2), del)]);

        // Final state: 1 is tombstoned, 2 still attached after the
        // (tombstoned) 1. Visible sequence is just [2].
        assert_eq!(peer1.to_vec(), peer2.to_vec());
        assert_eq!(peer1.to_vec(), vec![2]);
    }

    #[test]
    fn join_idempotent_under_duplicate_state() {
        let mut s = Sequence::<u32>::new();
        let mut state = CausalState::new();
        s.mutate(
            SequenceMut::InsertAt { pos: 0, value: 1 },
            &mut ctx_at(&mut state, 1, 1),
        );
        let snap = s.clone();
        s.join(snap);
        assert_eq!(s.to_vec(), vec![1]);
    }

    #[test]
    fn wire_round_trip_insert() {
        let d = SequenceDelta::Insert {
            predecessor: Some(SubDot::new(CrdtId::new(1, 5), 0)),
            value: "x".to_string(),
        };
        let w: WireDelta = d.clone().into();
        let back: SequenceDelta<String> = w.try_into().unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn wire_round_trip_delete() {
        let d: SequenceDelta<u32> = SequenceDelta::Delete {
            targets: vec![
                SubDot::new(CrdtId::new(1, 1), 0),
                SubDot::new(CrdtId::new(1, 2), 0),
            ],
        };
        let w: WireDelta = d.clone().into();
        let back: SequenceDelta<u32> = w.try_into().unwrap();
        assert_eq!(d, back);
    }
}
