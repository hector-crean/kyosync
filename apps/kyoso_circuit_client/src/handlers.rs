//! Top-level `AppCommand` dispatch and umbrella `AppEvent` projection.

use bevy::prelude::*;
use kyoso_circuit::{
    CircuitEdge, CircuitEdgeKind, DifferentialPairMarker, SameNetMarker, WireMarker,
    insert_marker_for,
};
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph_crdt::EdgeCategory;
use kyoso_graph_sync::{ClientSyncEngine, EdgeCategoryMarker};

use crate::msg::{AppCommand, AppEvent, ExternalId, GraphCommandExt, SyncCommand, SyncEvent};
use crate::tool::{
    ConnectCommand, ConnectKind, PlaceCommand, PlaceKind, PlaceLayer, SelectCommand, Tool,
    ToolCommand, ToolEvent,
};

type SyncedIndex = kyoso_graph_sync::EntityCrdtIndex;

pub fn dispatch_app_commands(
    mut commands: Commands,
    mut reader: MessageReader<AppCommand>,
    mut next_tool: ResMut<NextState<Tool>>,
    mut connect_kind: ResMut<ConnectKind>,
    mut place_kind: ResMut<PlaceKind>,
    mut place_layer: ResMut<PlaceLayer>,
    mut select_w: MessageWriter<SelectCommand>,
    mut place_w: MessageWriter<PlaceCommand>,
    mut connect_w: MessageWriter<ConnectCommand>,
    mut events: MessageWriter<AppEvent>,
    mut index: ResMut<SyncedIndex>,
    mut engine: ResMut<ClientSyncEngine>,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), With<CircuitEdge>>,
) {
    for cmd in reader.read() {
        match cmd {
            AppCommand::SetTool(tool) => next_tool.set(*tool),

            AppCommand::SetWireKind(kind) => {
                connect_kind.0 = *kind;
            }

            AppCommand::SetComponentKind(kind) => {
                place_kind.0 = *kind;
            }

            AppCommand::SetActiveLayer(layer) => {
                place_layer.0 = *layer;
            }

            AppCommand::Tool(ToolCommand::Select(c)) => {
                select_w.write(c.clone());
            }
            AppCommand::Tool(ToolCommand::Place(c)) => {
                place_w.write(c.clone());
            }
            AppCommand::Tool(ToolCommand::Connect(c)) => {
                connect_w.write(c.clone());
            }

            AppCommand::Graph(GraphCommandExt::SpawnComponent {
                position,
                kind,
                layer,
            }) => {
                crate::tool::place::spawn_component(&mut commands, *position, *kind, *layer);
            }
            AppCommand::Graph(GraphCommandExt::Connect { from, to, kind }) => {
                dispatch_connect(
                    &mut commands,
                    &mut index,
                    &mut engine,
                    &mut events,
                    *from,
                    *to,
                    *kind,
                );
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

fn category_for(kind: CircuitEdgeKind) -> EdgeCategory {
    match kind {
        CircuitEdgeKind::Wire => WireMarker::category(),
        CircuitEdgeKind::SameNet => SameNetMarker::category(),
        CircuitEdgeKind::DifferentialPair => DifferentialPairMarker::category(),
    }
}

fn dispatch_connect(
    commands: &mut Commands,
    index: &mut SyncedIndex,
    engine: &mut ClientSyncEngine,
    events: &mut MessageWriter<AppEvent>,
    from: ExternalId,
    to: ExternalId,
    kind: CircuitEdgeKind,
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
    // Spawn the edge entity locally with the structural marker plus the
    // per-kind marker. Then bind it to a CRDT id with the right category
    // ourselves, instead of relying on `detect_added_categorized_edges`
    // — that auto-detection races with the structural `detect_added_edges`
    // when the spawn happens inside a system with no explicit ordering
    // constraint to those detection systems, and the structural detector
    // wins, emitting an `AddRefEdge` with the default `Reference`
    // category instead of `circuit-wire` / `circuit-same-net` /
    // `circuit-diff-pair`. Doing the engine binding here is also slightly
    // more efficient: one categorised op instead of one structural op
    // immediately followed by an `Added<Marker>` in the next frame.
    let entity = commands
        .spawn((CircuitEdge, EdgeFrom(from_entity), EdgeTo(to_entity)))
        .id();
    let mut entity_commands = commands.entity(entity);
    insert_marker_for(&mut entity_commands, kind);
    let edge_id = engine.add_ref_edge_with_category(from, to, category_for(kind));
    index.bind_edge(entity, edge_id);
}

fn dispatch_disconnect(
    commands: &mut Commands,
    index: &SyncedIndex,
    edges: &Query<(Entity, &EdgeFrom, &EdgeTo), With<CircuitEdge>>,
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

pub fn forward_tool_events(
    mut select_r: MessageReader<crate::tool::SelectEvent>,
    mut place_r: MessageReader<crate::tool::PlaceEvent>,
    mut connect_r: MessageReader<crate::tool::ConnectEvent>,
    mut events: MessageWriter<AppEvent>,
) {
    for ev in select_r.read() {
        events.write(AppEvent::Tool(ToolEvent::Select(ev.clone())));
    }
    for ev in place_r.read() {
        events.write(AppEvent::Tool(ToolEvent::Place(ev.clone())));
    }
    for ev in connect_r.read() {
        events.write(AppEvent::Tool(ToolEvent::Connect(ev.clone())));
    }
}
