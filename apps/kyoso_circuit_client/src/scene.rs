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
    Capacitor, CircuitEdge, CircuitEdgeKind, CircuitNode, DifferentialPairMarker, Ground,
    Inductor, OnLayer, Resistor, SameNetMarker, VoltageSource, WireMarker, kind_of_entity,
};
use kyoso_drag::three_d::Draggable3d;
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_polyline::prelude::{Polyline, PolylineHandle, PolylineMaterial, PolylineMaterialHandle};

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
// Edge visuals (3D, retained-mode polylines)
//
// Each circuit edge entity owns a `Polyline` asset whose two-vertex
// vertex list is rewritten whenever the endpoint nodes move. One
// `PolylineMaterial` per [`CircuitEdgeKind`] — width and colour come
// from the material so signal-type reads at a glance and stays correct
// when zoomed out (gizmos can't do thickness).
// ---------------------------------------------------------------------------

/// Per-kind material handles for circuit edges. Created at startup
/// from [`CircuitEdgeKind::color_srgb`] so all wire edges share one
/// material, all same-net edges share another, etc. — keeping GPU
/// material binds down.
#[derive(Resource)]
pub struct CircuitEdgeMaterials(pub HashMap<CircuitEdgeKind, Handle<PolylineMaterial>>);

impl FromWorld for CircuitEdgeMaterials {
    fn from_world(world: &mut World) -> Self {
        let mut materials = world.resource_mut::<Assets<PolylineMaterial>>();
        let mut by_kind = HashMap::new();
        for kind in [
            CircuitEdgeKind::Wire,
            CircuitEdgeKind::SameNet,
            CircuitEdgeKind::DifferentialPair,
        ] {
            let c = kind.color_srgb();
            let mat = materials.add(PolylineMaterial {
                width: 6.0,
                color: Color::srgb(c[0], c[1], c[2]).to_linear(),
                perspective: false,
                ..default()
            });
            by_kind.insert(kind, mat);
        }
        Self(by_kind)
    }
}

impl CircuitEdgeMaterials {
    fn handle_for(&self, kind: CircuitEdgeKind) -> Handle<PolylineMaterial> {
        // SAFETY: from_world inserted every variant.
        self.0.get(&kind).cloned().expect("material per CircuitEdgeKind")
    }
}

/// On-add observer for circuit edges: attach a Transform (so layer-y
/// snapping has somewhere to live even though edges don't carry
/// `OnLayer` themselves) plus an empty [`Polyline`] asset and the
/// matching [`PolylineMaterial`] handle for the edge's current kind.
///
/// The polyline starts empty — vertices get filled by
/// [`update_polyline_endpoints`] on the next frame once both endpoint
/// transforms are queryable. If the kind marker arrives later than the
/// edge entity itself (the typical inbound-CRDT ordering),
/// [`sync_edge_material`] swaps in the correct material when the
/// marker shows up.
pub fn on_circuit_edge_added(
    trigger: On<Add, CircuitEdge>,
    mut commands: Commands,
    polylines: Option<ResMut<Assets<Polyline>>>,
    materials: Option<Res<CircuitEdgeMaterials>>,
    wires: Query<(), With<WireMarker>>,
    same_net: Query<(), With<SameNetMarker>>,
    diff_pair: Query<(), With<DifferentialPairMarker>>,
) {
    let entity = trigger.entity;
    commands.entity(entity).insert(Transform::default());
    let (Some(mut polylines), Some(materials)) = (polylines, materials) else {
        // Headless / non-visual mode — skip rendering setup.
        return;
    };
    let kind = kind_of_entity(entity, &wires, &same_net, &diff_pair)
        .unwrap_or(CircuitEdgeKind::Wire);
    let polyline = polylines.add(Polyline {
        vertices: Vec::new(),
        colors: None,
    });
    commands.entity(entity).insert((
        PolylineHandle(polyline),
        PolylineMaterialHandle(materials.handle_for(kind)),
    ));
}

/// Reassign the [`PolylineMaterialHandle`] when a kind marker is added
/// to an existing edge. The inbound CRDT path can deliver the structural
/// `AddRefEdge` op before the marker-bearing category projection (which
/// queues an `ApplyEdgeCategory` deferred command), so the edge's
/// initial material may be the default `Wire`. Once the real marker
/// arrives this system swaps in the right one.
pub fn sync_edge_material(
    changed: Query<
        Entity,
        (
            With<CircuitEdge>,
            Or<(
                Added<WireMarker>,
                Added<SameNetMarker>,
                Added<DifferentialPairMarker>,
            )>,
        ),
    >,
    materials: Option<Res<CircuitEdgeMaterials>>,
    wires: Query<(), With<WireMarker>>,
    same_net: Query<(), With<SameNetMarker>>,
    diff_pair: Query<(), With<DifferentialPairMarker>>,
    mut commands: Commands,
) {
    let Some(materials) = materials else { return };
    for entity in changed.iter() {
        let kind = kind_of_entity(entity, &wires, &same_net, &diff_pair)
            .unwrap_or(CircuitEdgeKind::Wire);
        commands
            .entity(entity)
            .insert(PolylineMaterialHandle(materials.handle_for(kind)));
    }
}

/// Per-frame: rewrite each edge's polyline vertices to match its
/// endpoint node transforms. Skips edges whose endpoints haven't been
/// projected yet (`transforms.get(...)` errors) and skips writes that
/// wouldn't change the vertex list — the latter avoids marking the
/// `Polyline` asset dirty (and re-extracting on the render world)
/// every frame for static edges.
pub fn update_polyline_endpoints(
    edges: Query<(&EdgeFrom, &EdgeTo, &PolylineHandle), With<CircuitEdge>>,
    transforms: Query<&Transform, With<CircuitNode>>,
    mut polylines: ResMut<Assets<Polyline>>,
) {
    for (from, to, poly_h) in edges.iter() {
        let (Ok(from_t), Ok(to_t)) = (transforms.get(from.0), transforms.get(to.0)) else {
            continue;
        };
        let Some(mut polyline) = polylines.get_mut(&poly_h.0) else {
            continue;
        };
        let new = [from_t.translation, to_t.translation];
        if polyline.vertices.len() == 2
            && polyline.vertices[0] == new[0]
            && polyline.vertices[1] == new[1]
        {
            continue;
        }
        polyline.vertices.clear();
        polyline.vertices.extend_from_slice(&new);
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
