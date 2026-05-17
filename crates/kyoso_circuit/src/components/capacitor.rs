//! Capacitor — two-terminal passive component, parameter
//! `capacitance_farads`. `Default` is zero-valued; the place tool
//! supplies a 1 µF preset on spawn.

use bevy::prelude::*;
use kyoso_sync::SchemaSync;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Capacitor")]
pub struct Capacitor {
    pub capacitance_farads: f32,
}
