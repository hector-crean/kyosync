//! Select tool — click to select, Delete/Backspace to remove.
//!
//! Selection state is local to each peer (it's a UI concept, not part
//! of the replicated graph), tracked via the [`Selected`] marker
//! component. A click on a node in `Tool::Select` selects (or with
//! Shift toggles) that node. Pressing `Delete` or `Backspace` writes
//! `AppCommand::Graph(GraphCommandExt::RemoveNode { id })` for every
//! selected node — same code path as a programmatic removal — so the
//! operation replicates to peers.

use bevy::picking::events::{Click, Pointer};
use bevy::prelude::*;
use kyoso_circuit::CircuitNode;
use serde::{Deserialize, Serialize};

use crate::msg::{AppCommand, AppEvent, ExternalId, GraphCommandExt};
use crate::tool::Tool;

/// Local-only marker on entities the user has currently selected.
/// Not replicated; selection is per-peer.
#[derive(Component, Debug, Default)]
pub struct Selected;


/// Commands that target the Select tool.
#[derive(Clone, Debug, Event, Message, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum SelectCommand {
    /// Mark a node as the only selected entity, clearing any previous selection.
    Select { target: ExternalId },
    /// Toggle whether `target` is in the selection (additive).
    Toggle { target: ExternalId },
    /// Clear selection.
    ClearSelection,
    /// Delete every entity in the given list. Replicates via `RemoveNode` ops.
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

/// Observer: a circuit-component node was clicked while `Tool::Select`
/// is active. Plain click → set as the only selected; Shift-click →
/// toggle additive.
fn on_node_clicked_for_select(
    trigger: On<Pointer<Click>>,
    state: Res<State<Tool>>,
    nodes: Query<&CircuitNode>,
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

/// Per-frame: draw a yellow wireframe cuboid around every selected
/// component using gizmos. Replaces the 2D outline-as-child approach
/// from before — gizmos render under a `Camera3d`, no asset
/// management needed, and they automatically follow the entity's
/// transform without a hierarchy. Wired into [`crate::VisualPlugin`].
pub fn draw_selection_outline(
    mut gizmos: Gizmos,
    selected: Query<&Transform, (With<Selected>, With<CircuitNode>)>,
) {
    let color = Color::srgba(1.0, 0.85, 0.2, 0.9);
    for transform in selected.iter() {
        let frame = Transform::from_translation(transform.translation)
            .with_scale(Vec3::new(1.2, 0.6, 0.6));
        gizmos.cube(frame, color);
    }
}

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
