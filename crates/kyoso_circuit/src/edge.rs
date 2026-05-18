//! Typed edge categories for circuits.
//!
//! Direct analogue of `kyoso_client::weave` for the circuit domain.
//! Three kinds:
//!
//! - **Wire** — the conducting electrical connection between two
//!   component pins. The structural edge that defines the netlist.
//! - **SameNet** — non-electrical hint that two pins must be at the
//!   same potential. Used by validators / netlist exporters to verify
//!   intent that's awkward to express through extra wires.
//! - **DifferentialPair** — marks two wires that must be matched
//!   (length, impedance) during routing. Pure metadata for the
//!   schematic; consumed by downstream layout tooling.
//!
//! Each kind has a per-variant ZST marker that is currently
//! **local-only** — the slim `kyoso_graph_sync::GraphSyncPlugin`
//! replicates edge presence + endpoints but not category. Apps that
//! need cross-peer category sync can derive `SchemaSync` on a custom
//! per-edge component and register it via
//! `SchemaSyncedComponentPlugin::<EdgeTarget, _>`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// sRGB triple (0..=1) — kept feature-free so the domain crate doesn't
/// have to opt into `bevy_color`. App code converts to `bevy::Color`.
pub type ColorRgb = [f32; 3];

/// User-facing kinds of circuit edge.
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
pub enum CircuitEdgeKind {
    /// Conducting electrical connection. Used as the default for the
    /// connect tool.
    #[default]
    Wire,
    /// Non-electrical hint: these pins must end up at the same potential.
    SameNet,
    /// Routing constraint: pair of wires that should be matched.
    DifferentialPair,
}

impl CircuitEdgeKind {
    /// User-readable name (used in the toolbar UI).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Wire => "Wire",
            Self::SameNet => "Same-Net",
            Self::DifferentialPair => "Diff Pair",
        }
    }

    /// Distinct sRGB colour per kind so peers can tell edges apart at
    /// a glance. Returned as a feature-free `[f32; 3]` so the domain
    /// crate doesn't need bevy's render features; app code constructs
    /// `Color::srgb(c[0], c[1], c[2])`.
    #[must_use]
    pub fn color_srgb(self) -> ColorRgb {
        match self {
            Self::Wire => [0.95, 0.85, 0.20],
            Self::SameNet => [0.30, 0.80, 0.40],
            Self::DifferentialPair => [0.85, 0.30, 0.85],
        }
    }
}

// ---------------------------------------------------------------------------
// Per-variant marker components
// ---------------------------------------------------------------------------

macro_rules! circuit_edge_marker {
    ($name:ident) => {
        #[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
        #[reflect(Component, Default)]
        pub struct $name;
    };
}

circuit_edge_marker!(WireMarker);
circuit_edge_marker!(SameNetMarker);
circuit_edge_marker!(DifferentialPairMarker);

/// Insert the right marker component for `kind` onto an edge entity.
/// Called by the connect tool when spawning a typed edge.
pub fn insert_marker_for(commands: &mut EntityCommands<'_>, kind: CircuitEdgeKind) {
    match kind {
        CircuitEdgeKind::Wire => {
            commands.insert(WireMarker);
        }
        CircuitEdgeKind::SameNet => {
            commands.insert(SameNetMarker);
        }
        CircuitEdgeKind::DifferentialPair => {
            commands.insert(DifferentialPairMarker);
        }
    }
}

/// Inverse: given an entity's marker components, identify its kind.
/// Returns `None` if the entity has no recognised circuit-edge marker.
pub fn kind_of_entity(
    entity: Entity,
    wires: &Query<(), With<WireMarker>>,
    same_net: &Query<(), With<SameNetMarker>>,
    diff_pair: &Query<(), With<DifferentialPairMarker>>,
) -> Option<CircuitEdgeKind> {
    if wires.get(entity).is_ok() {
        return Some(CircuitEdgeKind::Wire);
    }
    if same_net.get(entity).is_ok() {
        return Some(CircuitEdgeKind::SameNet);
    }
    if diff_pair.get(entity).is_ok() {
        return Some(CircuitEdgeKind::DifferentialPair);
    }
    None
}
