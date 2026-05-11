//! `kyoso_client` — the visual Bevy client for kyoso.
//!
//! ## Architecture (Figma-shape, agent-first)
//!
//! - **[`Tool`](tool::Tool)** is a Bevy `States`. Exactly one tool is
//!   active at a time (Select / Create / Connect / …). Each tool has
//!   its own plugin that registers its `Command` / `Event` types and
//!   gates its systems with `.run_if(in_state(Tool::X))`.
//! - **[`AppCommand`](msg::AppCommand)** is the dispatch hub. Every
//!   external producer (UI, JS, MCP, agent, the kyoso_server) writes
//!   one of these into the [`DuplexPlugin`](msg::DuplexPlugin)'s
//!   inbound channel. A small system fans top-level variants out to
//!   the right per-tool message stream.
//! - **[`GLOBAL`](msg::GLOBAL)** is a process-wide handle to the same
//!   channel. Use it from places that can't hold a runtime handle —
//!   wasm-bindgen FFI surfaces, agent tool implementations, embedded
//!   MCP servers.
//! - **[`AppPlugin`](AppPlugin)** is the headless logic stack: CRDT
//!   sync + tools + dispatch + AppEvent emission. Pair with
//!   [`VisualPlugin`] for the windowed client.
//!
//! Multiple producers can clone the same `Sender<AppCommand>` and
//! feed the same Bevy stream — crossbeam channels are MPMC. The CRDT
//! sync layer runs alongside on its own WebSocket channel as the
//! transport; CRDT activity is projected back into the umbrella as
//! semantic events ([`AppEvent::Graph`], [`AppEvent::Sync`]) by the
//! [`handlers`] and [`scene`] emit systems, so external observers see
//! one unified stream regardless of where mutations originated.

use bevy::prelude::*;
use kyoso_camera::controller::DefaultCameraSettings;
use kyoso_drag::two_d::DragTransform2dPlugin;
use kyoso_figma::KyosoFigmaPlugin;
use kyoso_polyline::prelude::PolylinePlugin;
use kyoso_graph_sync::SyncedEdgeCategoryPlugin;

pub mod handlers;
pub mod msg;
pub mod presence;
pub mod scene;
pub mod tool;
pub mod ui;
pub mod weave;

pub use msg::{create_duplex_plugin, AppCommand, AppEvent, DuplexPlugin, GLOBAL};
pub use tool::{Tool, ToolsPlugin};
pub use ui::UiPlugin;
pub use weave::{
    AnnotationMarker, CommentMarker, DependencyMarker, ReferenceMarker, WeaveEdgeKind,
};

/// Headless app plugin. Wires the figma+weave document model
/// (frames + typed cross-frame edges), CRDT sync, AppCommand dispatch,
/// per-tool plugins, and AppEvent emission. No rendering, no input —
/// pair with [`VisualPlugin`] for the windowed client.
pub struct AppPlugin {
    pub server_url: String,
    pub room: String,
}

impl Plugin for AppPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            // The figma side: `FigmaNode`/`FigmaEdge` markers as N/E,
            // typed schemas for `Frame` (and Rectangle/Text/TypeStyle/
            // Size/Transform) wired in.
            KyosoFigmaPlugin {
                server_url: self.server_url.clone(),
                room: self.room.clone(),
            },
            // The weave side: per-kind typed-edge categories. Each
            // plugin registers an inbound projector and an outbound
            // detection system for its marker.
            SyncedEdgeCategoryPlugin::<
                kyoso_figma::FigmaNode,
                kyoso_figma::FigmaEdge,
                ReferenceMarker,
            >::default(),
            SyncedEdgeCategoryPlugin::<
                kyoso_figma::FigmaNode,
                kyoso_figma::FigmaEdge,
                DependencyMarker,
            >::default(),
            SyncedEdgeCategoryPlugin::<
                kyoso_figma::FigmaNode,
                kyoso_figma::FigmaEdge,
                CommentMarker,
            >::default(),
            SyncedEdgeCategoryPlugin::<
                kyoso_figma::FigmaNode,
                kyoso_figma::FigmaEdge,
                AnnotationMarker,
            >::default(),
            ToolsPlugin,
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
/// (window, render, asset, input). Adds picking + polyline rendering
/// + drag input + the on-add observers that attach `Mesh2d` /
/// `MeshMaterial2d` / `Polyline` handles.
pub struct VisualPlugin;

impl Plugin for VisualPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MeshPickingPlugin);
        app.add_plugins(PolylinePlugin);
        app.insert_resource(DefaultCameraSettings::default());
        app.add_plugins(DragTransform2dPlugin::<DefaultCameraSettings>(
            DefaultCameraSettings::default(),
        ));
        app.add_plugins(UiPlugin);
        app.add_systems(Startup, setup_camera);
        app.add_observer(scene::on_frame_added);
        app.add_observer(scene::on_figma_edge_added);
        app.add_systems(
            Update,
            (
                scene::sync_frame_visuals,
                // Gizmo-based rendering — registered here in `VisualPlugin`
                // because it requires Bevy's `GizmoPlugin` (part of
                // `DefaultPlugins`) which headless tests don't include.
                scene::draw_edges_with_gizmos,
                tool::connect::update_ghost_line,
                tool::select::add_selection_outline,
                tool::select::remove_selection_outline,
            ),
        );
    }
}

fn setup_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

/// Convenience runner for a standalone native client. Builds the app,
/// installs a single `DuplexPlugin`, wires its endpoints into the
/// process-wide [`GLOBAL`] channel so any thread / FFI / MCP server
/// in the process can push `AppCommand`s, and runs until window close.
pub fn run(server_url: String, room: String) -> bevy::app::AppExit {
    let (duplex, ext_rx, ext_tx) = create_duplex_plugin::<AppCommand, AppEvent>();

    GLOBAL.set_sender(ext_tx);
    GLOBAL.set_receiver(ext_rx);

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("kyoso client — room={room}"),
                resolution: (900u32, 600u32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(duplex)
        .add_plugins(AppPlugin { server_url, room })
        .add_plugins(VisualPlugin)
        .run()
}
