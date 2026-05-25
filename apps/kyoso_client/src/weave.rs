//! Weave-style typed cross-frame edges.
//!
//! Where `kyoso_graph::tree::TreeEdge` represents the design hierarchy
//! (a frame's children), `WeaveEdgeKind` represents *relationships*
//! between frames — the FigJam/Weave side of the hybrid app. Each kind
//! is a separate per-variant ZST marker that ships through the
//! existing [`kyoso_sync::SyncedEdgeCategoryPlugin`] mechanism.
//!
//! ## Design
//!
//! - `WeaveEdgeKind` is an app-level enum (Reference / Dependency /
//!   Comment / Annotation). Adding kinds is a kyoso_client change, no
//!   touch to the CRDT layer.
//! - Each kind has a per-variant ZST marker (`ReferenceMarker`, ...)
//!   implementing [`kyoso_sync::EdgeCategoryMarker`]. The marker maps
//!   to a `kyoso_crdt::EdgeCategory::Custom("kebab-name")` on the wire.
//! - On spawn the user's Connect tool drops the matching marker on the
//!   edge entity along with the structural `(EdgeFrom, EdgeTo,
//!   SceneEdge)`. On inbound projection, [`SyncedEdgeCategoryPlugin`]
//!   re-attaches the marker on the receiving peer (Part V §V.1).
//! - `WeaveEdgeKind::color()` gives each kind a distinct render colour;
//!   used by the polyline material in `scene.rs`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// User-facing kinds of cross-frame relationship.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    Hash,
    Default,
    Reflect,
    Serialize,
    Deserialize,
)]
pub enum WeaveEdgeKind {
    /// Generic "this references that" — the catch-all kind. Used as
    /// the default for the Connect tool.
    #[default]
    Reference,
    /// "This depends on that" — directional, used in flowcharts /
    /// dependency graphs.
    Dependency,
    /// "This comments on that" — anchors a Comment frame to a target.
    Comment,
    /// "This annotates that" — for callouts / labels / highlights.
    Annotation,
}

impl WeaveEdgeKind {
    /// User-readable name (used in the toolbar UI).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            WeaveEdgeKind::Reference => "Reference",
            WeaveEdgeKind::Dependency => "Dependency",
            WeaveEdgeKind::Comment => "Comment",
            WeaveEdgeKind::Annotation => "Annotation",
        }
    }

    /// Distinct sRGB colour per kind so peers can tell edges apart at
    /// a glance.
    #[must_use]
    pub fn color(self) -> Color {
        match self {
            WeaveEdgeKind::Reference => Color::srgb(0.20, 0.55, 0.95),
            WeaveEdgeKind::Dependency => Color::srgb(0.95, 0.30, 0.20),
            WeaveEdgeKind::Comment => Color::srgb(0.30, 0.80, 0.40),
            WeaveEdgeKind::Annotation => Color::srgb(0.85, 0.60, 0.20),
        }
    }
}

// ---------------------------------------------------------------------------
// Per-variant marker components
// ---------------------------------------------------------------------------

macro_rules! weave_marker {
    ($name:ident) => {
        #[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
        #[reflect(Component, Default)]
        pub struct $name;
    };
}

weave_marker!(ReferenceMarker);
weave_marker!(DependencyMarker);
weave_marker!(CommentMarker);
weave_marker!(AnnotationMarker);

/// Insert the right marker component for `kind` onto an entity. Used
/// by the Connect tool when spawning a typed edge.
pub fn insert_marker_for(commands: &mut EntityCommands<'_>, kind: WeaveEdgeKind) {
    match kind {
        WeaveEdgeKind::Reference => {
            commands.insert(ReferenceMarker);
        }
        WeaveEdgeKind::Dependency => {
            commands.insert(DependencyMarker);
        }
        WeaveEdgeKind::Comment => {
            commands.insert(CommentMarker);
        }
        WeaveEdgeKind::Annotation => {
            commands.insert(AnnotationMarker);
        }
    }
}

/// Inverse: given an entity's marker components, identify its kind.
/// Returns `None` if the entity has no recognised weave-edge marker
/// (in which case it's a raw `SceneEdge` with no category).
pub fn kind_of_entity(
    entity: Entity,
    refs: &Query<(), With<ReferenceMarker>>,
    deps: &Query<(), With<DependencyMarker>>,
    comments: &Query<(), With<CommentMarker>>,
    annots: &Query<(), With<AnnotationMarker>>,
) -> Option<WeaveEdgeKind> {
    if refs.get(entity).is_ok() {
        return Some(WeaveEdgeKind::Reference);
    }
    if deps.get(entity).is_ok() {
        return Some(WeaveEdgeKind::Dependency);
    }
    if comments.get(entity).is_ok() {
        return Some(WeaveEdgeKind::Comment);
    }
    if annots.get(entity).is_ok() {
        return Some(WeaveEdgeKind::Annotation);
    }
    None
}
