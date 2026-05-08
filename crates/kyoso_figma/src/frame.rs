//! `Frame`: rectangular container with optional auto-layout.
//!
//! Unifies Figma's `FRAME` and `GROUP` per the "opinionated, Bevy-native"
//! decision (plan doc Part XI). The difference is `clips_content`:
//! Figma's `Frame` clips its children to its bounds; `Group` doesn't.
//! `LayoutMode` covers Figma's auto-layout direction (none / horizontal
//! / vertical).
//!
//! Frame entities also carry `Transform` (position+rotation+scale),
//! `Size` (width+height), `kyoso_graph::tree::TreeParent` +
//! `OrderKey` for hierarchy, and `crate::FigmaNode` (the structural
//! marker).

use bevy::prelude::*;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

use crate::paint::Paint;

#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "Frame")]
pub struct Frame {
    pub name: String,
    /// Whether children are clipped to the frame's bounds.
    /// Figma `Frame` ⇒ `true`; Figma `Group` ⇒ `false`.
    pub clips_content: bool,
    pub layout_mode: LayoutMode,
    /// Whole-list LWW. Concurrent fill edits on different peers will
    /// not compose — last writer wins on the entire list. See
    /// `paint.rs` for the rationale.
    pub fills: Vec<Paint>,
    pub strokes: Vec<Paint>,
    pub stroke_weight: f32,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Reflect, Serialize, Deserialize)]
pub enum LayoutMode {
    #[default]
    None,
    Horizontal,
    Vertical,
}
