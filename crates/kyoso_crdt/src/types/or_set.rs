//! Observed-Remove (add-wins) Set.
//!
//! Each element is paired with the set of [`SubDot`]s that added it; a
//! remove targets the dots it has *observed*. Concurrent add-vs-remove
//! resolves to add (the new add's dot is unobservable to the concurrent
//! remove).
//!
//! In kyoso's server-mediated total order, the per-add tag is the outer
//! op's [`CrdtId`](crate::CrdtId) (sub = 0). Multi-delta-per-op would
//! require sub-dot allocation; current design is one delta per op.

use std::collections::{BTreeSet, HashMap};
use std::hash::Hash;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::context::{CausalContext, SubDot};
use crate::delta::{Path, WireDelta};
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::schema::{IntoWireOp, SchemaApply};

/// Add-wins set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrSet<T: Eq + Hash + Ord> {
    /// element → set of dots that added it.
    adds: HashMap<T, BTreeSet<SubDot>>,
    /// observed-and-removed dots (tombstones at the dot level).
    removes: BTreeSet<SubDot>,
}

impl<T: Eq + Hash + Ord> OrSet<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            adds: HashMap::new(),
            removes: BTreeSet::new(),
        }
    }

    /// True iff at least one live (un-removed) add tag exists for `item`.
    pub fn contains(&self, item: &T) -> bool {
        self.adds
            .get(item)
            .is_some_and(|tags| tags.iter().any(|t| !self.removes.contains(t)))
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.adds.iter().filter_map(|(item, tags)| {
            if tags.iter().any(|t| !self.removes.contains(t)) {
                Some(item)
            } else {
                None
            }
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
}

impl<T: Eq + Hash + Ord> Default for OrSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Eq + Hash + Ord> Lattice for OrSet<T> {
    fn bottom() -> Self {
        Self::new()
    }

    fn join(&mut self, other: Self) {
        for (item, tags) in other.adds {
            self.adds.entry(item).or_default().extend(tags);
        }
        self.removes.extend(other.removes);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrSetDelta<T> {
    Add { value: T },
    Remove { observed: Vec<SubDot> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrSetMut<T> {
    Add(T),
    Remove(T),
}

impl<T> Crdt for OrSet<T>
where
    T: Clone + Eq + Hash + Ord + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Mutation = OrSetMut<T>;
    type Delta = OrSetDelta<T>;

    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError> {
        match delta {
            OrSetDelta::Add { value } => {
                let tag = SubDot::new(ctx.op_id, 0);
                self.adds.entry(value.clone()).or_default().insert(tag);
            }
            OrSetDelta::Remove { observed } => {
                self.removes.extend(observed.iter().copied());
            }
        }
        Ok(())
    }

    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta {
        match m {
            OrSetMut::Add(value) => {
                let tag = SubDot::new(ctx.op_id, 0);
                self.adds.entry(value.clone()).or_default().insert(tag);
                OrSetDelta::Add { value }
            }
            OrSetMut::Remove(value) => {
                let observed: Vec<SubDot> = self
                    .adds
                    .get(&value)
                    .map(|tags| {
                        tags.iter()
                            .filter(|t| !self.removes.contains(t))
                            .copied()
                            .collect()
                    })
                    .unwrap_or_default();
                for tag in &observed {
                    self.removes.insert(*tag);
                }
                OrSetDelta::Remove { observed }
            }
        }
    }
}

impl<T: Serialize> IntoWireOp for OrSetDelta<T> {
    fn into_wire_op(self) -> (Path, WireDelta) {
        (Path::new(), self.into())
    }
}

impl<T: Serialize> From<OrSetDelta<T>> for WireDelta {
    fn from(d: OrSetDelta<T>) -> Self {
        match d {
            OrSetDelta::Add { value } => WireDelta::OrSetAdd {
                value: postcard::to_allocvec(&value).expect("OrSet value must be serializable"),
            },
            OrSetDelta::Remove { observed } => WireDelta::OrSetRemove { observed },
        }
    }
}

impl<T> SchemaApply for OrSet<T>
where
    T: Clone + Eq + Hash + Ord + Serialize + DeserializeOwned + Send + Sync + 'static,
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
                    "OrSet leaf got non-empty path tail: {} segments remaining",
                    path.len()
                ),
            });
        }
        let typed: OrSetDelta<T> = delta.try_into()?;
        self.apply(&typed, ctx)
    }
}

impl<T: DeserializeOwned> TryFrom<WireDelta> for OrSetDelta<T> {
    type Error = DeltaError;
    fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
        match w {
            WireDelta::OrSetAdd { value } => {
                let v = postcard::from_bytes(&value).map_err(|e| DeltaError::Invalid {
                    reason: format!("OrSetDelta::Add decode: {e}"),
                })?;
                Ok(OrSetDelta::Add { value: v })
            }
            WireDelta::OrSetRemove { observed } => Ok(OrSetDelta::Remove { observed }),
            other => Err(DeltaError::TypeMismatch {
                reason: format!("expected OrSetAdd / OrSetRemove, got {other:?}"),
            }),
        }
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

    #[test]
    fn add_then_contains() {
        let mut s = OrSet::<u32>::new();
        let mut st = CausalState::new();
        s.apply(&OrSetDelta::Add { value: 7 }, &ctx_at(&mut st, 1, 1))
            .unwrap();
        assert!(s.contains(&7));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn remove_after_observed_add() {
        let mut s = OrSet::<u32>::new();
        let mut st = CausalState::new();
        s.apply(&OrSetDelta::Add { value: 7 }, &ctx_at(&mut st, 1, 1))
            .unwrap();
        let tag = SubDot::new(CrdtId::new(1, 1), 0);
        s.apply(
            &OrSetDelta::Remove { observed: vec![tag] },
            &ctx_at(&mut st, 1, 2),
        )
        .unwrap();
        assert!(!s.contains(&7));
    }

    #[test]
    fn add_wins_when_concurrent() {
        // Two replicas: A adds 5 at op (1,1); B doesn't see it and removes-with-empty-observed at op (2,1).
        // After merging, 5 is still present (B's remove was concurrent, observed nothing).
        let mut a = OrSet::<u32>::new();
        let mut b = OrSet::<u32>::new();
        let mut sa = CausalState::new();
        let mut sb = CausalState::new();
        a.apply(&OrSetDelta::Add { value: 5 }, &ctx_at(&mut sa, 1, 1))
            .unwrap();
        b.apply(
            &OrSetDelta::Remove { observed: vec![] },
            &ctx_at(&mut sb, 2, 1),
        )
        .unwrap();
        a.join(b);
        assert!(a.contains(&5));
    }

    #[test]
    fn join_idempotent() {
        let mut a = OrSet::<u32>::new();
        let mut st = CausalState::new();
        a.apply(&OrSetDelta::Add { value: 1 }, &ctx_at(&mut st, 1, 1))
            .unwrap();
        let snap = a.clone();
        a.join(snap);
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn wire_round_trip_add() {
        let d = OrSetDelta::Add { value: "x".to_string() };
        let w: WireDelta = d.clone().into();
        let back: OrSetDelta<String> = w.try_into().unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn wire_round_trip_remove() {
        let dot = SubDot::new(CrdtId::new(1, 5), 0);
        let d: OrSetDelta<u32> = OrSetDelta::Remove { observed: vec![dot] };
        let w: WireDelta = d.clone().into();
        let back: OrSetDelta<u32> = w.try_into().unwrap();
        assert_eq!(d, back);
    }
}
