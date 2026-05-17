//! Model-agnostic typed-schema trait layer.
//!
//! These traits bridge a Bevy component to a `derive(Crdt)` schema
//! struct. They are **not** graph-specific — they depend only on
//! [`kyoso_crdt`] and [`bevy`], so any model (graph nodes, graph edges,
//! or a future non-graph model) can synchronise its components through
//! them.
//!
//! - [`SchemaSync`] is implemented (usually via `#[derive(SchemaSync)]`)
//!   on the Bevy component: it names the companion schema struct and
//!   carries the outbound `diff` / inbound `write_back` bridge.
//! - [`SchemaField`] is implemented once per CRDT primitive
//!   ([`LwwRegister`], [`OrSet`], [`PnCounter`], [`CausalMap`],
//!   [`Sequence`]); it owns the per-kind diff/projection logic that the
//!   `derive(SchemaSync)`-generated code delegates to, one call per
//!   field.
//!
//! The *pipeline* that drives these traits (change detection, wire
//! routing, projection) is model-specific and lives in the per-model
//! plugin crates (e.g. `kyoso_graph_sync`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;

use bevy::ecs::component::{Component, Mutable};
use kyoso_crdt::types::{
    CausalMap, LwwMut, LwwRegister, MapMut, OrSet, OrSetMut, PnCounter, PnMut, Sequence, SequenceMut,
};
use kyoso_crdt::{Crdt, SchemaApply};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::sequence_diff::sequence_diff;

/// The mutation list produced by [`SchemaSync::diff`] — one CRDT
/// mutation per changed field of component `C`'s schema.
pub type SchemaMutations<C> = Vec<<<C as SchemaSync>::Schema as Crdt>::Mutation>;

/// Bridge between a Bevy component and a `derive(Crdt)` schema struct.
///
/// `PartialEq` on `Schema` is required so a change detector can
/// distinguish "the doc holds real hydrated state" from "the doc is
/// still at its empty/default lattice bottom" — the discriminator that
/// suppresses spurious "reset to default" ops on reconnect.
pub trait SchemaSync:
    Component<Mutability = Mutable> + Default + Sized + Send + Sync + 'static
{
    type Schema: Crdt + SchemaApply + Default + Clone + PartialEq + Send + Sync + 'static;
    const SCHEMA_NAME: &'static str;

    /// Outbound: the CRDT mutations that carry this component's local
    /// edits into `doc` (the replicated schema state).
    fn diff(&self, doc: &Self::Schema) -> SchemaMutations<Self>;

    /// Inbound: overwrite this component with the replicated `schema`.
    fn write_back(&mut self, schema: &Self::Schema);
}

/// Bridges one schema-side CRDT field to its Bevy-side representation.
///
/// Each CRDT primitive — [`LwwRegister`], [`OrSet`], [`Sequence`],
/// [`CausalMap`], [`PnCounter`] — implements this once. The per-kind
/// diff and projection logic (including the LWW echo-guard) lives in
/// those impls rather than being re-inlined per field by
/// `derive(SchemaSync)`, which just delegates here. A `derive(SchemaSync)`
/// type also gets `impl SchemaField<C> for CSchema`, so it composes as a
/// `#[crdt(nested)]` field of another component.
///
/// `#[crdt(with = "Type")]` is the escape hatch: it means "this field's
/// schema type is `Type`, and `Type` carries a hand-written `SchemaField`
/// impl" — for component fields whose Bevy type doesn't fit a built-in
/// CRDT (`Handle<Image>`, `Entity` references, opaque external tokens).
///
/// `Comp` is a type parameter, not an associated type, so one CRDT can
/// bridge several container shapes: [`OrSet<T>`] serves `Vec<T>`,
/// `HashSet<T>` and `BTreeSet<T>`.
pub trait SchemaField<Comp>: Crdt {
    /// Diff a live component value against this field's replicated
    /// state, returning the mutations that bring the doc up to the
    /// component.
    ///
    /// `baseline` is the component's default for this field. Only
    /// *clobbering* CRDTs consult it: [`LwwRegister`] treats an empty
    /// register as holding `baseline`, so a freshly-defaulted component
    /// emits no echo op. Additive CRDTs (set, counter, sequence, map)
    /// ignore it — a redundant op there is a harmless no-op.
    fn diff(&self, component: &Comp, baseline: &Comp) -> Vec<<Self as Crdt>::Mutation>;

    /// Project replicated state back onto the live component.
    fn project_to(&self, component: &mut Comp);
}

// --- LWW: the one clobbering CRDT; echo-guard lives here -------------------

impl<T> SchemaField<T> for LwwRegister<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn diff(&self, component: &T, baseline: &T) -> Vec<LwwMut<T>> {
        // Echo-guard: an empty register resolves to the component's
        // default, so a freshly-defaulted component emits nothing.
        if self.get().unwrap_or(baseline) != component {
            vec![LwwMut::Set(component.clone())]
        } else {
            Vec::new()
        }
    }

    fn project_to(&self, component: &mut T) {
        if let Some(value) = self.get() {
            *component = value.clone();
        }
    }
}

// --- OrSet: set-diff, additive; one impl per container shape --------------

macro_rules! impl_or_set_field {
    ($container:ident) => {
        impl<T> SchemaField<$container<T>> for OrSet<T>
        where
            T: Clone + Eq + Hash + Ord + Serialize + DeserializeOwned + Send + Sync + 'static,
        {
            fn diff(
                &self,
                component: &$container<T>,
                _baseline: &$container<T>,
            ) -> Vec<OrSetMut<T>> {
                let mut out = Vec::new();
                // Add: present locally, absent from the doc membership.
                for elem in component.iter() {
                    if !self.contains(elem) {
                        out.push(OrSetMut::Add(elem.clone()));
                    }
                }
                // Remove: present in the doc, no longer present locally.
                for elem in self.iter() {
                    if !component.iter().any(|e| e == elem) {
                        out.push(OrSetMut::Remove(elem.clone()));
                    }
                }
                out
            }

            fn project_to(&self, component: &mut $container<T>) {
                *component = self.iter().cloned().collect();
            }
        }
    };
}
impl_or_set_field!(Vec);
impl_or_set_field!(HashSet);
impl_or_set_field!(BTreeSet);

// --- PnCounter: signed diff against the resolved value --------------------

macro_rules! impl_counter_field {
    ($int:ty) => {
        impl SchemaField<$int> for PnCounter {
            fn diff(&self, component: &$int, _baseline: &$int) -> Vec<PnMut> {
                let diff: i64 = (*component as i64) - self.value();
                if diff > 0 {
                    vec![PnMut::Inc(diff as u64)]
                } else if diff < 0 {
                    vec![PnMut::Dec(diff.unsigned_abs())]
                } else {
                    Vec::new()
                }
            }

            fn project_to(&self, component: &mut $int) {
                *component = self.value() as $int;
            }
        }
    };
}
impl_counter_field!(i8);
impl_counter_field!(i16);
impl_counter_field!(i32);
impl_counter_field!(i64);
impl_counter_field!(u8);
impl_counter_field!(u16);
impl_counter_field!(u32);
impl_counter_field!(u64);
impl_counter_field!(isize);
impl_counter_field!(usize);

// --- CausalMap: per-key diff; LWW per value -------------------------------

macro_rules! impl_causal_map_field {
    ($map:ident) => {
        impl<V> SchemaField<$map<String, V>> for CausalMap<LwwRegister<V>>
        where
            V: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static,
        {
            fn diff(
                &self,
                component: &$map<String, V>,
                _baseline: &$map<String, V>,
            ) -> Vec<MapMut<LwwMut<V>>> {
                let mut out = Vec::new();
                // Apply: keys whose component value differs from the doc.
                for (key, value) in component {
                    let doc_value = self.get(key.as_str()).and_then(LwwRegister::get);
                    if doc_value != Some(value) {
                        out.push(MapMut::Apply {
                            key: key.clone(),
                            mutation: LwwMut::Set(value.clone()),
                        });
                    }
                }
                // Remove: keys present in the doc but absent locally.
                for (doc_key, _) in self.iter() {
                    if !component.contains_key(doc_key.as_str()) {
                        out.push(MapMut::Remove { key: doc_key.clone() });
                    }
                }
                out
            }

            fn project_to(&self, component: &mut $map<String, V>) {
                // Keys whose LwwRegister is still bottom are skipped —
                // matches an unobserved key in the original component.
                *component = self
                    .iter()
                    .filter_map(|(k, reg)| reg.get().map(|v| (k.clone(), v.clone())))
                    .collect();
            }
        }
    };
}
impl_causal_map_field!(HashMap);
impl_causal_map_field!(BTreeMap);

// --- Sequence: prefix-suffix diff -----------------------------------------

impl SchemaField<String> for Sequence<char> {
    fn diff(&self, component: &String, _baseline: &String) -> Vec<SequenceMut<char>> {
        let doc: Vec<char> = self.iter().into_iter().copied().collect();
        sequence_diff(doc, component.chars())
    }

    fn project_to(&self, component: &mut String) {
        *component = self.iter().into_iter().copied().collect();
    }
}

impl<T> SchemaField<Vec<T>> for Sequence<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn diff(&self, component: &Vec<T>, _baseline: &Vec<T>) -> Vec<SequenceMut<T>> {
        let doc: Vec<T> = self.iter().into_iter().cloned().collect();
        sequence_diff(doc, component.iter().cloned())
    }

    fn project_to(&self, component: &mut Vec<T>) {
        *component = self.iter().into_iter().cloned().collect();
    }
}
