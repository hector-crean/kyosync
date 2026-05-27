//! `Frame`: rectangular container with optional auto-layout.
//!
//! Unifies Figma's `FRAME` and `GROUP` per the "opinionated, Bevy-native"
//! decision. The difference is `clips_content`:
//! Figma's `Frame` clips its children to its bounds; `Group` doesn't.
//! `LayoutMode` covers Figma's auto-layout direction (none / horizontal
//! / vertical).
//!
//! Frame entities also carry `Transform` (position+rotation+scale),
//! `Size` (width+height), `kyoso_graph::tree::TreeParent` +
//! `OrderKey` for hierarchy, and `crate::SceneNode` (the structural
//! marker).
//!
//! This module is the single home for the Frame variant: marker
//! [`Frame`] component, owned [`FrameData`] bundle, borrowed
//! [`FrameQueryData`] projection, and [`NodeVariant`] impl wiring them
//! to the [`SceneNode`](crate::SceneNode) typed graph.

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
#[require(NodeKind = NodeKind::Frame)]
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

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Reflect, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum LayoutMode {
    #[default]
    None,
    Horizontal,
    Vertical,
}

/// Owned Frame variant payload + Bevy `Bundle`. Spawn-side mirror of
/// [`FrameQueryData`].
///
/// The `#[require(NodeKind = NodeKind::Frame)]` on [`Frame`] auto-inserts
/// the discriminator, so the sync layer's per-component ops produce the
/// same archetype as the local `*Data` spawn path.
#[derive(Bundle, Default, Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
pub struct FrameData {
    pub frame: Frame,
    pub size: Size,
}

/// Borrowed projection for typed queries. Plugs into
/// `kyoso_graph::GraphQuery<FrameQueryData, _>`.
#[derive(QueryData)]
pub struct FrameQueryData {
    pub entity: Entity,
    pub frame: &'static Frame,
    pub size: &'static Size,
    pub kind: &'static NodeKind,
}

impl NodeVariant for Frame {
    type Graph = SceneNode;
    type Data = FrameData;
    type Query = FrameQueryData;
    const KIND: NodeKind = NodeKind::Frame;

    fn wrap(data: FrameData) -> Node {
        Node::Frame(data)
    }

    fn materialize(item: ROQueryItem<'_, '_, FrameQueryData>) -> FrameData {
        FrameData {
            frame: item.frame.clone(),
            size: item.size.clone(),
        }
    }
}
