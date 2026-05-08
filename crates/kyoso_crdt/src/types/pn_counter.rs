//! Positive-Negative Counter.
//!
//! Per-replica monotonic counters: one for additions, one for subtractions.
//! Replica id at apply time is derived from `ctx.op_id.peer`. Idempotency
//! at the apply level is provided by the outer op log (kyoso applies each
//! op exactly once in `GlobalSeq` order); the `Lattice::join` view is
//! pointwise max, naturally idempotent.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::context::CausalContext;
use crate::delta::{Path, WireDelta};
use crate::id::PeerId;
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::schema::{IntoWireOp, SchemaApply};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PnCounter {
    pos: HashMap<PeerId, u64>,
    neg: HashMap<PeerId, u64>,
}

impl PnCounter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Net value: sum of positive contributions minus sum of negative.
    /// Saturates rather than overflowing.
    #[must_use]
    pub fn value(&self) -> i64 {
        let p: u64 = self.pos.values().copied().sum();
        let n: u64 = self.neg.values().copied().sum();
        i64::try_from(p).unwrap_or(i64::MAX).saturating_sub(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl Lattice for PnCounter {
    fn bottom() -> Self {
        Self::new()
    }

    fn join(&mut self, other: Self) {
        for (k, v) in other.pos {
            let entry = self.pos.entry(k).or_insert(0);
            *entry = (*entry).max(v);
        }
        for (k, v) in other.neg {
            let entry = self.neg.entry(k).or_insert(0);
            *entry = (*entry).max(v);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PnDelta {
    pub by: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PnMut {
    Inc(u64),
    Dec(u64),
}

impl Crdt for PnCounter {
    type Mutation = PnMut;
    type Delta = PnDelta;

    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError> {
        let peer = ctx.op_id.peer;
        if delta.by >= 0 {
            *self.pos.entry(peer).or_insert(0) += delta.by.unsigned_abs();
        } else {
            *self.neg.entry(peer).or_insert(0) += delta.by.unsigned_abs();
        }
        Ok(())
    }

    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta {
        let by = match m {
            PnMut::Inc(n) => i64::try_from(n).unwrap_or(i64::MAX),
            PnMut::Dec(n) => -i64::try_from(n).unwrap_or(i64::MAX),
        };
        let peer = ctx.op_id.peer;
        if by >= 0 {
            *self.pos.entry(peer).or_insert(0) += by.unsigned_abs();
        } else {
            *self.neg.entry(peer).or_insert(0) += by.unsigned_abs();
        }
        PnDelta { by }
    }
}

impl IntoWireOp for PnDelta {
    fn into_wire_op(self) -> (Path, WireDelta) {
        (Path::new(), self.into())
    }
}

impl From<PnDelta> for WireDelta {
    fn from(d: PnDelta) -> Self {
        WireDelta::PnCounterDelta { by: d.by }
    }
}

impl SchemaApply for PnCounter {
    fn apply_wire(
        &mut self,
        path: &Path,
        delta: WireDelta,
        ctx: &CausalContext,
    ) -> Result<(), DeltaError> {
        if !path.is_empty() {
            return Err(DeltaError::Invalid {
                reason: format!(
                    "PnCounter leaf got non-empty path tail: {} segments remaining",
                    path.len()
                ),
            });
        }
        let typed: PnDelta = delta.try_into()?;
        self.apply(&typed, ctx)
    }
}

impl TryFrom<WireDelta> for PnDelta {
    type Error = DeltaError;
    fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
        match w {
            WireDelta::PnCounterDelta { by } => Ok(PnDelta { by }),
            other => Err(DeltaError::TypeMismatch {
                reason: format!("expected PnCounterDelta, got {other:?}"),
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
    fn inc_dec() {
        let mut c = PnCounter::new();
        let mut s = CausalState::new();
        c.apply(&PnDelta { by: 3 }, &ctx_at(&mut s, 1, 1)).unwrap();
        c.apply(&PnDelta { by: -1 }, &ctx_at(&mut s, 1, 2)).unwrap();
        assert_eq!(c.value(), 2);
    }

    #[test]
    fn join_pointwise_max() {
        let mut a = PnCounter::new();
        let mut b = PnCounter::new();
        let mut sa = CausalState::new();
        let mut sb = CausalState::new();
        a.apply(&PnDelta { by: 5 }, &ctx_at(&mut sa, 1, 1)).unwrap();
        b.apply(&PnDelta { by: 3 }, &ctx_at(&mut sb, 2, 1)).unwrap();
        a.join(b);
        assert_eq!(a.value(), 8);
    }

    #[test]
    fn join_idempotent() {
        let mut a = PnCounter::new();
        let mut s = CausalState::new();
        a.apply(&PnDelta { by: 5 }, &ctx_at(&mut s, 1, 1)).unwrap();
        let snap = a.clone();
        a.join(snap);
        assert_eq!(a.value(), 5);
    }

    #[test]
    fn wire_round_trip() {
        let d = PnDelta { by: -7 };
        let w: WireDelta = d.into();
        let back: PnDelta = w.try_into().unwrap();
        assert_eq!(d, back);
    }
}
