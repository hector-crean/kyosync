//! Connect tool — click two components in succession to draw a typed
//! edge between them.
//!
//! ## UX
//!
//! - **Enter Connect mode** (toolbar Wire / Same-Net / Diff-Pair button).
//!   The active edge kind is stored in [`ConnectKind`].
//! - **First click** on a component arms the tool with that component as
//!   the source.
//! - **Second click** on a different component writes
//!   `AppCommand::Graph(GraphCommandExt::Connect { from, to, kind })`.
//!   The dispatcher spawns the edge entity with the right marker.
//! - **Esc / click on same node** cancels.

use bevy::picking::events::{Click, Pointer};
use bevy::prelude::*;
use kyoso_circuit::{CircuitEdgeKind, CircuitNode};
use serde::{Deserialize, Serialize};

use crate::msg::{AppCommand, AppEvent, ExternalId, GraphCommandExt};
use crate::tool::Tool;

/// The active edge kind for the connect tool. Set by the toolbar UI.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct ConnectKind(pub CircuitEdgeKind);

/// Programmatic-control surface for the connect tool.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum ConnectCommand {
    /// Cancel any in-progress connection.
    Cancel,
}

/// Per-tool sub-events.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ConnectEvent {
    /// First-click registered; rubber-band could begin from `from`.
    Armed { from: ExternalId },
    /// Second-click landed; an edge command was issued.
    Completed { from: ExternalId, to: ExternalId },
    /// State was reset without a completion.
    Cancelled,
}

/// Pending first-click state. `None` when no connection is in progress.
#[derive(Resource, Default, Debug)]
pub struct ConnectState {
    pub from: Option<Entity>,
}

pub struct ConnectToolPlugin;

impl Plugin for ConnectToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ConnectCommand>();
        app.add_message::<ConnectEvent>();
        app.init_resource::<ConnectState>();
        app.init_resource::<ConnectKind>();
        app.add_observer(on_node_clicked_for_connect);
        app.add_systems(
            Update,
            (
                handle_connect_commands.run_if(in_state(Tool::Connect)),
                cancel_on_escape.run_if(in_state(Tool::Connect)),
            ),
        );
        app.add_systems(OnExit(Tool::Connect), reset_connect_state);
    }
}

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

fn on_node_clicked_for_connect(
    trigger: On<Pointer<Click>>,
    state: Res<State<Tool>>,
    nodes: Query<&CircuitNode>,
    mut connect: ResMut<ConnectState>,
    kind: Res<ConnectKind>,
    index: Res<SyncedIndex>,
    mut commands_w: MessageWriter<AppCommand>,
    mut events: MessageWriter<AppEvent>,
    mut connect_events: MessageWriter<ConnectEvent>,
) {
    if !matches!(*state.get(), Tool::Connect) {
        return;
    }
    let target = trigger.entity;
    if !nodes.contains(target) {
        return;
    }

    if let Some(from_entity) = connect.from {
        if from_entity == target {
            connect.from = None;
            connect_events.write(ConnectEvent::Cancelled);
            return;
        }
        let Some(&from_id) = index.node_of_entity.get(&from_entity) else {
            connect.from = None;
            events.write(AppEvent::CommandError {
                message: "Connect: armed component has no CrdtId yet".into(),
            });
            return;
        };
        let Some(&to_id) = index.node_of_entity.get(&target) else {
            events.write(AppEvent::CommandError {
                message: "Connect: target component has no CrdtId yet".into(),
            });
            return;
        };
        commands_w.write(AppCommand::Graph(GraphCommandExt::Connect {
            from: from_id,
            to: to_id,
            kind: kind.0,
        }));
        connect_events.write(ConnectEvent::Completed {
            from: from_id,
            to: to_id,
        });
        connect.from = None;
    } else {
        let Some(&from_id) = index.node_of_entity.get(&target) else {
            return;
        };
        connect.from = Some(target);
        connect_events.write(ConnectEvent::Armed { from: from_id });
    }
}

fn handle_connect_commands(
    mut reader: MessageReader<ConnectCommand>,
    mut connect: ResMut<ConnectState>,
    mut events: MessageWriter<ConnectEvent>,
) {
    for cmd in reader.read() {
        match cmd {
            ConnectCommand::Cancel => {
                if connect.from.take().is_some() {
                    events.write(ConnectEvent::Cancelled);
                }
            }
        }
    }
}

fn cancel_on_escape(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut connect: ResMut<ConnectState>,
    mut events: MessageWriter<ConnectEvent>,
) {
    let Some(keys) = keys else {
        return;
    };
    if keys.just_pressed(KeyCode::Escape) && connect.from.take().is_some() {
        events.write(ConnectEvent::Cancelled);
    }
}

fn reset_connect_state(
    mut connect: ResMut<ConnectState>,
    mut events: MessageWriter<ConnectEvent>,
) {
    if connect.from.take().is_some() {
        events.write(ConnectEvent::Cancelled);
    }
}

/// Per-frame: while a connect is armed, draw a 3D gizmo line from the
/// source component to where the cursor projects onto the source's
/// own layer-plane. Registered in [`crate::VisualPlugin`] since it
/// requires Bevy's `GizmoPlugin` (not present under `MinimalPlugins`).
pub fn update_ghost_line(
    mut gizmos: Gizmos,
    state: Res<State<Tool>>,
    connect: Res<ConnectState>,
    kind: Res<ConnectKind>,
    transforms: Query<&Transform, With<CircuitNode>>,
    ray_map: Res<bevy::picking::backend::ray::RayMap>,
    cameras: Query<Entity, With<kyoso_camera::markers::MainCamera>>,
) {
    if !matches!(*state.get(), Tool::Connect) {
        return;
    }
    let Some(source) = connect.from else {
        return;
    };
    let Ok(source_t) = transforms.get(source) else {
        return;
    };
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    use kyoso_camera::raycast::RayMapExt;
    let Some(cursor_world) = ray_map.pointer_plane_intersection(
        camera,
        bevy::picking::pointer::PointerId::Mouse,
        Vec3::new(0.0, source_t.translation.y, 0.0),
        Vec3::Y,
    ) else {
        return;
    };
    let c = kind.0.color_srgb();
    gizmos.line(
        source_t.translation,
        cursor_world,
        Color::srgb(c[0], c[1], c[2]),
    );
}
