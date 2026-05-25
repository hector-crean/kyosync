//! Variant-agnostic borrowed projection for the scene typed graph.
//!
//! Per-variant `FrameQueryData` / `RectangleQueryData` / `TextQueryData`
//! live in their respective modules (`frame.rs`, `rectangle.rs`,
//! `text.rs`) alongside their owned `*Data` bundles and `NodeVariant`
//! impls. This module is just the shared "discriminator + size"
//! projection — used when you only need the kind + shared satellite
//! data and want to defer typed access.

use bevy::ecs::query::QueryData;
use bevy::prelude::*;

use crate::size::Size;
use crate::NodeKind;

/// Variant-agnostic projection: just the discriminator + shared
/// fields. Useful for iterating every scene node and dispatching to
/// typed lookups in a second pass.
#[derive(QueryData)]
pub struct AnyNodeQueryData {
    pub entity: Entity,
    pub kind: &'static NodeKind,
    pub size: &'static Size,
}
