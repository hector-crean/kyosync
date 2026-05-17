//! Inductor — two-terminal passive component, parameter
//! `inductance_henries`. `Default` is zero-valued; the place tool
//! supplies a 1 mH preset on spawn.

use bevy::prelude::*;
use kyoso_sync::SchemaSync;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Inductor")]
pub struct Inductor {
    pub inductance_henries: f32,
}
