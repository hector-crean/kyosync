//! `kyoso_circuit_client` — the visual Bevy client for the kyoso
//! analogue-circuit designer.
//!
//! ## Architecture (3D, layered, agent-first)
//!
//! Mirrors `kyoso_client`'s shape, swapping the 2D Figma+Weave document
//! model for the 3D `kyoso_circuit` domain (Resistor / Capacitor /
//! Inductor / VoltageSource / Ground as nodes; Wire / SameNet /
//! DifferentialPair as typed edges; Signal / Power / Ground /
//! Mechanical as schema-synced [`OnLayer`](kyoso_circuit::OnLayer)
//! board layers stacked along the world Y axis).
//!
//! - **[`Tool`](tool::Tool)** is a Bevy `States`. Exactly one tool is
//!   active at a time (Select / Place / Connect). Each tool has its
//!   own plugin gated with `.run_if(in_state(Tool::X))`.
//! - **[`AppCommand`](msg::AppCommand)** is the dispatch hub. Every
//!   external producer (UI, MCP, agent, the kyoso_server) writes one
//!   of these into the [`DuplexPlugin`](msg::DuplexPlugin)'s inbound
//!   channel.
//! - **[`GLOBAL`](msg::GLOBAL)** is a process-wide handle to the same
//!   channel for places that can't hold a runtime handle.
//! - **[`AppPlugin`](AppPlugin)** is the headless logic stack. Pair
//!   with [`VisualPlugin`] for the windowed client.
//!
//! ## 3D rendering stack
//!
//! Reuses the existing `kyoso_camera` (orbit controller, analytical
//! plane raycasting) and `kyoso_drag` (3D draggables) crates already
//! in the workspace — no cross-workspace deps needed. Components are
//! `Mesh3d` + `MeshMaterial3d<StandardMaterial>`; edges are 3D gizmo
//! lines. Layer planes are drawn as faint gridded planes for
//! orientation.

use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::prelude::*;
use kyoso_camera::controller::pan_orbit_camera::OrbitCameraController;
use kyoso_camera::controller::{DefaultCameraSettings, OrbitCameraControllerPlugin};
use kyoso_camera::markers::MainCamera;
use kyoso_camera::raycast::AnalyticalPlanePickingPlugin;
use kyoso_circuit::{
    CircuitEdge, CircuitNode, DifferentialPairMarker, KyosoCircuitPlugin, SameNetMarker,
    WireMarker,
};
use kyoso_drag::three_d::DragTransform3dPlugin;
use kyoso_graph_sync::SyncedEdgeCategoryPlugin;
use kyoso_polyline::PolylinePlugin;

pub mod grid_manager;
pub mod handlers;
pub mod layer_manager;
pub mod msg;
pub mod presence;
pub mod scene;
pub mod tool;
pub mod ui;

pub use grid_manager::{GridLayout3d, GridManager, GridManagerPlugin};
pub use layer_manager::{LayerManager, LayerManagerPlugin};
pub use msg::{AppCommand, AppEvent, DuplexPlugin, GLOBAL, create_duplex_plugin};
pub use tool::{Tool, ToolsPlugin};
pub use ui::UiPlugin;

/// Headless app plugin. Wires the kyoso_circuit document model
/// (components + typed edges + board layers), CRDT sync, AppCommand
/// dispatch, per-tool plugins, and AppEvent emission. No rendering, no
/// input — pair with [`VisualPlugin`] for the windowed client.
pub struct AppPlugin {
    pub server_url: String,
    pub room: String,
}

impl Plugin for AppPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            // The circuit side: `CircuitNode`/`CircuitEdge` markers as
            // N/E, typed schemas for each component kind, plus
            // `OnLayer` for board-layer assignment.
            KyosoCircuitPlugin {
                server_url: self.server_url.clone(),
                room: self.room.clone(),
            },
            // Per-edge-category plugins: each registers an inbound
            // projector and an outbound detection system for its marker.
            SyncedEdgeCategoryPlugin::<CircuitNode, CircuitEdge, WireMarker>::default(),
            SyncedEdgeCategoryPlugin::<CircuitNode, CircuitEdge, SameNetMarker>::default(),
            SyncedEdgeCategoryPlugin::<CircuitNode, CircuitEdge, DifferentialPairMarker>::default(),
            ToolsPlugin,
            LayerManagerPlugin,
            GridManagerPlugin,
            presence::PresencePlugin,
        ));

        app.init_resource::<scene::ExternalIdCache>();

        app.add_systems(
            Update,
            (
                handlers::dispatch_app_commands,
                handlers::forward_tool_events,
                handlers::emit_connected_once,
                handlers::emit_disconnected,
                handlers::emit_tool_changed,
                scene::sync_layer_y,
                scene::emit_node_appeared,
                scene::emit_node_moved,
                scene::emit_node_removed,
                scene::emit_edge_appeared,
                scene::emit_edge_removed,
            ),
        );
    }
}

/// Visual scaffolding plugin. Requires `DefaultPlugins` already present
/// (window, render, asset, input, gizmos). Adds 3D picking + the orbit
/// camera + 3D drag input + the on-add observers that attach `Mesh3d`
/// / `MeshMaterial3d<StandardMaterial>` and the gizmo-based edge /
/// selection / layer-plane rendering.
pub struct VisualPlugin;

impl Plugin for VisualPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MeshPickingPlugin);
        app.insert_resource(DefaultCameraSettings::default());
        app.add_plugins(OrbitCameraControllerPlugin::<DefaultCameraSettings>::default());
        app.add_plugins(AnalyticalPlanePickingPlugin);
        app.add_plugins(DragTransform3dPlugin::<DefaultCameraSettings>(
            DefaultCameraSettings::default(),
        ));
        app.add_plugins(UiPlugin);
        app.add_plugins(PolylinePlugin);
        app.init_resource::<scene::CircuitEdgeMaterials>();
        app.add_systems(Startup, (setup_camera, setup_lighting));
        app.add_observer(scene::on_circuit_node_added);
        app.add_observer(scene::on_circuit_edge_added);
        app.add_systems(
            Update,
            (
                scene::sync_component_visuals,
                scene::sync_edge_material,
                scene::update_polyline_endpoints,
                grid_manager::draw_grid_planes,
                grid_manager::draw_snap_preview,
                tool::connect::update_ghost_line,
                tool::select::draw_selection_outline,
            ),
        );
    }
}

fn setup_camera(mut commands: Commands) {
    let initial_transform = Transform::from_xyz(8.0, 8.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y);
    let distance = initial_transform.translation.length();
    let orbit = OrbitCameraController::new(distance, Vec3::ZERO, initial_transform);

    commands.spawn((
        Camera3d::default(),
        Camera::default(),
        DepthPrepass,
        initial_transform,
        MainCamera,
        orbit,
    ));
}

fn setup_lighting(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            ..default()
        },
        Transform::from_xyz(5.0, 10.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// Convenience runner for a standalone native client.
pub fn run(server_url: String, room: String) -> bevy::app::AppExit {
    let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();

    GLOBAL.set_sender(ext_tx);
    GLOBAL.set_receiver(ext_rx);

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("kyoso circuit client — room={room}"),
                resolution: (1100u32, 720u32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(duplex)
        .add_plugins(AppPlugin { server_url, room })
        .add_plugins(VisualPlugin)
        .run()
}
