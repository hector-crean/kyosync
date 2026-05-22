//! Bevy-native, opinionated Figma-shaped document model for kyoso.
//!
//! ## What this crate provides
//!
//! - **Component definitions** — `Frame`, `Rectangle`, `Text` and their
//!   value-type satellites `Size`, `Paint`, `TypeStyle`. Each is a Bevy
//!   `Component` with `derive(SchemaSync)` so per-field mutations
//!   replicate via the `kyoso_sync` typed schema path. CRDT-kind choice
//!   per field (LWW for scalars, OrSet for fills, Sequence for collab
//!   text, nested for TypeStyle inside Text).
//! - **Structural markers** — `FigmaNode` and `FigmaEdge` (zero-sized
//!   types) used as the `<N, E>` parameters to
//!   [`kyoso_graph_sync::GraphSyncPlugin`]. Every spawned figma entity
//!   carries `FigmaNode` plus exactly one of the field-bearing components.
//! - **`KyosoFigmaPlugin`** — single-call entry point that registers
//!   all per-component schema plugins.
//! - **Figma import adapter** — `KyosoVisitor` impl over the vendored
//!   `walker::NodeVisitor`. Translates `figma_api` types into Bevy
//!   bundles. One-way, best-effort, log-and-skip on unsupported
//!   variants.
//!
//! ## Opinionated departures from Figma's data model
//!
//! - **Group + Frame unified** as `Frame` with a `clips_content: bool`.
//! - **No `absolute_bounding_box`** — position+rotation+scale via
//!   Bevy `Transform`; size via the standalone `Size` component.
//!   World coords come from `GlobalTransform` on demand.
//! - **Idiomatic Rust field names** — snake_case; `is_clip` →
//!   `clips_content`; etc.
//!
//! ## Initial cut scope
//!
//! Three node types: `Frame`, `Rectangle`, `Text`. `Page` /
//! `Component` / `Instance` / `Vector` / `Group` / `BooleanOperation`
//! and the rest of Figma's surface are deferred — see plan doc Part XI
//! §XI.7.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

pub mod descriptor;
pub mod frame;
pub mod import;
pub mod node;
pub mod paint;
pub mod plugin;
pub mod query_data;
pub mod rectangle;
pub mod size;
pub mod text;
pub mod typestyle;
pub mod visitor;
pub mod walker;

pub use descriptor::{
    build_figma_node_descriptor, build_figma_scene_descriptor, figma_node_descriptor,
    node_payload, node_type_str,
};
pub use frame::{Frame, LayoutMode};
pub use node::{
    FigmaNodeQuery, FrameData, Node, NodeBehavior, RectangleData, TextData,
};
// NodeVariant / EdgeVariant are the per-variant relator traits; re-exported
// from `kyoso_graph` (they live there because they're graph-crate concerns).
pub use kyoso_graph::{EdgeVariant, NodeVariant};
pub use visitor::{SceneVisitor, Traverse, VisitContext};
pub use paint::{GradientStop, GradientType, ImageScaleMode, Paint};
pub use plugin::KyosoFigmaPlugin;
pub use query_data::{AnyNodeQueryData, FrameQueryData, RectangleQueryData, TextQueryData};
pub use rectangle::Rectangle;
pub use size::Size;
pub use text::Text;
pub use typestyle::TypeStyle;
pub use walker::{NodeContext, NodeVisitor, SubcanvasNodeExt, Walker};

// ---------------------------------------------------------------------------
// Structural markers
// ---------------------------------------------------------------------------

/// Zero-sized marker: every kyoso_figma node entity carries this. Used
/// as the `N` parameter to [`kyoso_graph_sync::GraphSyncPlugin`] so
/// structural ops (`AddNode`, `RemoveNode`, `Move`) operate on a
/// uniform "this is a figma node" identity, decoupled from which
/// specific node kind (`Frame` / `Rectangle` / `Text` / ...) the
/// entity carries.
#[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
#[require(kyoso_graph_sync::NodePresence)]
pub struct FigmaNode;

/// Discriminator tag: which variant of [`Node`](crate::node::Node) does
/// this entity carry? Inserted atomically with the variant's data via
/// the `*Data` bundles (see [`FrameData`](crate::node::FrameData) etc.)
/// so the tag and the data cannot drift apart.
#[derive(
    Component, Reflect, Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Frame,
    Rectangle,
    Text,
}

/// Zero-sized marker: every kyoso_figma edge entity carries this. The
/// initial cut only uses tree edges (via `kyoso_graph::tree`);
/// reference edges (component→main, prototype links) are deferred.
#[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
#[require(kyoso_graph_sync::EdgeEndpoints)]
pub struct FigmaEdge;

// ---------------------------------------------------------------------------
// Typed-graph trait impl — FigmaNode IS the typed graph identifier.
// One `impl Graph for FigmaNode` replaces the previous separate
// `TypedGraphNode` + `TypedGraphEdge` impls.
// ---------------------------------------------------------------------------

impl kyoso_graph::Graph for FigmaNode {
    // Nodes
    type NodeMarker = FigmaNode;
    type Node = crate::node::Node;
    type NodeData = crate::query_data::AnyNodeQueryData;
    type NodeDiscriminator = NodeKind;
    // Edges — no typed edges yet; `()` is the empty-edge-variant shape.
    type EdgeMarker = FigmaEdge;
    type Edge = ();
    type EdgeData = ();
    type EdgeDiscriminator = ();
}
