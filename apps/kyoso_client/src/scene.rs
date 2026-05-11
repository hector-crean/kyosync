//! Scene primitives for the figma+weave hybrid app.
//!
//! - **Nodes** are kyoso_figma `Frame` entities. `kyoso_figma::FigmaNode`
//!   is the structural marker; the `Frame` component carries the
//!   per-field synced state (name, fills, layout, etc.).
//! - **Edges** are weave-style typed cross-frame relationships. Each
//!   carries `EdgeFrom`/`EdgeTo`, `kyoso_figma::FigmaEdge` (the
//!   structural marker for `<E>` in `CrdtSyncPlugin`), and exactly one
//!   of the per-kind marker components from [`crate::weave`]. A
//!   reusable polyline rendering plumbs colour from the kind.
//!
//! AppEvent projection (`NodeAppeared` / `NodeMoved` / `EdgeAppeared` /
//! removals) follows the same shape as before — those are abstract
//! events tied to the `EntityCrdtIndex`, not to a specific component
//! type — but the queries now match `Frame` and `FigmaEdge` instead of
//! the old `GraphNode` / `GraphEdge`.

use std::collections::HashMap;

use bevy::math::primitives::Rectangle as RectangleShape;
use bevy::prelude::*;
use kyoso_drag::two_d::Draggable2d;
use kyoso_figma::paint::Paint;
use kyoso_figma::{FigmaEdge, FigmaNode, Frame, Size};
use kyoso_graph::components::{EdgeFrom, EdgeTo};

use crate::msg::{AppEvent, ExternalId, GraphMessageExt};
use crate::weave::{
    AnnotationMarker, CommentMarker, DependencyMarker, ReferenceMarker, WeaveEdgeKind,
};

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

/// Reverse cache `Entity → ExternalId` for nodes and edges.
///
/// The sync layer's `EntityCrdtIndex` loses the `Entity → ExternalId`
/// row at the same time the entity is despawned — by the time
/// `RemovedComponents` fires we can no longer look it up. This cache
/// holds onto it long enough to emit one `NodeRemoved`/`EdgeRemoved`.
#[derive(Resource, Default)]
pub struct ExternalIdCache {
    pub nodes: HashMap<Entity, ExternalId>,
    pub edges: HashMap<Entity, ExternalId>,
}

// ---------------------------------------------------------------------------
// Frame visuals
// ---------------------------------------------------------------------------

/// Default frame visual size (used when a Frame is spawned without an
/// explicit `Size`).
const DEFAULT_FRAME_SIZE: Vec2 = Vec2::new(180.0, 100.0);

/// On-add observer for `Frame` entities: attach a 2D rectangle mesh +
/// material + drag handle. Visual size comes from the `Size` component
/// when present, else `DEFAULT_FRAME_SIZE`. Color comes from the first
/// `Solid` paint in `Frame.fills`, else a light-grey default.
pub fn on_frame_added(
    trigger: On<Add, Frame>,
    frames: Query<(&Frame, Option<&Size>)>,
    mut commands: Commands,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    let entity = trigger.entity;
    let Ok((frame, size)) = frames.get(entity) else {
        return;
    };
    // `Visibility` is required for the mesh to enter the render /
    // picking pipeline. `kyoso_figma::Frame` can't `#[require]` it
    // because kyoso_figma uses bevy with `default-features = false`
    // (no bevy_render). Insert it here on every frame add (local
    // spawn AND remote `InsertSchemaProjected`) — `insert` is
    // idempotent if the component is already present.
    commands
        .entity(entity)
        .insert((Visibility::default(), Draggable2d::default()));
    if let (Some(mut meshes), Some(mut materials)) = (meshes, materials) {
        let dims = size.map_or(DEFAULT_FRAME_SIZE, |s| Vec2::new(s.width, s.height));
        let mesh = meshes.add(Mesh::from(RectangleShape::new(
            dims.x.max(1.0),
            dims.y.max(1.0),
        )));
        let color = frame_solid_color(frame);
        let material = materials.add(ColorMaterial::from_color(color));
        commands
            .entity(entity)
            .insert((Mesh2d(mesh), MeshMaterial2d(material)));
    }
}

fn frame_solid_color(frame: &Frame) -> Color {
    frame
        .fills
        .iter()
        .find_map(|p| match p {
            Paint::Solid { color } => {
                Some(Color::srgba(color[0], color[1], color[2], color[3]))
            }
            _ => None,
        })
        .unwrap_or(Color::srgb(0.85, 0.86, 0.90))
}

/// Re-bake mesh dimensions and material colour whenever the `Frame` or
/// its `Size` changes. Required because `on_frame_added` bakes the mesh
/// at the instant the component is first inserted — and for **remotely-
/// applied** spawns, that instant is *before* the trailing
/// `SetNodeProperty` ops have updated the field values.
pub fn sync_frame_visuals(
    frames: Query<
        (&Frame, Option<&Size>, &Mesh2d, &MeshMaterial2d<ColorMaterial>),
        Or<(Changed<Frame>, Changed<Size>)>,
    >,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    for (frame, size, mesh_h, mat_h) in frames.iter() {
        let dims = size.map_or(DEFAULT_FRAME_SIZE, |s| Vec2::new(s.width, s.height));
        if let Some(mut mesh) = meshes.get_mut(&mesh_h.0) {
            *mesh = Mesh::from(RectangleShape::new(
                dims.x.max(1.0),
                dims.y.max(1.0),
            ));
        }
        if let Some(mut mat) = materials.get_mut(&mat_h.0) {
            mat.color = frame_solid_color(frame);
        }
    }
}

// ---------------------------------------------------------------------------
// Edge visuals
// ---------------------------------------------------------------------------

/// On-add observer for `FigmaEdge` entities. The actual drawing is
/// done immediate-mode by [`draw_edges_with_gizmos`] every frame, so
/// the only thing the observer needs to do is make sure the entity has
/// a `Transform` (used internally by Bevy's hierarchy machinery; not
/// used by the gizmo renderer itself).
pub fn on_figma_edge_added(
    trigger: On<Add, FigmaEdge>,
    mut commands: Commands,
) {
    commands.entity(trigger.entity).insert(Transform::default());
}

/// Per-frame: draw every figma-edge as a 2D gizmo line from its source
/// frame's world position to its target frame's world position.
/// Color matches the edge's `WeaveEdgeKind` (or default Reference blue
/// if no marker is present).
///
/// Bevy's built-in `Gizmos` API renders through the active 2D camera;
/// `kyoso_polyline` only queues into 3D render phases and so doesn't
/// show up under a `Camera2d`. Gizmos are immediate-mode (no entity
/// lifecycle) which also simplifies cleanup for the in-progress drag
/// ghost line in the Connect tool.
pub fn draw_edges_with_gizmos(
    mut gizmos: Gizmos,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), With<FigmaEdge>>,
    refs: Query<(), With<ReferenceMarker>>,
    deps: Query<(), With<DependencyMarker>>,
    comments: Query<(), With<CommentMarker>>,
    annots: Query<(), With<AnnotationMarker>>,
    transforms: Query<&Transform, With<Frame>>,
) {
    for (entity, from, to) in edges.iter() {
        let (Ok(from_t), Ok(to_t)) = (transforms.get(from.0), transforms.get(to.0)) else {
            continue;
        };
        let kind = crate::weave::kind_of_entity(entity, &refs, &deps, &comments, &annots)
            .unwrap_or(WeaveEdgeKind::Reference);
        gizmos.line_2d(
            from_t.translation.truncate(),
            to_t.translation.truncate(),
            kind.color(),
        );
    }
}

// ---------------------------------------------------------------------------
// AppEvent projection
// ---------------------------------------------------------------------------

/// Project newly-added node entities (Frames) into
/// `AppEvent::Graph(NodeAppeared)`.
pub fn emit_node_appeared(
    nodes: Query<(Entity, &Transform), Added<FigmaNode>>,
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
            position: transform.translation.truncate().into(),
        }));
    }
}

/// Project Transform changes on figma node entities into
/// `AppEvent::Graph(NodeMoved)`.
pub fn emit_node_moved(
    moved: Query<(Entity, &Transform), (Changed<Transform>, With<FigmaNode>)>,
    index: Res<SyncedIndex>,
    mut events: MessageWriter<AppEvent>,
) {
    for (entity, transform) in moved.iter() {
        let Some(&id) = index.node_of_entity.get(&entity) else {
            continue;
        };
        events.write(AppEvent::Graph(GraphMessageExt::NodeMoved {
            id,
            position: transform.translation.truncate().into(),
        }));
    }
}

/// Project despawned node entities into `AppEvent::Graph(NodeRemoved)`.
pub fn emit_node_removed(
    mut removed: RemovedComponents<FigmaNode>,
    mut cache: ResMut<ExternalIdCache>,
    mut events: MessageWriter<AppEvent>,
) {
    for entity in removed.read() {
        if let Some(id) = cache.nodes.remove(&entity) {
            events.write(AppEvent::Graph(GraphMessageExt::NodeRemoved { id }));
        }
    }
}

/// Project newly-added edge entities into
/// `AppEvent::Graph(EdgeAppeared)`.
pub fn emit_edge_appeared(
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), Added<FigmaEdge>>,
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

/// Project despawned edge entities into `AppEvent::Graph(EdgeRemoved)`.
pub fn emit_edge_removed(
    mut removed: RemovedComponents<FigmaEdge>,
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
