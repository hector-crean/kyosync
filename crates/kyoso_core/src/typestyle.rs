//! `TypeStyle`: typography settings for a `Text` node. Derives
//! `SchemaSync` so it can be embedded in `Text` via `#[crdt(nested)]`,
//! making each field independently mergeable (concurrent font-size and
//! font-family edits don't clobber each other).
//!
//! Initial cut: family / size / weight / line-height. Letter spacing,
//! decoration, hyperlink, OpenType features, etc. are deferred.

use bevy::prelude::*;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

#[derive(Component, Clone, Debug, PartialEq, Reflect, SchemaSync, Serialize, Deserialize)]
#[reflect(Component, Default)]
#[schema(name = "TypeStyle")]
pub struct TypeStyle {
    pub font_family: String,
    pub font_size: f32,
    pub font_weight: u32,
    pub line_height: f32,
}

impl Default for TypeStyle {
    fn default() -> Self {
        Self {
            font_family: String::from("Inter"),
            font_size: 16.0,
            font_weight: 400,
            line_height: 1.4,
        }
    }
}
