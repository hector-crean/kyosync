//! Figma REST API → kyoso_core ECS import adapter.
//!
//! This crate is intentionally narrow: it walks a `figma_api::CanvasNode`
//! tree and spawns matching `kyoso_core` entities (Frame / Rectangle /
//! Text) into a Bevy `World`. Component definitions, sync wiring, and
//! generic scene traversal live in `kyoso_core`.
//!
//! ## Layout
//!
//! - [`walker`] — vendored, dep-free Figma node walker (`Walker` +
//!   `NodeVisitor`). Translates `figma_api` enum-shaped trees into a
//!   visitor pattern without dragging the rest of the etch_figma
//!   pipeline in.
//! - [`import`] — `KyosoVisitor` impl over `walker::NodeVisitor`. Maps
//!   Figma variants to `kyoso_core` components on a one-way,
//!   best-effort, log-and-skip basis (unsupported variants — Vector,
//!   Group, Component, etc. — are dropped with a `tracing::warn!`).

pub mod import;
pub mod walker;

pub use import::import_canvas;
pub use walker::{NodeContext, NodeVisitor, SubcanvasNodeExt, Walker};
