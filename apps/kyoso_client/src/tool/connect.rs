//! Connect tool — drag from a frame's connection handle to another
//! frame's handle to draw a typed edge.
//!
//! ## UX
//!
//! - **Enter Connect mode** (toolbar Reference / Dependency / Comment /
//!   Annotation button). A small blue circular handle appears on the
//!   right edge of every frame.
//! - **DragStart on a handle** arms the tool with that handle's parent
//!   frame as the source, and spawns a ghost polyline that follows the
//!   cursor.
//! - **DragDrop on another frame's handle** writes
//!   `AppCommand::Graph(GraphCommandExt::Connect { from, to, kind })`
//!   and despawns the ghost. The `kind` comes from
//!   [`ConnectKind`] — set by the toolbar's weave-kind buttons.
//! - **DragEnd anywhere else** cancels: the ghost disappears, no edge
//!   is created.
//!
//! ## Click-pair fallback
//!
//! For accessibility / keyboard input, a click-pair flow is also
//! implemented: click frame A → click frame B. Same outcome as the
//! drag-and-drop flow.
//!
//! ## State
//!
//! - [`ConnectState`] — pending click-pair source, separate from the
//!   drag flow.
//! - [`ConnectDrag`] — pending drag source + ghost line entity.
//! - [`ConnectKind`] — which weave kind is active.
//!
//! Reset on tool-exit handles both flows.

use bevy::picking::events::{Click, DragDrop, DragEnd, DragStart, Pointer};
use bevy::prelude::*;
use kyoso_core::Frame;
use serde::{Deserialize, Serialize};

use crate::msg::{AppCommand, AppEvent, ExternalId, GraphCommandExt};
use crate::tool::Tool;
use crate::weave::WeaveEdgeKind;

/// The active edge kind for the Connect tool. Set by the toolbar UI
/// when the user clicks one of the Connect-X buttons; the click-pair
/// flow reads this to know which marker to attach to the spawned edge.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct ConnectKind(pub WeaveEdgeKind);

/// Programmatic-control surface for the connect tool. Today only a
/// single variant — the interactive click-pair flow is internal to
/// this plugin and writes `AppCommand::Graph` directly. Variants for
/// programmatic begin/complete will land alongside an MCP tool surface.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum ConnectCommand {
    /// Cancel any in-progress connection drag.
    Cancel,
}

/// Per-tool sub-events emitted by the connect tool.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ConnectEvent {
    /// First-click registered. UI may render a rubber-band line from this node.
    Armed { from: ExternalId },
    /// Second-click landed; an edge command was issued.
    Completed { from: ExternalId, to: ExternalId },
    /// State was reset without a completion.
    Cancelled,
}

/// Pending first-click state for the click-pair fallback flow. `None`
/// when no connection is in progress.
#[derive(Resource, Default, Debug)]
pub struct ConnectState {
    pub from: Option<Entity>,
}

/// Pending drag-and-drop state for the handle-drag flow. The "ghost
/// line" is drawn immediate-mode via gizmos every frame while
/// `source.is_some()` — no entity to track.
#[derive(Resource, Default, Debug)]
pub struct ConnectDrag {
    /// Source frame the user started dragging from. The Frame entity,
    /// not the handle entity.
    pub source: Option<Entity>,
}

/// Marker on a small clickable circle attached as a child of each
/// Frame entity while `Tool::Connect` is active. The `frame` field
/// duplicates the Bevy `ChildOf` parent so observers don't need to
/// walk the hierarchy.
#[derive(Component, Debug, Clone, Copy)]
pub struct ConnectionHandle {
    pub frame: Entity,
}

pub struct ConnectToolPlugin;

impl Plugin for ConnectToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ConnectCommand>();
        app.add_message::<ConnectEvent>();
        app.init_resource::<ConnectState>();
        app.init_resource::<ConnectKind>();
        app.init_resource::<ConnectDrag>();

        // Click-pair fallback — observers self-gate on Tool::Connect.
        app.add_observer(on_node_clicked_for_connect);

        // Handle-drag flow — observers gate on the dragged entity
        // being a `ConnectionHandle`.
        app.add_observer(on_handle_drag_start);
        app.add_observer(on_handle_drag_drop);
        app.add_observer(on_drag_end);

        app.add_systems(
            Update,
            (
                handle_connect_commands.run_if(in_state(Tool::Connect)),
                cancel_on_escape.run_if(in_state(Tool::Connect)),
                spawn_handles_for_new_frames.run_if(in_state(Tool::Connect)),
                // `update_ghost_line` uses `Gizmos` and is registered
                // in `VisualPlugin` instead — headless tests don't
                // bring in `GizmoPlugin`.
            ),
        );
        app.add_systems(OnEnter(Tool::Connect), spawn_handles_on_enter);
        app.add_systems(OnExit(Tool::Connect), (despawn_handles_on_exit, reset_connect_state));
    }
}

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

/// Observer: a `Frame` was clicked while `Tool::Connect` is active.
/// First click arms; second click completes with the active
/// [`ConnectKind`].
fn on_node_clicked_for_connect(
    trigger: On<Pointer<Click>>,
    state: Res<State<Tool>>,
    frames: Query<&Frame>,
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
    if !frames.contains(target) {
        return;
    }

    if let Some(from_entity) = connect.from {
        if from_entity == target {
            // Click on the same frame twice — treat as cancel.
            connect.from = None;
            connect_events.write(ConnectEvent::Cancelled);
            return;
        }
        let Some(&from_id) = index.node_of_entity.get(&from_entity) else {
            connect.from = None;
            events.write(AppEvent::CommandError {
                message: "Connect: armed frame has no CrdtId yet".into(),
            });
            return;
        };
        let Some(&to_id) = index.node_of_entity.get(&target) else {
            events.write(AppEvent::CommandError {
                message: "Connect: target frame has no CrdtId yet".into(),
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

// ---------------------------------------------------------------------------
// Handle-drag flow (the "ghost line" UX)
// ---------------------------------------------------------------------------

/// Visual size + offset of the small connection handle anchored on the
/// right edge of each frame.
const HANDLE_RADIUS: f32 = 8.0;
/// Local-space offset for the handle, relative to the frame's centre.
/// Hardcoded for the default 180×100 frame; for varying sizes the
/// placement is good-enough at right-of-frame.
const HANDLE_OFFSET: Vec2 = Vec2::new(96.0, 0.0);

const HANDLE_COLOR: bevy::color::Color = bevy::color::Color::srgb(0.20, 0.55, 0.95);

/// On entry to Connect mode: spawn a handle child on every existing
/// frame.
fn spawn_handles_on_enter(
    mut commands: Commands,
    frames: Query<Entity, With<Frame>>,
    handles: Query<&ConnectionHandle>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let mesh = meshes.add(Mesh::from(bevy::math::primitives::Circle::new(HANDLE_RADIUS)));
    let material = materials.add(ColorMaterial::from_color(HANDLE_COLOR));
    let already_handled: std::collections::HashSet<Entity> =
        handles.iter().map(|h| h.frame).collect();
    for frame_entity in frames.iter() {
        if already_handled.contains(&frame_entity) {
            continue;
        }
        spawn_handle(
            &mut commands,
            frame_entity,
            mesh.clone(),
            material.clone(),
        );
    }
}

/// While Connect mode is active, also spawn handles for newly-added
/// frames (e.g. a peer pushed a new frame after we entered Connect).
fn spawn_handles_for_new_frames(
    mut commands: Commands,
    new_frames: Query<Entity, Added<Frame>>,
    existing_handles: Query<&ConnectionHandle>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    if new_frames.is_empty() {
        return;
    }
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let already_handled: std::collections::HashSet<Entity> =
        existing_handles.iter().map(|h| h.frame).collect();
    let mesh = meshes.add(Mesh::from(bevy::math::primitives::Circle::new(HANDLE_RADIUS)));
    let material = materials.add(ColorMaterial::from_color(HANDLE_COLOR));
    for frame_entity in new_frames.iter() {
        if already_handled.contains(&frame_entity) {
            continue;
        }
        spawn_handle(
            &mut commands,
            frame_entity,
            mesh.clone(),
            material.clone(),
        );
    }
}

fn spawn_handle(
    commands: &mut Commands,
    frame: Entity,
    mesh: Handle<Mesh>,
    material: Handle<ColorMaterial>,
) {
    commands.entity(frame).with_children(|parent| {
        parent.spawn((
            ConnectionHandle { frame },
            Mesh2d(mesh),
            MeshMaterial2d(material),
            Transform::from_xyz(HANDLE_OFFSET.x, HANDLE_OFFSET.y, 0.5),
            Visibility::default(),
        ));
    });
}

/// On exit from Connect mode: despawn every handle entity. Ghost line
/// is gizmo-based (no entity), so just clear the resource.
fn despawn_handles_on_exit(
    mut commands: Commands,
    handles: Query<Entity, With<ConnectionHandle>>,
    mut connect_drag: ResMut<ConnectDrag>,
) {
    for entity in handles.iter() {
        commands.entity(entity).despawn();
    }
    connect_drag.source = None;
}

/// Observer: a `ConnectionHandle` was DragStarted. Arm the source
/// frame so [`update_ghost_line`] starts drawing the ghost.
fn on_handle_drag_start(
    trigger: On<Pointer<DragStart>>,
    handles: Query<&ConnectionHandle>,
    state: Res<State<Tool>>,
    mut connect_drag: ResMut<ConnectDrag>,
) {
    if !matches!(*state.get(), Tool::Connect) {
        return;
    }
    let Ok(handle) = handles.get(trigger.entity) else {
        return;
    };
    connect_drag.source = Some(handle.frame);
}

/// While a drag is in progress, draw an immediate-mode gizmo line from
/// the source frame's world position to the cursor's world position,
/// in the active `WeaveEdgeKind`'s colour. Registered in
/// [`crate::VisualPlugin`] (not the headless tool plugin) since it
/// requires Bevy's `GizmoPlugin`. The early-out checks below also gate
/// it to `Tool::Connect` so unrelated apps don't pay the per-frame
/// cost.
pub fn update_ghost_line(
    mut gizmos: Gizmos,
    connect_drag: Res<ConnectDrag>,
    kind: Res<ConnectKind>,
    transforms: Query<&Transform, With<Frame>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) {
    let Some(source) = connect_drag.source else {
        return;
    };
    let Ok(source_t) = transforms.get(source) else {
        return;
    };
    let Some(window) = windows.iter().next() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Some((camera, cam_t)) = cameras.iter().next() else {
        return;
    };
    let Ok(cursor_world) = camera.viewport_to_world_2d(cam_t, cursor) else {
        return;
    };
    gizmos.line_2d(
        source_t.translation.truncate(),
        cursor_world,
        kind.0.color(),
    );
}

/// Observer: a drag was dropped onto a `ConnectionHandle`. Resolve
/// source + target frames and dispatch the Connect command. Cleanup
/// the ghost and resource state.
fn on_handle_drag_drop(
    trigger: On<Pointer<DragDrop>>,
    handles: Query<&ConnectionHandle>,
    state: Res<State<Tool>>,
    kind: Res<ConnectKind>,
    mut connect_drag: ResMut<ConnectDrag>,
    index: Res<SyncedIndex>,
    mut commands_w: MessageWriter<AppCommand>,
    mut events: MessageWriter<AppEvent>,
    mut connect_events: MessageWriter<ConnectEvent>,
) {
    if !matches!(*state.get(), Tool::Connect) {
        return;
    }
    let drop_target = trigger.entity;
    let dropped = trigger.event().dropped;
    // Both ends must be `ConnectionHandle`s for the drop to mean a
    // typed-edge connection. Drop-target on anything else (frame body,
    // empty space) is treated as cancel by `on_drag_end`.
    let (Ok(target_handle), Ok(source_handle)) =
        (handles.get(drop_target), handles.get(dropped))
    else {
        return;
    };
    let from_frame = source_handle.frame;
    let to_frame = target_handle.frame;
    if from_frame == to_frame {
        connect_drag.source = None;
        connect_events.write(ConnectEvent::Cancelled);
        return;
    }
    let Some(&from_id) = index.node_of_entity.get(&from_frame) else {
        connect_drag.source = None;
        events.write(AppEvent::CommandError {
            message: "Connect-drag: source frame has no CrdtId yet".into(),
        });
        return;
    };
    let Some(&to_id) = index.node_of_entity.get(&to_frame) else {
        connect_drag.source = None;
        events.write(AppEvent::CommandError {
            message: "Connect-drag: target frame has no CrdtId yet".into(),
        });
        return;
    };
    commands_w.write(AppCommand::Graph(GraphCommandExt::Connect {
        from: from_id,
        to: to_id,
        kind: kind.0,
    }));
    connect_events.write(ConnectEvent::Completed { from: from_id, to: to_id });
    connect_drag.source = None;
}

/// Observer: any drag ended. If a drag was in progress and DragDrop
/// didn't fire (i.e. the user released over empty space), the source
/// is still set — clear it.
fn on_drag_end(
    _trigger: On<Pointer<DragEnd>>,
    state: Res<State<Tool>>,
    mut connect_drag: ResMut<ConnectDrag>,
    mut connect_events: MessageWriter<ConnectEvent>,
) {
    if !matches!(*state.get(), Tool::Connect) {
        return;
    }
    if connect_drag.source.take().is_some() {
        connect_events.write(ConnectEvent::Cancelled);
    }
}
