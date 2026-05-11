//! 2D size component.
//!
//! Bevy's `Transform` is a 4×4 matrix and doesn't carry `width`/`height`
//! for 2D shapes. `Size` fills that gap as a separate Bevy component
//! that node entities (Frame, Rectangle, ...) attach alongside their
//! `Transform`. Per-field LWW.

use bevy::prelude::*;
use kyoso_graph_sync::SchemaSync;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Size")]
pub struct Size {
    pub width: f32,
    pub height: f32,
}
