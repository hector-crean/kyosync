//! Scene primitives for the 3D circuit client.
//!
//! - **Nodes** are `kyoso_circuit::CircuitNode` entities carrying one of
//!   the per-kind component schemas (`Resistor`, `Capacitor`, …) plus
//!   a `Transform` (with the layer's y-offset baked in) and an
//!   [`OnLayer`] to identify which board layer they live on. The
//!   on-add observer attaches a 3D mesh sized for the kind:
//!   resistor / inductor → cuboid, capacitor → cylinder,
//!   voltage source → sphere, ground → cone.
//! - **Edges** are `CircuitEdge` entities with `EdgeFrom`/`EdgeTo` plus
//!   one of the per-kind markers from `kyoso_circuit::edge`. They're
//!   rendered immediate-mode via 3D gizmos in
//!   [`draw_edges_with_gizmos`], coloured by [`CircuitEdgeKind`].
//! - **Layers** are stacked along the world Y axis at fixed offsets
//!   ([`CircuitLayer::y_offset`]). When an entity's [`OnLayer`] arrives
//!   from the network or is mutated locally, [`sync_layer_y`] snaps its
//!   transform onto the matching plane so peers see the same layered
//!   board.

use std::collections::HashMap;

use bevy::math::primitives::{
    Cone as ConeShape, Cuboid as CuboidShape, Cylinder as CylinderShape, Sphere as SphereShape,
};
use bevy::prelude::*;
use kyoso_circuit::{
    Capacitor, CircuitEdge, CircuitEdgeKind, CircuitLayer, CircuitNode, DifferentialPairMarker,
    Ground, Inductor, OnLayer, Resistor, SameNetMarker, VoltageSource, WireMarker,
    kind_of_entity,
};
use kyoso_drag::three_d::Draggable3d;
use kyoso_graph::components::{EdgeFrom, EdgeTo};

use crate::msg::{AppEvent, ExternalId, GraphMessageExt};

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

/// Reverse cache `Entity → ExternalId` for nodes and edges. The sync
/// layer's `EntityCrdtIndex` loses the `Entity → ExternalId` row at the
/// same time the entity is despawned; this cache holds onto it long
/// enough to emit one `NodeRemoved`/`EdgeRemoved`.
#[derive(Resource, Default)]
pub struct ExternalIdCache {
    pub nodes: HashMap<Entity, ExternalId>,
    pub edges: HashMap<Entity, ExternalId>,
}

// ---------------------------------------------------------------------------
// Component visuals (3D)
// ---------------------------------------------------------------------------

/// Pick a 3D mesh primitive per component kind. Approximate schematic
/// caricatures: a long thin cuboid for the resistor, a short cylinder
/// for the capacitor, a longer cylinder for the inductor (placeholder
/// for a future toroid/coil), a sphere for the voltage source, and a
/// downward cone for the ground reference.
fn component_mesh(
    entity: Entity,
    resistors: &Query<(), With<Resistor>>,
    capacitors: &Query<(), With<Capacitor>>,
    inductors: &Query<(), With<Inductor>>,
    voltage_sources: &Query<(), With<VoltageSource>>,
    grounds: &Query<(), With<Ground>>,
) -> Mesh {
    if resistors.get(entity).is_ok() {
        Mesh::from(CuboidShape::new(0.9, 0.3, 0.3))
    } else if capacitors.get(entity).is_ok() {
        Mesh::from(CylinderShape::new(0.25, 0.5))
    } else if inductors.get(entity).is_ok() {
        Mesh::from(CylinderShape::new(0.22, 0.9))
    } else if voltage_sources.get(entity).is_ok() {
        Mesh::from(SphereShape::new(0.32))
    } else if grounds.get(entity).is_ok() {
        Mesh::from(ConeShape::new(0.32, 0.45))
    } else {
        // Fallback for entities whose schema hasn't arrived yet on a
        // remote peer — small cube so the user sees *something* before
        // the kind-specific schema replicates a frame later.
        Mesh::from(CuboidShape::new(0.3, 0.3, 0.3))
    }
}

fn component_color(
    entity: Entity,
    resistors: &Query<(), With<Resistor>>,
    capacitors: &Query<(), With<Capacitor>>,
    inductors: &Query<(), With<Inductor>>,
    voltage_sources: &Query<(), With<VoltageSource>>,
    grounds: &Query<(), With<Ground>>,
) -> Color {
    if resistors.get(entity).is_ok() {
        Color::srgb(0.85, 0.55, 0.20)
    } else if capacitors.get(entity).is_ok() {
        Color::srgb(0.30, 0.55, 0.95)
    } else if inductors.get(entity).is_ok() {
        Color::srgb(0.60, 0.30, 0.85)
    } else if voltage_sources.get(entity).is_ok() {
        Color::srgb(0.95, 0.30, 0.30)
    } else if grounds.get(entity).is_ok() {
        Color::srgb(0.50, 0.50, 0.55)
    } else {
        Color::srgb(0.85, 0.86, 0.90)
    }
}

/// On-add observer for `CircuitNode` entities: attach a 3D mesh +
/// material + 3D drag handle. Mesh shape and colour are keyed off the
/// per-kind schema component.
pub fn on_circuit_node_added(
    trigger: On<Add, CircuitNode>,
    mut commands: Commands,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    resistors: Query<(), With<Resistor>>,
    capacitors: Query<(), With<Capacitor>>,
    inductors: Query<(), With<Inductor>>,
    voltage_sources: Query<(), With<VoltageSource>>,
    grounds: Query<(), With<Ground>>,
) {
    let entity = trigger.entity;
    commands
        .entity(entity)
        .insert((Visibility::default(), Draggable3d::default()));
    if let (Some(mut meshes), Some(mut materials)) = (meshes, materials) {
        let mesh = meshes.add(component_mesh(
            entity,
            &resistors,
            &capacitors,
            &inductors,
            &voltage_sources,
            &grounds,
        ));
        let color = component_color(
            entity,
            &resistors,
            &capacitors,
            &inductors,
            &voltage_sources,
            &grounds,
        );
        let material = materials.add(StandardMaterial {
            base_color: color,
            perceptual_roughness: 0.5,
            metallic: 0.1,
            ..default()
        });
        commands
            .entity(entity)
            .insert((Mesh3d(mesh), MeshMaterial3d(material)));
    }
}

/// Re-bake mesh + material whenever the per-kind schema component is
/// added — required because remote spawns receive the structural
/// `AddNode` op before the trailing `SetNodeProperty` ops attach the
/// kind-specific schema (see the place-tool docstring on why
/// `Default::default()` doesn't replicate).
pub fn sync_component_visuals(
    new_schemas: Query<
        (
            Entity,
            &Mesh3d,
            &MeshMaterial3d<StandardMaterial>,
        ),
        Or<(
            Added<Resistor>,
            Added<Capacitor>,
            Added<Inductor>,
            Added<VoltageSource>,
            Added<Ground>,
        )>,
    >,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    resistors: Query<(), With<Resistor>>,
    capacitors: Query<(), With<Capacitor>>,
    inductors: Query<(), With<Inductor>>,
    voltage_sources: Query<(), With<VoltageSource>>,
    grounds: Query<(), With<Ground>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    for (entity, mesh_h, mat_h) in new_schemas.iter() {
        if let Some(mut mesh) = meshes.get_mut(&mesh_h.0) {
            *mesh = component_mesh(
                entity,
                &resistors,
                &capacitors,
                &inductors,
                &voltage_sources,
                &grounds,
            );
        }
        if let Some(mut mat) = materials.get_mut(&mat_h.0) {
            mat.base_color = component_color(
                entity,
                &resistors,
                &capacitors,
                &inductors,
                &voltage_sources,
                &grounds,
            );
        }
    }
}

/// When [`OnLayer`] is added or changes (locally or via inbound CRDT
/// op), snap the entity's transform.y to the matching layer's y-offset
/// so layers visibly stack along the world Y axis. Leaves x/z alone so
/// the user-chosen 2D position on a layer is preserved.
pub fn sync_layer_y(
    mut moved: Query<(&OnLayer, &mut Transform), (With<CircuitNode>, Changed<OnLayer>)>,
) {
    for (on_layer, mut transform) in moved.iter_mut() {
        if let Some(layer) = on_layer.layer() {
            transform.translation.y = layer.y_offset();
        }
    }
}

// ---------------------------------------------------------------------------
// Edge visuals (3D)
// ---------------------------------------------------------------------------

pub fn on_circuit_edge_added(trigger: On<Add, CircuitEdge>, mut commands: Commands) {
    commands.entity(trigger.entity).insert(Transform::default());
}

/// Per-frame: draw every circuit edge as a 3D gizmo line from source
/// component to target component, coloured by [`CircuitEdgeKind`].
/// Bevy's `Gizmos::line` renders as a 3D line segment under any active
/// 3D camera.
pub fn draw_edges_with_gizmos(
    mut gizmos: Gizmos,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), With<CircuitEdge>>,
    wires: Query<(), With<WireMarker>>,
    same_net: Query<(), With<SameNetMarker>>,
    diff_pair: Query<(), With<DifferentialPairMarker>>,
    transforms: Query<&Transform, With<CircuitNode>>,
) {
    for (entity, from, to) in edges.iter() {
        let (Ok(from_t), Ok(to_t)) = (transforms.get(from.0), transforms.get(to.0)) else {
            continue;
        };
        let kind = kind_of_entity(entity, &wires, &same_net, &diff_pair)
            .unwrap_or(CircuitEdgeKind::Wire);
        let c = kind.color_srgb();
        gizmos.line(
            from_t.translation,
            to_t.translation,
            Color::srgb(c[0], c[1], c[2]),
        );
    }
}

/// Draw the layer planes as faint grids so the user can see which layer
/// they're working on. Lives in `VisualPlugin` since it uses gizmos.
pub fn draw_layer_planes(mut gizmos: Gizmos) {
    const HALF_SIZE: f32 = 8.0;
    const GRID_DIVS: u32 = 16;
    for layer in CircuitLayer::all() {
        let y = layer.y_offset();
        let c = layer.color_srgb();
        let color = Color::srgba(c[0], c[1], c[2], 0.25);
        gizmos.grid(
            Isometry3d::new(
                Vec3::new(0.0, y, 0.0),
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            ),
            UVec2::splat(GRID_DIVS),
            Vec2::splat(HALF_SIZE * 2.0 / GRID_DIVS as f32),
            color,
        );
    }
}

// ---------------------------------------------------------------------------
// AppEvent projection
// ---------------------------------------------------------------------------

pub fn emit_node_appeared(
    nodes: Query<(Entity, &Transform), Added<CircuitNode>>,
    index: Res<SyncedIndex>,
    mut cache: ResMut<ExternalIdCache>,
    mut events: MessageWriter<AppEvent>,
) {
    for (entity, transform) in nodes.iter() {
        let Some(&id) = index.node_of_entity.get(&entity) else {
            continue;
        };
        cache.nodes.insert(entity, id);
        events.write(AppEvent::Graph(GraphMessageExt::NodeAppeared {
            id,
            position: transform.translation.into(),
        }));
    }
}

pub fn emit_node_moved(
    moved: Query<(Entity, &Transform), (Changed<Transform>, With<CircuitNode>)>,
    index: Res<SyncedIndex>,
    mut events: MessageWriter<AppEvent>,
) {
    for (entity, transform) in moved.iter() {
        let Some(&id) = index.node_of_entity.get(&entity) else {
            continue;
        };
        events.write(AppEvent::Graph(GraphMessageExt::NodeMoved {
            id,
            position: transform.translation.into(),
        }));
    }
}

pub fn emit_node_removed(
    mut removed: RemovedComponents<CircuitNode>,
    mut cache: ResMut<ExternalIdCache>,
    mut events: MessageWriter<AppEvent>,
) {
    for entity in removed.read() {
        if let Some(id) = cache.nodes.remove(&entity) {
            events.write(AppEvent::Graph(GraphMessageExt::NodeRemoved { id }));
        }
    }
}

pub fn emit_edge_appeared(
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), Added<CircuitEdge>>,
    index: Res<SyncedIndex>,
    mut cache: ResMut<ExternalIdCache>,
    mut events: MessageWriter<AppEvent>,
) {
    for (entity, ef, et) in edges.iter() {
        let Some(&edge_id) = index.edge_of_entity.get(&entity) else {
            continue;
        };
        let Some(&from_id) = index.node_of_entity.get(&ef.0) else {
            continue;
        };
        let Some(&to_id) = index.node_of_entity.get(&et.0) else {
            continue;
        };
        cache.edges.insert(entity, edge_id);
        events.write(AppEvent::Graph(GraphMessageExt::EdgeAppeared {
            id: edge_id,
            from: from_id,
            to: to_id,
        }));
    }
}

pub fn emit_edge_removed(
    mut removed: RemovedComponents<CircuitEdge>,
    mut cache: ResMut<ExternalIdCache>,
    mut events: MessageWriter<AppEvent>,
) {
    for entity in removed.read() {
        if let Some(id) = cache.edges.remove(&entity) {
            events.write(AppEvent::Graph(GraphMessageExt::EdgeRemoved {
                id,
                from: id,
                to: id,
            }));
        }
    }
}
