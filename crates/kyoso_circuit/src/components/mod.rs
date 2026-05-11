//! Per-component schemas for circuit nodes.
//!
//! Each module defines one Bevy `Component` with `derive(SchemaSync)`,
//! mirroring the kyoso_figma per-shape pattern (`Frame`, `Rectangle`,
//! `Text`). Scalar parameters use LWW. The component kinds are mutually
//! exclusive on a given entity — exactly one of them is paired with the
//! [`crate::CircuitNode`] structural marker.

pub mod capacitor;
pub mod ground;
pub mod inductor;
pub mod resistor;
pub mod voltage_source;

pub use capacitor::Capacitor;
pub use ground::Ground;
pub use inductor::Inductor;
pub use resistor::Resistor;
pub use voltage_source::VoltageSource;

use serde::{Deserialize, Serialize};

/// User-facing palette of component kinds. Used by the place-tool +
/// toolbar to decide which component to spawn next, and by the scene
/// renderer to pick a symbol.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ComponentKind {
    #[default]
    Resistor,
    Capacitor,
    Inductor,
    VoltageSource,
    Ground,
}

impl ComponentKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Resistor => "Resistor",
            Self::Capacitor => "Capacitor",
            Self::Inductor => "Inductor",
            Self::VoltageSource => "Voltage",
            Self::Ground => "Ground",
        }
    }

    /// Number of pins the component exposes. Pin offsets in screen space
    /// are computed by the scene renderer; the count is what the
    /// connect-tool uses to know how many anchor points to draw.
    #[must_use]
    pub fn pin_count(self) -> u8 {
        match self {
            Self::Ground => 1,
            _ => 2,
        }
    }

    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::Resistor,
            Self::Capacitor,
            Self::Inductor,
            Self::VoltageSource,
            Self::Ground,
        ]
        .into_iter()
    }
}
