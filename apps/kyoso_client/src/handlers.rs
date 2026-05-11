//! Top-level `AppCommand` dispatch and umbrella `AppEvent` projection.
//!
//! ## Dispatch (`AppCommand → internal streams`)
//!
//! The fan-out hub. Each variant of [`AppCommand`] is either handled
//! here directly (app-wide state changes, sync-layer control, graph
//! topology) or forwarded into the contributing plugin's leaf message
//! stream (`MessageWriter<SelectCommand>`, `MessageWriter<CreateCommand>`).
//!
//! Tool plugins gate their handlers on `run_if(in_state(Tool::X))` so
//! only the active tool acts on a forwarded command.
//!
//! The downstream tool message writers fire even when the tool isn't
//! active — that's intentional: it makes "switch tool then immediately
//! run a command" work in one frame (`SetTool(...)` + `Tool(Create(...))`),
//! and it's harmless because the gated reader simply doesn't poll the
//! buffer while inactive (Bevy auto-drains uncollected messages).
//!
//! ## Projection (`internal state → AppEvent`)
//!
//! Sync-layer transitions ([`emit_connected_once`], [`emit_disconnected`])
//! and tool transitions ([`emit_tool_changed`]) are projected here. The
//! topology projections (node/edge appeared/moved/removed) live next to
//! the components they observe, in [`crate::scene`].

use bevy::prelude::*;
use kyoso_figma::FigmaEdge;
use kyoso_graph::components::{EdgeFrom, EdgeTo};

use crate::msg::{
    AppCommand, AppEvent, ExternalId, GraphCommandExt, SyncCommand, SyncEvent,
};
use crate::tool::{
    ConnectCommand, ConnectKind, CreateCommand, SelectCommand, Tool, ToolCommand, ToolEvent,
};
use crate::weave::{insert_marker_for, WeaveEdgeKind};

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

pub fn dispatch_app_commands(
    mut commands: Commands,
    mut reader: MessageReader<AppCommand>,
    mut next_tool: ResMut<NextState<Tool>>,
    mut connect_kind: ResMut<ConnectKind>,
    mut select_w: MessageWriter<SelectCommand>,
    mut create_w: MessageWriter<CreateCommand>,
    mut connect_w: MessageWriter<ConnectCommand>,
    mut events: MessageWriter<AppEvent>,
    index: Res<SyncedIndex>,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), With<FigmaEdge>>,
) {
    for cmd in reader.read() {
        match cmd {
            AppCommand::SetTool(tool) => next_tool.set(*tool),

            AppCommand::SetConnectKind(kind) => {
                connect_kind.0 = *kind;
            }

            AppCommand::Tool(ToolCommand::Select(c)) => {
                select_w.write(c.clone());
            }
            AppCommand::Tool(ToolCommand::Create(c)) => {
                create_w.write(c.clone());
            }
            AppCommand::Tool(ToolCommand::Connect(c)) => {
                connect_w.write(c.clone());
            }

            AppCommand::Graph(GraphCommandExt::Connect { from, to, kind }) => {
                dispatch_connect(&mut commands, &index, &mut events, *from, *to, *kind);
            }
            AppCommand::Graph(GraphCommandExt::Disconnect { from, to }) => {
                dispatch_disconnect(&mut commands, &index, &edges, &mut events, *from, *to);
            }
            AppCommand::Graph(GraphCommandExt::RemoveNode { id }) => {
                dispatch_remove_node(&mut commands, &index, &mut events, *id);
            }
            AppCommand::Graph(GraphCommandExt::RemoveEdge { id }) => {
                dispatch_remove_edge(&mut commands, &index, &mut events, *id);
            }

            AppCommand::Sync(SyncCommand::Reconnect)
            | AppCommand::Sync(SyncCommand::RestoreFromSnapshot) => {
                events.write(AppEvent::CommandError {
                    message: "Sync control commands not yet wired in the transport".into(),
                });
            }
        }
    }
}

fn dispatch_connect(
    commands: &mut Commands,
    index: &SyncedIndex,
    events: &mut MessageWriter<AppEvent>,
    from: ExternalId,
    to: ExternalId,
    kind: WeaveEdgeKind,
) {
    let Some(&from_entity) = index.entity_of_node.get(&from) else {
        events.write(AppEvent::CommandError {
            message: format!("Graph::Connect: unknown from-id {from}"),
        });
        return;
    };
    let Some(&to_entity) = index.entity_of_node.get(&to) else {
        events.write(AppEvent::CommandError {
            message: format!("Graph::Connect: unknown to-id {to}"),
        });
        return;
    };
    let mut spawned = commands.spawn((
        FigmaEdge,
        EdgeFrom(from_entity),
        EdgeTo(to_entity),
    ));
    insert_marker_for(&mut spawned, kind);
}

fn dispatch_disconnect(
    commands: &mut Commands,
    index: &SyncedIndex,
    edges: &Query<(Entity, &EdgeFrom, &EdgeTo), With<FigmaEdge>>,
    events: &mut MessageWriter<AppEvent>,
    from: ExternalId,
    to: ExternalId,
) {
    let Some(&from_entity) = index.entity_of_node.get(&from) else {
        events.write(AppEvent::CommandError {
            message: format!("Graph::Disconnect: unknown from-id {from}"),
        });
        return;
    };
    let Some(&to_entity) = index.entity_of_node.get(&to) else {
        events.write(AppEvent::CommandError {
            message: format!("Graph::Disconnect: unknown to-id {to}"),
        });
        return;
    };
    for (edge, ef, et) in edges.iter() {
        if ef.0 == from_entity && et.0 == to_entity {
            commands.entity(edge).despawn();
            return;
        }
    }
    events.write(AppEvent::CommandError {
        message: format!("Graph::Disconnect: no edge {from} -> {to}"),
    });
}

fn dispatch_remove_node(
    commands: &mut Commands,
    index: &SyncedIndex,
    events: &mut MessageWriter<AppEvent>,
    id: ExternalId,
) {
    if let Some(&entity) = index.entity_of_node.get(&id) {
        commands.entity(entity).despawn();
    } else {
        events.write(AppEvent::CommandError {
            message: format!("Graph::RemoveNode: unknown id {id}"),
        });
    }
}

fn dispatch_remove_edge(
    commands: &mut Commands,
    index: &SyncedIndex,
    events: &mut MessageWriter<AppEvent>,
    id: ExternalId,
) {
    if let Some(&entity) = index.entity_of_edge.get(&id) {
        commands.entity(entity).despawn();
    } else {
        events.write(AppEvent::CommandError {
            message: format!("Graph::RemoveEdge: unknown id {id}"),
        });
    }
}

/// Watch `SyncStatus` and emit `AppEvent::Sync(Connected)` once the
/// welcome has been processed. Fires exactly once per connection.
pub fn emit_connected_once(
    status: Res<kyoso_sync::SyncStatus>,
    mut events: MessageWriter<AppEvent>,
    mut announced: Local<bool>,
) {
    if *announced {
        return;
    }
    if let kyoso_sync::SyncStatus::Connected { peer } = *status {
        events.write(AppEvent::Sync(SyncEvent::Connected { peer }));
        *announced = true;
    }
}

/// Emit `AppEvent::Sync(Disconnected)` when the sync status transitions
/// into `Disconnected`. Fires exactly once per disconnect.
pub fn emit_disconnected(
    status: Res<kyoso_sync::SyncStatus>,
    mut events: MessageWriter<AppEvent>,
    mut last_was_disconnected: Local<bool>,
) {
    let now_disconnected = matches!(*status, kyoso_sync::SyncStatus::Disconnected);
    if now_disconnected && !*last_was_disconnected {
        events.write(AppEvent::Sync(SyncEvent::Disconnected));
    }
    *last_was_disconnected = now_disconnected;
}

/// Emit `AppEvent::Tool(Switched { tool })` whenever `State<Tool>` changes.
/// Fires once per transition regardless of cause (UI click, hotkey,
/// programmatic `SetTool`).
pub fn emit_tool_changed(
    state: Res<State<Tool>>,
    mut events: MessageWriter<AppEvent>,
    mut last: Local<Option<Tool>>,
) {
    let now = *state.get();
    if Some(now) != *last {
        events.write(AppEvent::Tool(ToolEvent::Switched { tool: now }));
        *last = Some(now);
    }
}

/// Forward per-tool sub-events into the umbrella `AppEvent::Tool(...)`
/// stream. Each tool plugin writes its own [`SelectEvent`]/[`CreateEvent`]
/// internally; this system tags-and-bumps them onto the external bus
/// without each tool needing to know about `AppEvent`.
pub fn forward_tool_events(
    mut select_r: MessageReader<crate::tool::SelectEvent>,
    mut create_r: MessageReader<crate::tool::CreateEvent>,
    mut connect_r: MessageReader<crate::tool::ConnectEvent>,
    mut events: MessageWriter<AppEvent>,
) {
    for ev in select_r.read() {
        events.write(AppEvent::Tool(ToolEvent::Select(ev.clone())));
    }
    for ev in create_r.read() {
        events.write(AppEvent::Tool(ToolEvent::Create(ev.clone())));
    }
    for ev in connect_r.read() {
        events.write(AppEvent::Tool(ToolEvent::Connect(ev.clone())));
    }
}
