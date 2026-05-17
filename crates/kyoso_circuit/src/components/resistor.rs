//! Resistor — two-terminal passive component, parameter
//! `resistance_ohms`.
//!
//! `Default` produces a zero-valued resistor — the SchemaSync derive
//! compares the Bevy component's value against the component's own
//! `Default` to decide which fields are non-default and need
//! replicating. So "default = empty/zero, explicit constructor = sensible
//! preset" is the convention across the kyoso schemas; the place tool
//! supplies the sensible preset (1 kΩ) on spawn.

use bevy::prelude::*;
use kyoso_sync::SchemaSync;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Resistor")]
pub struct Resistor {
    pub resistance_ohms: f32,
}
