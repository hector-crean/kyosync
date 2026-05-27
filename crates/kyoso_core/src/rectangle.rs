//! `Rectangle`: primitive shape with corner-radius + fills + strokes.
//!
//! Initial cut uses a single `corner_radius`; per-corner radii
//! (`top_left_radius`, ...) are a follow-up.
//!
//! This module is the single home for the Rectangle variant: marker
//! [`Rectangle`] component, owned [`RectangleData`] bundle, borrowed
//! [`RectangleQueryData`] projection, and [`NodeVariant`] impl.

use bevy::ecs::query::{QueryData, ROQueryItem};
use bevy::prelude::*;
use kyoso_graph::NodeVariant;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

use crate::node::Node;
use crate::paint::Paint;
use crate::size::Size;
use crate::{NodeKind, SceneNode};

#[derive(
    Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync, Serialize, Deserialize,
    schemars::JsonSchema,
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

/// Owned Rectangle variant payload + Bevy `Bundle`.
#[derive(Bundle, Default, Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
pub struct RectangleData {
    pub rectangle: Rectangle,
    pub size: Size,
}

/// Borrowed projection for typed queries.
#[derive(QueryData)]
pub struct RectangleQueryData {
    pub entity: Entity,
    pub rectangle: &'static Rectangle,
    pub size: &'static Size,
    pub kind: &'static NodeKind,
}

impl NodeVariant for Rectangle {
    type Graph = SceneNode;
    type Data = RectangleData;
    type Query = RectangleQueryData;
    const KIND: NodeKind = NodeKind::Rectangle;

    fn wrap(data: RectangleData) -> Node {
        Node::Rectangle(data)
    }

    fn materialize(item: ROQueryItem<'_, '_, RectangleQueryData>) -> RectangleData {
        RectangleData {
            rectangle: item.rectangle.clone(),
            size: item.size.clone(),
        }
    }
}
