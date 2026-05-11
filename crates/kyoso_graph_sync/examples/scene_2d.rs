//! Visual 2D scene-graph demo: connect to `kyoso_server`, share a small
//! graph of circle "nodes" connected by polyline "edges". Drag a node
//! and watch every other connected peer follow.
//!
//! ## Architecture in this demo
//!
//! - **`GraphNode`** is the consumer's marker component. Structural ops
//!   (Add/Remove) flow through `GraphSyncPlugin`. Field sync is opt-in
//!   per-component; this demo doesn't sync `radius`/`color_rgb`.
//! - **`Transform`** is the spatial component. Replicated via
//!   `SchemaSyncedNodeComponentPlugin::<_, _, Transform>` (typed path
//!   using `kyoso_sync::TransformSchema`) so peers see each other's
//!   drag positions.
//! - **`GraphEdge`** is the consumer's edge component. Edges carry their
//!   own `Polyline` / `PolylineMaterial` asset handles plus `EdgeFrom`
//!   / `EdgeTo` relationships into endpoint nodes.
//! - **Drag**: `kyoso_drag::Draggable2d` writes directly to `Transform`.
//!   The sync layer detects `Changed<Transform>` and ships per-field
//!   `SetNodeProperty` ops. No mirroring / no logical-vs-presentation
//!   split — Bevy components are the source of truth, full stop.
//!
//! ## Usage
//!
//! ```bash
//! # Terminal 1: the server
//! cargo run -p kyoso_server
//!
//! # Terminal 2: peer A
//! cargo run -p kyoso_sync --example scene_2d -- demo
//!
//! # Terminal 3: peer B
//! cargo run -p kyoso_sync --example scene_2d -- demo
//! ```
//!
//! Drag a node in either window — the other follows.

use bevy::math::primitives::Circle as CircleShape;
use bevy::prelude::*;
use kyoso_camera::controller::DefaultCameraSettings;
use kyoso_drag::two_d::{DragTransform2dPlugin, Draggable2d};
use kyoso_polyline::prelude::*;
use kyoso_graph_sync::{GraphSyncPlugin, SchemaSyncedNodeComponentPlugin};
use kyoso_sync::SyncStatus;

// ---------------------------------------------------------------------------
// Synced components
// ---------------------------------------------------------------------------

/// The "node" of the visual graph. Holds the parts of the document
/// model worth replicating — purely topological/visual identity here.
/// `Transform` carries the actual position; that's a separately-synced
/// component on the same entity.
#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
#[require(Transform, Visibility)]
struct GraphNode {
    radius: f32,
    /// Hex-style RGB so reflection-based serde sees a flat struct, not
    /// the variant enum that Bevy's `Color` is. This keeps the wire
    /// payload tiny and avoids needing the type registry to know
    /// every `Color` variant.
    color_rgb: [f32; 3],
}

/// The "edge". Holds line styling. `EdgeFrom` / `EdgeTo` carry the
/// topology (already part of `kyoso_graph`).
#[derive(Component, Default, Debug, Clone, Reflect)]
#[reflect(Component, Default)]
struct GraphEdge {
    line_width: f32,
    color_rgb: [f32; 3],
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

fn main() {
    let mut argv = std::env::args().skip(1);
    let room = argv.next().unwrap_or_else(|| "scene".into());
    let url = std::env::var("KYOSO_URL").unwrap_or_else(|_| "ws://127.0.0.1:7878/ws".into());

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: format!("kyoso scene — room={room}"),
            resolution: (900u32, 600u32).into(),
            ..default()
        }),
        ..default()
    }));
    app.add_plugins(MeshPickingPlugin);
    app.add_plugins(PolylinePlugin);
    app.insert_resource(DefaultCameraSettings::default());
    app.add_plugins((
        DragTransform2dPlugin::<DefaultCameraSettings>(DefaultCameraSettings::default()),
        GraphSyncPlugin::<GraphNode, GraphEdge>::new(url, room),
        SchemaSyncedNodeComponentPlugin::<GraphNode, GraphEdge, Transform>::default(),
    ));

    app.add_systems(Startup, setup);
    app.add_observer(on_graph_node_added);
    app.add_observer(on_graph_edge_added);
    app.add_systems(
        Update,
        (
            spawn_initial_graph_once,
            update_edge_polylines,
        ),
    );

    app.run();
}

#[derive(Resource, Default)]
struct InitialGraphSpawned(bool);

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);
    commands.insert_resource(InitialGraphSpawned(false));
}

/// Spawn a small starter graph once both
/// (a) we're connected and
/// (b) no other peer has populated the room yet.
///
/// Using a one-shot flag plus a node-count check keeps two simultaneously-
/// joining peers from each spawning their own copy.
fn spawn_initial_graph_once(
    mut commands: Commands,
    status: Res<SyncStatus>,
    mut spawned: ResMut<InitialGraphSpawned>,
    nodes: Query<(), With<GraphNode>>,
) {
    if spawned.0 {
        return;
    }
    if !status.is_connected() {
        return;
    }
    // If anyone else has already populated the room, leave it alone.
    if nodes.iter().count() > 0 {
        spawned.0 = true;
        return;
    }
    spawned.0 = true;

    let positions = [
        (Vec3::new(-200.0, 100.0, 0.0), [0.85, 0.30, 0.30]),
        (Vec3::new(200.0, 100.0, 0.0), [0.30, 0.85, 0.40]),
        (Vec3::new(-200.0, -120.0, 0.0), [0.30, 0.55, 0.95]),
        (Vec3::new(200.0, -120.0, 0.0), [0.95, 0.80, 0.30]),
    ];
    let mut node_ids: Vec<Entity> = Vec::new();
    for (pos, rgb) in positions {
        let id = commands
            .spawn((
                GraphNode {
                    radius: 32.0,
                    color_rgb: rgb,
                },
                Transform::from_translation(pos),
            ))
            .id();
        node_ids.push(id);
    }

    // Connect every node to the next one to form a cycle of edges.
    for i in 0..node_ids.len() {
        let from = node_ids[i];
        let to = node_ids[(i + 1) % node_ids.len()];
        commands.spawn((
            GraphEdge {
                line_width: 4.0,
                color_rgb: [0.6, 0.6, 0.7],
            },
            kyoso_graph::components::EdgeFrom(from),
            kyoso_graph::components::EdgeTo(to),
        ));
    }
}

// ---------------------------------------------------------------------------
// Visual scaffolding via observers
// ---------------------------------------------------------------------------

fn on_graph_node_added(
    trigger: On<Add, GraphNode>,
    nodes: Query<&GraphNode>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let entity = trigger.entity;
    let Ok(node) = nodes.get(entity) else { return };
    let mesh = meshes.add(Mesh::from(CircleShape::new(node.radius)));
    let color = Color::srgb(node.color_rgb[0], node.color_rgb[1], node.color_rgb[2]);
    let material = materials.add(ColorMaterial::from_color(color));
    commands.entity(entity).insert((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Draggable2d::default(),
    ));
}

fn on_graph_edge_added(
    trigger: On<Add, GraphEdge>,
    edges: Query<&GraphEdge>,
    mut commands: Commands,
    mut polylines: ResMut<Assets<Polyline>>,
    mut materials: ResMut<Assets<PolylineMaterial>>,
) {
    let entity = trigger.entity;
    let Ok(edge) = edges.get(entity) else { return };
    let polyline = polylines.add(Polyline {
        vertices: vec![Vec3::ZERO, Vec3::ZERO],
        colors: None,
    });
    let material = materials.add(PolylineMaterial {
        width: edge.line_width,
        color: LinearRgba::from_f32_array_no_alpha(edge.color_rgb),
        depth_bias: -0.0001,
        perspective: false,
    });
    commands.entity(entity).insert((
        PolylineHandle(polyline),
        PolylineMaterialHandle(material),
    ));
}

/// Polylines need their vertex buffer updated whenever an endpoint moves.
/// Run on every frame for any edge whose either endpoint's `Transform`
/// has changed.
fn update_edge_polylines(
    edges: Query<
        (
            &kyoso_graph::components::EdgeFrom,
            &kyoso_graph::components::EdgeTo,
            &PolylineHandle,
        ),
        With<GraphEdge>,
    >,
    transforms: Query<&Transform>,
    mut polylines: ResMut<Assets<Polyline>>,
) {
    for (from, to, handle) in edges.iter() {
        let (Ok(from_t), Ok(to_t)) = (transforms.get(from.0), transforms.get(to.0)) else {
            continue;
        };
        if let Some(mut polyline) = polylines.get_mut(&handle.0) {
            polyline.vertices.clear();
            polyline.vertices.push(from_t.translation);
            polyline.vertices.push(to_t.translation);
        }
    }
}
