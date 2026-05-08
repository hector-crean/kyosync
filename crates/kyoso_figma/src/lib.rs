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
//!   `kyoso_sync::CrdtSyncPlugin`. Every spawned figma entity carries
//!   `FigmaNode` plus exactly one of the field-bearing components.
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

pub mod frame;
pub mod import;
pub mod paint;
pub mod plugin;
pub mod rectangle;
pub mod size;
pub mod text;
pub mod typestyle;
pub mod walker;

pub use frame::{Frame, LayoutMode};
pub use paint::{GradientStop, GradientType, ImageScaleMode, Paint};
pub use plugin::KyosoFigmaPlugin;
pub use rectangle::Rectangle;
pub use size::Size;
pub use text::Text;
pub use typestyle::TypeStyle;
pub use walker::{NodeContext, NodeVisitor, SubcanvasNodeExt, Walker};

// ---------------------------------------------------------------------------
// Structural markers
// ---------------------------------------------------------------------------

/// Zero-sized marker: every kyoso_figma node entity carries this. Used
/// as the `N` parameter to [`kyoso_sync::CrdtSyncPlugin`] so structural
/// ops (`AddNode`, `RemoveNode`, `Move`) operate on a uniform "this is
/// a figma node" identity, decoupled from which specific node kind
/// (`Frame` / `Rectangle` / `Text` / ...) the entity carries.
#[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
pub struct FigmaNode;

/// Zero-sized marker: every kyoso_figma edge entity carries this. Used
/// as the `E` parameter to [`kyoso_sync::CrdtSyncPlugin`]. The initial
/// cut only uses tree edges (via `kyoso_graph::tree`); reference edges
/// (component→main, prototype links) are deferred.
#[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
pub struct FigmaEdge;
