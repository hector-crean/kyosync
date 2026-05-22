//! `Rectangle`: primitive shape with corner-radius + fills + strokes.
//!
//! Initial cut uses a single `corner_radius`; per-corner radii
//! (`top_left_radius`, ...) are a follow-up (plan doc Part XI §XI.7).

use bevy::prelude::*;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

use crate::paint::Paint;
use crate::NodeKind;

#[derive(
    Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync, Serialize, Deserialize,
)]
#[reflect(Component, Default)]
#[require(NodeKind = NodeKind::Rectangle)]
#[schema(name = "Rectangle")]
pub struct Rectangle {
    pub corner_radius: f32,
    pub fills: Vec<Paint>,
    pub strokes: Vec<Paint>,
    pub stroke_weight: f32,
}
