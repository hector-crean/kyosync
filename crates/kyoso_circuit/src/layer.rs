//! Layer model for circuit boards.
//!
//! Inspired by `bild_canvas::stack::layer` but stripped down for the
//! circuit domain: no `Grid2D` payload, just a small numeric id and a
//! per-id metadata table maintained by the app. A circuit "layer" is a
//! parallel z-plane (Signal / Power / Ground / Mechanical) on which
//! components live; layer assignment replicates across peers via the
//! schema-synced [`OnLayer`] component.
//!
//! ## Wire representation
//!
//! [`OnLayer`] is the only thing that crosses the wire — a single `u8`
//! per node. The app-side [`CircuitLayer`] enum interprets the id and
//! provides the rendering metadata (z-offset, label, colour). Both
//! sides of a connected room must agree on the enum mapping (id → label
//! → z-offset). For the initial cut, the four built-in layers below
//! cover schematic-style work; extending the palette later is purely
//! additive on the enum.

use bevy::prelude::*;
use kyoso_sync::SchemaSync;
use serde::{Deserialize, Serialize};

/// Schema-synced component: which board layer this circuit node lives
/// on. Default `layer_id: 0` means "unassigned" — the place tool always
/// stamps a non-zero id (the active [`CircuitLayer`]) so the
/// `SchemaSync` change-detection sees a non-default value and emits a
/// replication op.
#[derive(Component, Default, Clone, Debug, PartialEq, Reflect, SchemaSync)]
#[reflect(Component, Default)]
#[schema(name = "OnLayer")]
pub struct OnLayer {
    pub layer_id: u8,
}

impl OnLayer {
    #[must_use]
    pub fn new(layer: CircuitLayer) -> Self {
        Self {
            layer_id: layer.id(),
        }
    }

    #[must_use]
    pub fn layer(&self) -> Option<CircuitLayer> {
        CircuitLayer::from_id(self.layer_id)
    }
}

/// Built-in circuit board layers. Matches a typical 4-layer PCB stackup.
///
/// Numeric ids start at 1 so that `0` is reserved as "unassigned" for
/// `OnLayer::default()` — see the module docstring for why.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum CircuitLayer {
    /// Signal traces — the default working layer for placing components.
    #[default]
    Signal = 1,
    /// Power distribution plane.
    Power = 2,
    /// Ground reference plane.
    Ground = 3,
    /// Mechanical / silkscreen — non-electrical annotations and
    /// mounting features.
    Mechanical = 4,
}

impl CircuitLayer {
    /// Numeric id stamped into [`OnLayer::layer_id`].
    #[must_use]
    pub fn id(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Signal),
            2 => Some(Self::Power),
            3 => Some(Self::Ground),
            4 => Some(Self::Mechanical),
            _ => None,
        }
    }

    /// Human-readable name for toolbar UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Signal => "Signal",
            Self::Power => "Power",
            Self::Ground => "GND",
            Self::Mechanical => "Mech",
        }
    }

    /// World-space y offset for this layer. Layers are stacked along
    /// the Y axis with constant spacing so the orbit camera sees a
    /// PCB-like layered board from any angle.
    #[must_use]
    pub fn y_offset(self) -> f32 {
        const LAYER_SPACING: f32 = 1.5;
        f32::from(self.id() - 1) * LAYER_SPACING
    }

    /// Distinct sRGB tint per layer for board / outline / panel UI.
    /// Returned as a feature-free `[f32; 3]` so the domain crate stays
    /// independent of bevy's render features.
    #[must_use]
    pub fn color_srgb(self) -> [f32; 3] {
        match self {
            Self::Signal => [0.95, 0.85, 0.20],
            Self::Power => [0.95, 0.30, 0.30],
            Self::Ground => [0.40, 0.40, 0.45],
            Self::Mechanical => [0.30, 0.80, 0.40],
        }
    }

    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Signal, Self::Power, Self::Ground, Self::Mechanical].into_iter()
    }
}
