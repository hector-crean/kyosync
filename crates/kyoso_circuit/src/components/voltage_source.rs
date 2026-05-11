//! Voltage source — two-terminal active component (DC for the initial
//! cut), parameter `voltage_volts`. Pin 0 is `+`; pin 1 is `−`.
//! `Default` is zero-valued; the place tool supplies a 5 V preset on
//! spawn.

use bevy::prelude::*;
use kyoso_graph_sync::SchemaSync;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "VoltageSource")]
pub struct VoltageSource {
    pub voltage_volts: f32,
}
