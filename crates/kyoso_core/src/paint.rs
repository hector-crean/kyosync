//! `Paint`: solid colour / gradient / image fill, used in both
//! `Frame.fills` and `Rectangle.fills` / `strokes`.
//!
//! Plain serde-serializable enum. Stored on the wire as a whole-list
//! `LwwRegister<Vec<Paint>>` (LWW over the full list of fills) — see
//! [`crate::frame::Frame`] for the `#[crdt(lww)]` choice.
//!
//! ## Why LWW over the list (not OrSet over each Paint)
//!
//! `OrSet<Paint>` would let concurrent fill-additions on different
//! peers compose without clobbering, but it requires `Paint: Eq + Hash
//! + Ord` — and Paint contains `f32` colours, which means hand-rolled
//! `f32::to_bits`-based impls. The cost-benefit didn't favour it for
//! the initial cut. When per-fill granularity matters (collaborative
//! gradient stop editing, etc.), revisit by introducing a `PaintSchema`
//! that's itself a SchemaSync type and use `#[crdt(nested)]` on the field.

use serde::{Deserialize, Serialize};

use bevy::prelude::*;

#[derive(Clone, Debug, PartialEq, Reflect, Serialize, Deserialize)]
pub enum Paint {
    Solid {
        /// sRGB linear with alpha, in `[0.0, 1.0]`.
        color: [f32; 4],
    },
    Gradient {
        kind: GradientType,
        stops: Vec<GradientStop>,
    },
    Image {
        /// Server-resolved image key. The actual image bytes are
        /// fetched separately (out of the document's CRDT scope).
        image_ref: String,
        scale_mode: ImageScaleMode,
    },
}

impl Default for Paint {
    fn default() -> Self {
        Paint::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Reflect, Serialize, Deserialize)]
pub enum GradientType {
    #[default]
    Linear,
    Radial,
    Angular,
    Diamond,
}

#[derive(Clone, Debug, PartialEq, Reflect, Serialize, Deserialize)]
pub struct GradientStop {
    /// `[0.0, 1.0]` along the gradient.
    pub position: f32,
    pub color: [f32; 4],
}

impl Default for GradientStop {
    fn default() -> Self {
        Self {
            position: 0.0,
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Reflect, Serialize, Deserialize)]
pub enum ImageScaleMode {
    #[default]
    Fill,
    Fit,
    Tile,
    Stretch,
}
