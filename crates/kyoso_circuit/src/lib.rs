//! Bevy-native analogue-circuit document model for kyoso.
//!
//! ## What this crate provides
//!
//! - **Component definitions** — `Resistor`, `Capacitor`, `Inductor`,
//!   `VoltageSource`, `Ground`. Each is a Bevy `Component` with
//!   `derive(SchemaSync)` so per-field mutations replicate via the
//!   `kyoso_sync` typed-schema path. Scalar parameters use LWW.
//! - **Structural markers** — [`CircuitNode`] and [`CircuitEdge`]
//!   (zero-sized) used as the `<N, E>` parameters to
//!   `kyoso_graph_sync::GraphSyncPlugin`. Every spawned circuit-component
//!   entity carries `CircuitNode` plus exactly one of the field-bearing
//!   component types; every edge carries `CircuitEdge` plus exactly one
//!   of the per-kind markers from [`edge`].
//! - **Edge categories** — [`edge::CircuitEdgeKind`] plus per-variant
//!   marker components (`WireMarker`, `SameNetMarker`,
//!   `DifferentialPairMarker`) that map to
//!   `kyoso_crdt::EdgeCategory::Custom(...)` values on the wire. These
//!   live in the domain crate (not the app) because the category strings
//!   are part of the synced model — peers in different processes need to
//!   agree on them.
//! - **`KyosoCircuitPlugin`** — single-call entry that registers the
//!   `SyncTransportPlugin`, `GraphSyncPlugin<CircuitNode, CircuitEdge>`,
//!   and one `SchemaSyncedComponentPlugin` per component type.
//!
//! The sibling `kyoso_figma` crate is the design-tool counterpart;
//! kyoso_circuit is its analogue for schematic capture.

use bevy::prelude::*;

pub mod components;
pub mod edge;
pub mod layer;
pub mod plugin;

pub use components::{Capacitor, ComponentKind, Ground, Inductor, Resistor, VoltageSource};
pub use edge::{
    CircuitEdgeKind, DifferentialPairMarker, SameNetMarker, WireMarker, insert_marker_for,
    kind_of_entity,
};
pub use layer::{CircuitLayer, OnLayer};
pub use plugin::KyosoCircuitPlugin;

// ---------------------------------------------------------------------------
// Structural markers
// ---------------------------------------------------------------------------

/// Zero-sized marker: every kyoso_circuit node entity carries this. Used
/// as the `N` parameter to `kyoso_graph_sync::GraphSyncPlugin` so
/// structural ops (`AddNode`, `RemoveNode`, …) operate on a uniform
/// "this is a circuit node" identity, decoupled from which specific
/// component kind (`Resistor` / `Capacitor` / `Inductor` / …) the
/// entity carries.
#[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
pub struct CircuitNode;

/// Zero-sized marker: every kyoso_circuit edge entity carries this. Used
/// as the `E` parameter to `kyoso_graph_sync::GraphSyncPlugin`. Every
/// circuit edge also carries exactly one of the per-kind markers from
/// [`edge`] so the per-category dispatch can route it correctly.
#[derive(Component, Default, Clone, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
pub struct CircuitEdge;
