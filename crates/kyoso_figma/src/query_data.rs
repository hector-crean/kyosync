//! Per-variant + shared `QueryData` projections.
//!
//! Plug `FrameQueryData` / `RectangleQueryData` / `TextQueryData` into
//! `kyoso_graph::GraphQuery<V, E, NF, EF>` to traverse the graph with a
//! single variant's data already fetched. Use `AnyNodeQueryData` when
//! you only need the shared discriminator + size (e.g. cross-variant
//! iteration that defers typed access to [`FigmaNodeQuery::get`]).
//!
//! [`FigmaNodeQuery::get`]: crate::node::FigmaNodeQuery::get

use bevy::ecs::query::QueryData;
use bevy::prelude::*;

use crate::frame::Frame;
use crate::rectangle::Rectangle;
use crate::size::Size;
use crate::text::Text;
use crate::NodeKind;

#[derive(QueryData)]
pub struct FrameQueryData {
    pub entity: Entity,
    pub frame: &'static Frame,
    pub size: &'static Size,
    pub kind: &'static NodeKind,
}

#[derive(QueryData)]
pub struct RectangleQueryData {
    pub entity: Entity,
    pub rectangle: &'static Rectangle,
    pub size: &'static Size,
    pub kind: &'static NodeKind,
}

#[derive(QueryData)]
pub struct TextQueryData {
    pub entity: Entity,
    pub text: &'static Text,
    pub size: &'static Size,
    pub kind: &'static NodeKind,
}

/// Variant-agnostic projection: just the discriminator + shared
/// fields. Useful for iterating every Figma node and dispatching to
/// typed lookups in a second pass.
#[derive(QueryData)]
pub struct AnyNodeQueryData {
    pub entity: Entity,
    pub kind: &'static NodeKind,
    pub size: &'static Size,
}
