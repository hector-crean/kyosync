//! Ground — one-terminal reference; defines the 0 V net of the circuit.
//!
//! Carries a `label` field (e.g. `"GND"`, `"GND1"`) primarily so the
//! `SchemaSync` derive has a field to wire — the typed-schema CRDT path
//! requires at least one. The label is also useful for distinguishing
//! multiple ground references on the same schematic.

use bevy::prelude::*;
use kyoso_graph_sync::SchemaSync;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Ground")]
pub struct Ground {
    pub label: String,
}
