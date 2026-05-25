//! Select tool — click to select, drag to move, Delete/Backspace to remove.
//!
//! Selection state is local to each peer (it's a UI concept, not part
//! of the replicated graph), tracked via the [`Selected`] marker
//! component. A click on a node in `Tool::Select` toggles `Selected`;
//! a click on empty canvas clears all `Selected` markers. Pressing
//! `Delete` or `Backspace` writes
//! `AppCommand::Graph(GraphCommandExt::RemoveNode { id })` for every
//! currently-selected node — same code path as a programmatic
//! removal — so the operation replicates to peers.

use bevy::picking::events::{Click, Pointer};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::msg::{AppCommand, AppEvent, ExternalId, GraphCommandExt};
use kyoso_core::Frame;
use crate::tool::Tool;

/// Local-only marker on entities the user has currently selected.
/// Not replicated; selection is per-peer.
#[derive(Component, Debug, Default)]
pub struct Selected;

/// Marker on the outline child entity spawned for a Selected node, so
/// the deselection system can find and despawn it.
#[derive(Component)]
pub struct SelectionOutline;

/// Commands that target the Select tool. Wrapped by
/// `AppCommand::Tool(ToolCommand::Select(SelectCommand::...))` on the Duplex bus.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum SelectCommand {
    /// Mark a node as the only selected entity, clearing any previous selection.
    Select { target: ExternalId },
    /// Toggle whether `target` is in the selection (additive).
    Toggle { target: ExternalId },
    /// Clear selection.
    ClearSelection,
    /// Delete every entity in the given list. Replicates via
    /// `RemoveNode` ops.
    DeleteTargets { ids: Vec<ExternalId> },
}

#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SelectEvent {
    Selected { target: ExternalId },
    Toggled { target: ExternalId, selected: bool },
    SelectionCleared,
    DeletedTargets { ids: Vec<ExternalId> },
}

pub struct SelectToolPlugin;

impl Plugin for SelectToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SelectCommand>();
        app.add_message::<SelectEvent>();
        app.add_observer(on_node_clicked_for_select);
        app.add_systems(
            Update,
            (
                handle_select_commands,
                handle_delete_key.run_if(in_state(Tool::Select)),
            ),
        );
        app.add_systems(OnExit(Tool::Select), clear_selection_on_tool_exit);
    }
}

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

fn handle_select_commands(
    mut commands: Commands,
    mut reader: MessageReader<SelectCommand>,
    mut select_events: MessageWriter<SelectEvent>,
    mut app_events: MessageWriter<AppEvent>,
    index: Res<SyncedIndex>,
    selected_q: Query<Entity, With<Selected>>,
    is_selected: Query<&Selected>,
) {
    for cmd in reader.read() {
        match cmd {
            SelectCommand::Select { target } => {
                let Some(&entity) = index.entity_of_node.get(target) else {
                    app_events.write(AppEvent::CommandError {
                        message: format!("Select::Select: unknown id {target}"),
                    });
                    continue;
                };
                for e in selected_q.iter() {
                    commands.entity(e).remove::<Selected>();
                }
                commands.entity(entity).insert(Selected);
                select_events.write(SelectEvent::Selected { target: *target });
            }
            SelectCommand::Toggle { target } => {
                let Some(&entity) = index.entity_of_node.get(target) else {
                    app_events.write(AppEvent::CommandError {
                        message: format!("Select::Toggle: unknown id {target}"),
                    });
                    continue;
                };
                let now_selected = if is_selected.get(entity).is_ok() {
                    commands.entity(entity).remove::<Selected>();
                    false
                } else {
                    commands.entity(entity).insert(Selected);
                    true
                };
                select_events.write(SelectEvent::Toggled {
                    target: *target,
                    selected: now_selected,
                });
            }
            SelectCommand::ClearSelection => {
                for e in selected_q.iter() {
                    commands.entity(e).remove::<Selected>();
                }
                select_events.write(SelectEvent::SelectionCleared);
            }
            SelectCommand::DeleteTargets { ids } => {
                // Per-node `AppEvent::Graph(NodeRemoved)` is emitted by
                // `scene::emit_node_removed`; we just despawn here.
                let mut deleted = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(&entity) = index.entity_of_node.get(id) {
                        commands.entity(entity).despawn();
                        deleted.push(*id);
                    } else {
                        app_events.write(AppEvent::CommandError {
                            message: format!("Select::DeleteTargets: unknown id {id}"),
                        });
                    }
                }
                if !deleted.is_empty() {
                    select_events.write(SelectEvent::DeletedTargets { ids: deleted });
                }
            }
        }
    }
}

/// Observer: a `Frame` was clicked while `Tool::Select` is active.
/// Plain click → set as the only selected; Shift-click → toggle additive.
fn on_node_clicked_for_select(
    trigger: On<Pointer<Click>>,
    state: Res<State<Tool>>,
    nodes: Query<&Frame>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    index: Res<SyncedIndex>,
    mut commands_w: MessageWriter<AppCommand>,
) {
    if !matches!(*state.get(), Tool::Select) {
        return;
    }
    let target = trigger.entity;
    if !nodes.contains(target) {
        return;
    }
    let Some(&id) = index.node_of_entity.get(&target) else {
        return;
    };

    let additive = keys
        .map(|k| k.pressed(KeyCode::ShiftLeft) || k.pressed(KeyCode::ShiftRight))
        .unwrap_or(false);
    let cmd = if additive {
        SelectCommand::Toggle { target: id }
    } else {
        SelectCommand::Select { target: id }
    };
    commands_w.write(AppCommand::Tool(crate::tool::ToolCommand::Select(cmd)));
}

/// `Delete` or `Backspace` while `Tool::Select` is active deletes every
/// selected node. Goes through `AppCommand::Graph(RemoveNode)` so the
/// path is identical to a programmatic remove.
fn handle_delete_key(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    selected: Query<Entity, With<Selected>>,
    index: Res<SyncedIndex>,
    mut commands_w: MessageWriter<AppCommand>,
) {
    let Some(keys) = keys else {
        return;
    };
    if !(keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace)) {
        return;
    }
    for entity in selected.iter() {
        if let Some(&id) = index.node_of_entity.get(&entity) {
            commands_w.write(AppCommand::Graph(GraphCommandExt::RemoveNode { id }));
        }
    }
}

/// Spawn a yellow outline child when `Selected` is added to a node.
/// Requires `Assets<Mesh>` / `Assets<ColorMaterial>`, so it's wired
/// into [`crate::VisualPlugin`] rather than the headless tool plugin.
pub fn add_selection_outline(
    just_selected: Query<(Entity, Option<&kyoso_core::Size>), (Added<Selected>, With<Frame>)>,
    mut commands: Commands,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    for (entity, size) in just_selected.iter() {
        // Outline is a slightly larger rectangle than the frame.
        let dims = size.map_or(bevy::math::Vec2::new(180.0, 100.0), |s| {
            bevy::math::Vec2::new(s.width, s.height)
        });
        let pad = 8.0;
        let outline_mesh = meshes.add(Mesh::from(bevy::math::primitives::Rectangle::new(
            (dims.x + pad).max(1.0),
            (dims.y + pad).max(1.0),
        )));
        let outline_mat = materials.add(ColorMaterial::from_color(Color::srgba(
            1.0, 0.85, 0.2, 0.9,
        )));
        commands.entity(entity).with_children(|parent| {
            parent.spawn((
                Mesh2d(outline_mesh),
                MeshMaterial2d(outline_mat),
                Transform::from_xyz(0.0, 0.0, -0.1),
                SelectionOutline,
            ));
        });
    }
}

/// Despawn the outline child when `Selected` is removed. Pairs with
/// [`add_selection_outline`] — wired into [`crate::VisualPlugin`].
pub fn remove_selection_outline(
    mut removed: RemovedComponents<Selected>,
    children_q: Query<&Children>,
    outlines: Query<Entity, With<SelectionOutline>>,
    mut commands: Commands,
) {
    for parent in removed.read() {
        let Ok(children) = children_q.get(parent) else {
            continue;
        };
        for child in children.iter() {
            if outlines.contains(child) {
                commands.entity(child).despawn();
            }
        }
    }
}

/// On tool-exit, clear all `Selected` markers so leaving Select mode
/// always lands the user in a clean state.
fn clear_selection_on_tool_exit(
    selected: Query<Entity, With<Selected>>,
    mut commands: Commands,
    mut events: MessageWriter<SelectEvent>,
) {
    let mut any = false;
    for e in selected.iter() {
        commands.entity(e).remove::<Selected>();
        any = true;
    }
    if any {
        events.write(SelectEvent::SelectionCleared);
    }
}
