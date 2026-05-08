//! String-keyed map of CRDT values — the central composition combinator.
//!
//! `CausalMap<V>` lets one CRDT value hang off each string key, with the
//! map itself being a CRDT (the join applies pointwise via `V::join`).
//! Apply-side ops are `Put` (route inner delta to a value, creating it
//! at lattice bottom if absent) and `Remove` (drop a key entirely).
//!
//! In kyoso's server-mediated total-order model, removed keys are simply
//! dropped — the [`GlobalSeq`](crate::GlobalSeq) ordering linearizes
//! concurrent put-vs-remove deterministically. A purely P2P deployment
//! would need observed-set tracking for true add-wins semantics; the
//! `Remove` delta already carries an `observed` field for that future
//! shape, currently unused.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::context::{CausalContext, SubDot};
use crate::delta::{Path, PathSegment, WireDelta};
use crate::lattice::{Crdt, DeltaError, Lattice};
use crate::schema::{IntoWireOp, SchemaApply};

/// String-keyed map of CRDT values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalMap<V: Crdt> {
    values: HashMap<String, V>,
}

impl<V: Crdt> CausalMap<V> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        self.values.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.values.get_mut(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.values.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl<V: Crdt> Default for CausalMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Crdt> Lattice for CausalMap<V> {
    fn bottom() -> Self {
        Self::new()
    }

    fn join(&mut self, other: Self) {
        for (k, v) in other.values {
            match self.values.get_mut(&k) {
                Some(existing) => existing.join(v),
                None => {
                    self.values.insert(k, v);
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapDelta<VD> {
    Put { key: String, inner: VD },
    Remove { key: String, observed: Vec<SubDot> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapMut<VM> {
    Apply { key: String, mutation: VM },
    Remove { key: String },
}

impl<V> Crdt for CausalMap<V>
where
    V: Crdt + Default + Send + Sync + 'static,
    V::Delta: Clone,
    V::Mutation: Send + Sync + 'static,
{
    type Mutation = MapMut<V::Mutation>;
    type Delta = MapDelta<V::Delta>;

    fn apply(&mut self, delta: &Self::Delta, ctx: &CausalContext) -> Result<(), DeltaError> {
        match delta {
            MapDelta::Put { key, inner } => {
                let v = self.values.entry(key.clone()).or_insert_with(V::bottom);
                v.apply(inner, ctx)
            }
            MapDelta::Remove { key, .. } => {
                self.values.remove(key);
                Ok(())
            }
        }
    }

    fn mutate(&mut self, m: Self::Mutation, ctx: &mut CausalContext) -> Self::Delta {
        match m {
            MapMut::Apply { key, mutation } => {
                let v = self.values.entry(key.clone()).or_insert_with(V::bottom);
                let inner = v.mutate(mutation, ctx);
                MapDelta::Put { key, inner }
            }
            MapMut::Remove { key } => {
                self.values.remove(&key);
                MapDelta::Remove {
                    key,
                    observed: Vec::new(),
                }
            }
        }
    }
}

impl<VD> From<MapDelta<VD>> for WireDelta
where
    VD: Into<WireDelta>,
{
    fn from(d: MapDelta<VD>) -> Self {
        match d {
            MapDelta::Put { key, inner } => WireDelta::MapPut {
                key: PathSegment::Key(key),
                inner: Box::new(inner.into()),
            },
            MapDelta::Remove { key, observed } => WireDelta::MapRemove {
                key: PathSegment::Key(key),
                observed,
            },
        }
    }
}

impl<V> SchemaApply for CausalMap<V>
where
    V: Crdt + SchemaApply + Default + Send + Sync + 'static,
    V::Delta: Clone,
    V::Mutation: Send + Sync + 'static,
{
    /// Path-driven dispatch into a [`CausalMap`]. The head segment is
    /// the dynamic map key; the tail recurses into the value's own
    /// `apply_wire`. A leaf-level `WireDelta::MapRemove` (tail empty)
    /// drops the entry — this is the path-driven counterpart of the
    /// typed [`MapDelta::Remove`].
    fn apply_wire(
        &mut self,
        path: &Path,
        delta: WireDelta,
        ctx: &CausalContext,
    ) -> Result<(), DeltaError> {
        let (head, tail) = path.split_first().ok_or_else(|| DeltaError::Invalid {
            reason: "CausalMap apply_wire requires a path with at least the dynamic key"
                .to_string(),
        })?;
        let key = match head {
            PathSegment::Field(s) | PathSegment::Key(s) => s.clone(),
        };
        if tail.0.is_empty() {
            if let WireDelta::MapRemove { .. } = &delta {
                self.values.remove(&key);
                return Ok(());
            }
        }
        let entry = self.values.entry(key).or_insert_with(V::bottom);
        entry.apply_wire(&tail, delta, ctx)
    }
}

/// Path-driven wire-op shape for `MapDelta`. Composes with the outer
/// schema's `IntoWireOp` (generated by `derive(Crdt)`): the outer impl
/// prepends its field-name segment, this impl prepends the dynamic map
/// key, and the inner `V::Delta`'s impl produces the leaf wire delta.
///
/// - `Put { key, inner }`: emits `path = [key, ...inner_path]` with the
///   inner's wire delta as the leaf. Receive-side `apply_wire` walks
///   key → value → leaf.
/// - `Remove { key, observed }`: emits `path = [key]` with
///   `WireDelta::MapRemove`. The wire-side `key` field is left empty —
///   the authoritative key lives in the path. `CausalMap::apply_wire`
///   detects this leaf form and drops the entry.
impl<VD> IntoWireOp for MapDelta<VD>
where
    VD: IntoWireOp,
{
    fn into_wire_op(self) -> (Path, WireDelta) {
        match self {
            MapDelta::Put { key, inner } => {
                let (inner_path, wire) = inner.into_wire_op();
                let mut path = Path::new();
                path.0.push(PathSegment::Key(key));
                for seg in inner_path.0 {
                    path.0.push(seg);
                }
                (path, wire)
            }
            MapDelta::Remove { key, observed } => {
                let mut path = Path::new();
                path.0.push(PathSegment::Key(key));
                // Leaf wire is MapRemove with empty key (the real key
                // lives in the path).
                let wire = WireDelta::MapRemove {
                    key: PathSegment::Key(String::new()),
                    observed,
                };
                (path, wire)
            }
        }
    }
}

impl<VD> TryFrom<WireDelta> for MapDelta<VD>
where
    VD: TryFrom<WireDelta, Error = DeltaError>,
{
    type Error = DeltaError;
    fn try_from(w: WireDelta) -> Result<Self, Self::Error> {
        match w {
            WireDelta::MapPut { key, inner } => {
                let key = match key {
                    PathSegment::Key(s) | PathSegment::Field(s) => s,
                };
                let inner = VD::try_from(*inner)?;
                Ok(MapDelta::Put { key, inner })
            }
            WireDelta::MapRemove { key, observed } => {
                let key = match key {
                    PathSegment::Key(s) | PathSegment::Field(s) => s,
                };
                Ok(MapDelta::Remove { key, observed })
            }
            other => Err(DeltaError::TypeMismatch {
                reason: format!("expected MapPut / MapRemove, got {other:?}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::CausalState;
    use crate::id::CrdtId;
    use crate::types::lww::{LwwDelta, LwwMut, LwwRegister};

    fn ctx_at(state: &mut CausalState, peer: u32, seq: u64) -> CausalContext<'_> {
        CausalContext::new(CrdtId::new(peer, seq), Some(seq), state)
    }

    #[test]
    fn put_then_get() {
        let mut m: CausalMap<LwwRegister<u32>> = CausalMap::new();
        let mut s = CausalState::new();
        let d = MapDelta::Put {
            key: "x".to_string(),
            inner: LwwDelta { value: 1u32 },
        };
        m.apply(&d, &ctx_at(&mut s, 1, 1)).unwrap();
        assert_eq!(m.get("x").and_then(LwwRegister::get), Some(&1));
    }

    #[test]
    fn remove_drops_key() {
        let mut m: CausalMap<LwwRegister<u32>> = CausalMap::new();
        let mut s = CausalState::new();
        m.apply(
            &MapDelta::Put {
                key: "x".to_string(),
                inner: LwwDelta { value: 1u32 },
            },
            &ctx_at(&mut s, 1, 1),
        )
        .unwrap();
        m.apply(
            &MapDelta::Remove {
                key: "x".to_string(),
                observed: vec![],
            },
            &ctx_at(&mut s, 1, 2),
        )
        .unwrap();
        assert!(m.get("x").is_none());
    }

    #[test]
    fn join_pointwise_lww() {
        let mut a: CausalMap<LwwRegister<u32>> = CausalMap::new();
        let mut b: CausalMap<LwwRegister<u32>> = CausalMap::new();
        let mut sa = CausalState::new();
        let mut sb = CausalState::new();
        a.apply(
            &MapDelta::Put {
                key: "x".to_string(),
                inner: LwwDelta { value: 1u32 },
            },
            &ctx_at(&mut sa, 1, 1),
        )
        .unwrap();
        b.apply(
            &MapDelta::Put {
                key: "y".to_string(),
                inner: LwwDelta { value: 2u32 },
            },
            &ctx_at(&mut sb, 2, 1),
        )
        .unwrap();
        a.join(b);
        assert_eq!(a.get("x").and_then(LwwRegister::get), Some(&1));
        assert_eq!(a.get("y").and_then(LwwRegister::get), Some(&2));
    }

    #[test]
    fn mutate_round_trips_through_wire() {
        let mut map: CausalMap<LwwRegister<u32>> = CausalMap::new();
        let mut state = CausalState::new();
        let mut cx = ctx_at(&mut state, 1, 1);
        let delta = map.mutate(
            MapMut::Apply {
                key: "x".to_string(),
                mutation: LwwMut::Set(7u32),
            },
            &mut cx,
        );
        let wire: WireDelta = delta.clone().into();
        let back: MapDelta<LwwDelta<u32>> = wire.try_into().unwrap();
        assert_eq!(delta, back);
    }
}
