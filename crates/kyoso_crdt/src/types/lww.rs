//! Last-Writer-Wins register.
//!
//! Single-value cell whose conflict resolution is "the op with the larger
//! `(GlobalSeq, PeerId)` stamp wins." Stamp components are derived at
//! apply time from the outer op's identity via [`CausalContext`] — no
//! stamp travels in [`WireDelta::LwwReplace`].
//!
//! Suitable for scalar fields where concurrent writes are rare and
//! "later wins" is the right answer (transforms, names, single-pick
//! enums). For collaboratively-edited text or set-shaped data, prefer
//! [`crate::types::Sequence`] / [`crate::types::OrSet`] instead.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::context::CausalContext;
use crate::delta::{Path, WireDelta};
use crate::id::{GlobalSeq, PeerId};
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::opaque::OpaqueValue;
use crate::schema::{IntoWireOp, SchemaApply};

/// Internal LWW stamp. Total ordering is `(seq, peer)` lexicographic;
/// `None < Some(_)` so a server-confirmed write always beats a still-
/// pending one when those collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Stamp {
    seq: Option<GlobalSeq>,
    peer: PeerId,
}

/// A cell holding at most one stamped value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwRegister<T> {
    inner: Option<(Stamp, T)>,
}

impl<T> LwwRegister<T> {
    #[must_use]
    pub const fn empty() -> Self {
        Self { inner: None }
    }

    pub fn get(&self) -> Option<&T> {
        self.inner.as_ref().map(|(_, v)| v)
    }

    #[must_use]
    pub fn into_inner(self) -> Option<T> {
        self.inner.map(|(_, v)| v)
    }
}

impl<T: Clone + Default> LwwRegister<T> {
    /// Read the current value, or `T::default()` if the register is
    /// empty. Useful in `SchemaSync::diff` impls: comparing
    /// `self.field` against `doc.field.get_or_default()` treats an
    /// un-stamped (bottom) doc value as "default, no opinion", which
    /// avoids emitting echo ops for fields the local replica has never
    /// mutated.
    #[must_use]
    pub fn get_or_default(&self) -> T {
        self.inner
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

impl<T> Default for LwwRegister<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T: Clone + PartialEq> Lattice for LwwRegister<T> {
    fn bottom() -> Self {
        Self::empty()
    }

    fn join(&mut self, other: Self) {
        let take = match (&self.inner, &other.inner) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some((a, _)), Some((b, _))) => b > a,
        };
        if take {
            self.inner = other.inner;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LwwDelta<T> {
    pub value: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LwwMut<T> {
    Set(T),
}

impl<T> Crdt for LwwRegister<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Mutation = LwwMut<T>;
    type Delta = LwwDelta<T>;

    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError> {
        let stamp = Stamp {
            seq: ctx.seq,
            peer: ctx.op_id.peer,
        };
        let take = match &self.inner {
            None => true,
            Some((current, _)) => stamp > *current,
        };
        if take {
            self.inner = Some((stamp, delta.value.clone()));
        }
        Ok(())
    }

    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta {
        let LwwMut::Set(value) = m;
        let stamp = Stamp {
            seq: ctx.seq,
            peer: ctx.op_id.peer,
        };
        self.inner = Some((stamp, value.clone()));
        LwwDelta { value }
    }
}

impl<T: Serialize> IntoWireOp for LwwDelta<T> {
    fn into_wire_op(self) -> (Path, WireDelta) {
        (Path::new(), self.into())
    }
}

impl<T: Serialize> From<LwwDelta<T>> for WireDelta {
    fn from(d: LwwDelta<T>) -> Self {
        WireDelta::LwwReplace {
            value: postcard::to_allocvec(&d.value)
                .expect("LwwRegister value must be serializable"),
        }
    }
}

impl<T> SchemaApply for LwwRegister<T>
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
                    "LwwRegister leaf got non-empty path tail: {} segments remaining",
                    path.len()
                ),
            });
        }
        let typed: LwwDelta<T> = delta.try_into()?;
        self.apply(&typed, ctx)
    }

    fn install_state(&mut self, path: &Path, field: OpaqueValue) -> Result<(), DeltaError> {
        if !path.is_empty() {
            return Err(DeltaError::Invalid {
                reason: format!(
                    "LwwRegister leaf got non-empty path tail in install_state: {} segments",
                    path.len()
                ),
            });
        }
        let OpaqueValue::Lww(byte_reg) = field else {
            return Err(DeltaError::TypeMismatch {
                reason: "expected OpaqueValue::Lww for LwwRegister".to_string(),
            });
        };
        // Decode the opaque bytes back to T using the same postcard
        // encoding the wire op used on the way in.
        self.inner = match byte_reg.inner {
            None => None,
            Some((stamp, bytes)) => {
                let value: T = postcard::from_bytes(&bytes).map_err(|e| DeltaError::Invalid {
                    reason: format!("LwwRegister install_state decode: {e}"),
                })?;
                Some((stamp, value))
            }
        };
        Ok(())
    }
}

impl<T: DeserializeOwned> TryFrom<WireDelta> for LwwDelta<T> {
    type Error = DeltaError;
    fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
        match w {
            WireDelta::LwwReplace { value } => {
                let v = postcard::from_bytes(&value).map_err(|e| DeltaError::Invalid {
                    reason: format!("LwwDelta decode: {e}"),
                })?;
                Ok(LwwDelta { value: v })
            }
            other => Err(DeltaError::TypeMismatch {
                reason: format!("expected LwwReplace, got {other:?}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CausalState;
    use crate::id::CrdtId;

    fn ctx(state: &mut CausalState, peer: PeerId, seq: u64) -> CausalContext<'_> {
        CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
    }

    #[test]
    fn lww_apply_replaces_when_stamp_larger() {
        let mut a = LwwRegister::<u32>::empty();
        let mut s = CausalState::new();
        a.apply(&LwwDelta { value: 1 }, &ctx(&mut s, 1, 1)).unwrap();
        assert_eq!(a.get(), Some(&1));
        a.apply(&LwwDelta { value: 2 }, &ctx(&mut s, 1, 5)).unwrap();
        assert_eq!(a.get(), Some(&2));
    }

    #[test]
    fn lww_apply_keeps_existing_when_stamp_smaller() {
        let mut a = LwwRegister::<u32>::empty();
        let mut s = CausalState::new();
        a.apply(&LwwDelta { value: 5 }, &ctx(&mut s, 1, 10)).unwrap();
        a.apply(&LwwDelta { value: 1 }, &ctx(&mut s, 1, 5)).unwrap();
        assert_eq!(a.get(), Some(&5));
    }

    #[test]
    fn lww_apply_idempotent() {
        let mut a = LwwRegister::<u32>::empty();
        let mut s = CausalState::new();
        let d = LwwDelta { value: 7 };
        a.apply(&d, &ctx(&mut s, 1, 3)).unwrap();
        a.apply(&d, &ctx(&mut s, 1, 3)).unwrap();
        assert_eq!(a.get(), Some(&7));
    }

    #[test]
    fn lww_join_pointwise() {
        let mut a = LwwRegister::<u32>::empty();
        let mut b = LwwRegister::<u32>::empty();
        let mut s1 = CausalState::new();
        let mut s2 = CausalState::new();
        a.apply(&LwwDelta { value: 1 }, &ctx(&mut s1, 1, 1)).unwrap();
        b.apply(&LwwDelta { value: 2 }, &ctx(&mut s2, 2, 2)).unwrap();
        a.join(b);
        assert_eq!(a.get(), Some(&2));
    }

    #[test]
    fn lww_wire_round_trip() {
        let d = LwwDelta { value: "hi".to_string() };
        let w: WireDelta = d.clone().into();
        let back: LwwDelta<String> = w.try_into().unwrap();
        assert_eq!(d, back);
    }
}
