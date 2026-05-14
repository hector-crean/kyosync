//! Runtime layer manager for the 3D circuit client.
//!
//! [`CircuitLayer`](kyoso_circuit::CircuitLayer) is a fixed compile-time
//! enum (Signal / Power / Ground / Mechanical). This module adds the
//! runtime state on top:
//!
//! - **Active layer** — which layer place-tool spawns drop onto and
//!   which one the grid manager treats as the focus plane. Driven by
//!   keyboard (`1`..`4`) and by toolbar clicks.
//! - **Per-layer visibility** — `Shift+1`..`Shift+4` hide / show a
//!   whole layer's worth of nodes + their incident edges. Implemented
//!   by flipping `Visibility::Visible` / `Hidden` on every entity
//!   indexed under that layer.
//! - **Entity index** — bidirectional `Entity ↔ CircuitLayer` map
//!   kept in sync as [`OnLayer`] components arrive (via local spawn
//!   or remote CRDT projection). Lets visibility toggles + future
//!   layer-filtered queries run in O(layer-size) instead of scanning
//!   every node.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use kyoso_circuit::{CircuitEdge, CircuitLayer, CircuitNode, OnLayer};
use kyoso_graph::components::{EdgeFrom, EdgeTo};

/// The active layer plus per-layer visibility + a reverse index.
///
/// All circuit-client systems that need "which layer is in focus?" or
/// "which entities live on layer L?" should consult this resource
/// instead of scanning `OnLayer` queries directly.
#[derive(Resource, Debug)]
pub struct LayerManager {
    active: CircuitLayer,
    visible: HashMap<CircuitLayer, bool>,
    entities_by_layer: HashMap<CircuitLayer, HashSet<Entity>>,
    layer_by_entity: HashMap<Entity, CircuitLayer>,
}

impl Default for LayerManager {
    fn default() -> Self {
        let mut visible = HashMap::new();
        let mut entities_by_layer = HashMap::new();
        for layer in CircuitLayer::all() {
            visible.insert(layer, true);
            entities_by_layer.insert(layer, HashSet::new());
        }
        Self {
            active: CircuitLayer::default(),
            visible,
            entities_by_layer,
            layer_by_entity: HashMap::new(),
        }
    }
}

impl LayerManager {
    /// Currently active layer (place-tool target + grid focus plane).
    #[must_use]
    pub fn active(&self) -> CircuitLayer {
        self.active
    }

    /// Switch the active layer. Idempotent — repeated `set_active(x)`
    /// is a no-op so visibility-driven systems don't re-run.
    pub fn set_active(&mut self, layer: CircuitLayer) -> bool {
        if self.active == layer {
            return false;
        }
        self.active = layer;
        true
    }

    /// Whether `layer` is currently shown. Defaults to `true` for any
    /// layer not explicitly hidden.
    #[must_use]
    pub fn is_visible(&self, layer: CircuitLayer) -> bool {
        self.visible.get(&layer).copied().unwrap_or(true)
    }

    /// Flip a layer's visibility. Returns the new value.
    pub fn toggle_visibility(&mut self, layer: CircuitLayer) -> bool {
        let entry = self.visible.entry(layer).or_insert(true);
        *entry = !*entry;
        *entry
    }

    pub fn set_visible(&mut self, layer: CircuitLayer, visible: bool) {
        self.visible.insert(layer, visible);
    }

    /// Entities currently indexed on `layer`. Empty `HashSet` if none.
    #[must_use]
    pub fn entities_on(&self, layer: CircuitLayer) -> &HashSet<Entity> {
        self.entities_by_layer
            .get(&layer)
            .expect("LayerManager initializes a slot for every CircuitLayer variant")
    }

    /// Which layer an entity is on, if any.
    #[must_use]
    pub fn layer_of(&self, entity: Entity) -> Option<CircuitLayer> {
        self.layer_by_entity.get(&entity).copied()
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct LayerManagerPlugin;

impl Plugin for LayerManagerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LayerManager>();
        app.add_systems(
            Update,
            (
                index_node_layers,
                forget_removed_nodes,
                layer_hotkeys,
                apply_node_visibility,
                apply_edge_visibility,
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// Index maintenance — keep the LayerManager's reverse map current
// ---------------------------------------------------------------------------

/// Insert / move entities in the LayerManager's reverse index whenever
/// their [`OnLayer`] component is added or mutated (locally or via an
/// inbound CRDT op).
fn index_node_layers(
    mut manager: ResMut<LayerManager>,
    changed: Query<(Entity, &OnLayer), (With<CircuitNode>, Changed<OnLayer>)>,
) {
    for (entity, on_layer) in changed.iter() {
        let Some(layer) = on_layer.layer() else {
            continue;
        };
        // If the entity was previously on a different layer, drop it
        // from that slot first.
        if let Some(prev) = manager.layer_by_entity.get(&entity).copied() {
            if prev != layer {
                if let Some(set) = manager.entities_by_layer.get_mut(&prev) {
                    set.remove(&entity);
                }
            }
        }
        manager.layer_by_entity.insert(entity, layer);
        manager
            .entities_by_layer
            .entry(layer)
            .or_default()
            .insert(entity);
    }
}

/// When a `CircuitNode` is despawned (local or remote), drop it from
/// the reverse index so visibility / layer iteration don't see stale
/// entity ids.
fn forget_removed_nodes(
    mut manager: ResMut<LayerManager>,
    mut removed: RemovedComponents<CircuitNode>,
) {
    for entity in removed.read() {
        if let Some(layer) = manager.layer_by_entity.remove(&entity) {
            if let Some(set) = manager.entities_by_layer.get_mut(&layer) {
                set.remove(&entity);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Hotkeys
// ---------------------------------------------------------------------------

/// `1`..`4` set the active layer. `Shift+1`..`Shift+4` toggle that
/// layer's visibility. Numeric ids match
/// [`CircuitLayer::id`](kyoso_circuit::CircuitLayer::id).
fn layer_hotkeys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut manager: ResMut<LayerManager>,
) {
    let Some(keys) = keys else { return };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let pressed_id = if keys.just_pressed(KeyCode::Digit1) {
        Some(1)
    } else if keys.just_pressed(KeyCode::Digit2) {
        Some(2)
    } else if keys.just_pressed(KeyCode::Digit3) {
        Some(3)
    } else if keys.just_pressed(KeyCode::Digit4) {
        Some(4)
    } else {
        None
    };
    let Some(id) = pressed_id else { return };
    let Some(layer) = CircuitLayer::from_id(id) else {
        return;
    };
    if shift {
        manager.toggle_visibility(layer);
    } else {
        manager.set_active(layer);
    }
}

// ---------------------------------------------------------------------------
// Visibility application
// ---------------------------------------------------------------------------

/// Flip `Visibility::Visible` / `Hidden` on every node entity based on
/// its layer's current visibility state. Runs only when `LayerManager`
/// changes — for static frames this is a no-op.
fn apply_node_visibility(
    manager: Res<LayerManager>,
    mut nodes: Query<(&OnLayer, &mut Visibility), With<CircuitNode>>,
) {
    if !manager.is_changed() {
        return;
    }
    for (on_layer, mut vis) in nodes.iter_mut() {
        let Some(layer) = on_layer.layer() else {
            continue;
        };
        let target = if manager.is_visible(layer) {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != target {
            *vis = target;
        }
    }
}

/// Edges don't carry [`OnLayer`] directly — they inherit visibility
/// from their endpoints. An edge is shown iff both endpoint nodes'
/// layers are visible. Avoids ghost edges floating in space when one
/// end's layer is hidden.
fn apply_edge_visibility(
    manager: Res<LayerManager>,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), With<CircuitEdge>>,
    mut vis_q: Query<&mut Visibility, With<CircuitEdge>>,
) {
    if !manager.is_changed() {
        return;
    }
    for (entity, from, to) in edges.iter() {
        let from_visible = manager
            .layer_of(from.0)
            .map(|l| manager.is_visible(l))
            .unwrap_or(true);
        let to_visible = manager
            .layer_of(to.0)
            .map(|l| manager.is_visible(l))
            .unwrap_or(true);
        let target = if from_visible && to_visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        let Ok(mut vis) = vis_q.get_mut(entity) else {
            continue;
        };
        if *vis != target {
            *vis = target;
        }
    }
}
