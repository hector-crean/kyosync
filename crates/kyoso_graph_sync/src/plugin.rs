//! Slim, generics-free graph sync Bevy plugin.
//!
//! Sits on top of [`kyoso_sync::SyncTransportPlugin`] and registers
//! the graph model. Express graph structure as two normal
//! [`SchemaSync`](kyoso_sync::SchemaSync) components —
//! [`NodePresence`](crate::structural::NodePresence) and
//! [`EdgeEndpoints`](crate::structural::EdgeEndpoints) — which ride
//! the standard component-sync pipeline. The plugin itself only:
//!
//! 1. Wires the `EntityCrdtIndex` resource + the
//!    [`ClientSyncEngine`] (still used as the shared id source,
//!    `applied_seq` tracker, and outbound queue).
//! 2. Drives id assignment on local spawn + tombstone-emit on local
//!    despawn (via systems in [`crate::structural`]).
//! 3. Decodes inbound `WsInbound` graph traffic, spawns placeholder
//!    entities for any unknown `CrdtId`, applies ops to the engine,
//!    emits [`RemoteOpApplied`] for the typed-schema plugins.
//! 4. Runs the [`resolve_pending_edges`] system to materialize Bevy
//!    relationships once both endpoints land.
//! 5. Drains the engine's pending queue out through the transport.
//!
//! ## Wiring
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_sync::{SyncTransportPlugin, SchemaSyncedComponentPlugin};
//! use kyoso_graph_sync::{
//!     GraphSyncPlugin, NodeTarget, EdgeTarget,
//!     NodePresence, EdgeEndpoints,
//! };
//!
//! #[derive(Component, Default, Debug, Clone)]
//! #[require(NodePresence)]
//! struct MyNode;
//!
//! #[derive(Component, Default, Debug, Clone)]
//! #[require(EdgeEndpoints)]
//! struct MyEdge;
//!
//! App::new()
//!     .add_plugins((
//!         SyncTransportPlugin::new("ws://...", "demo"),
//!         GraphSyncPlugin,
//!         SchemaSyncedComponentPlugin::<NodeTarget, NodePresence>::default(),
//!         SchemaSyncedComponentPlugin::<EdgeTarget, EdgeEndpoints>::default(),
//!         SchemaSyncedComponentPlugin::<NodeTarget, Transform>::default(),
//!     ))
//!     .run();
//! ```

use bevy::prelude::*;
use kyoso_crdt::{Op, OpaqueRecord, PathSegment};
use kyoso_graph_crdt::{OpKind, graph_model};
use kyoso_sync::{
    ModelRegistry, PeerIdGen, SchemaHydrators, SchemaSyncedComponentPlugin, SyncSet, SyncStatus,
    TargetKind, WsBridge, WsInbound,
};

use crate::engine::{ClientSyncEngine, ServerSnapshot};
use crate::index::EntityCrdtIndex;
use crate::structural::{
    EDGE_ENDPOINTS_SCHEMA, EdgeEndpoints, NODE_PRESENCE_SCHEMA, NodePresence,
    assign_local_edge_ids, assign_local_node_ids, despawn_tombstoned_edges,
    despawn_tombstoned_nodes, detect_local_edge_despawn, detect_local_node_despawn,
    ensure_inbound_entity, hydrate_snapshot_placeholders, resolve_pending_edges,
};

/// Graph-model alias for the op envelope. The wire payload is
/// unchanged from the legacy plugin — only the *property* variants
/// (`SetNodeProperty` / `SetRefEdgeProperty`) get used now, since
/// node/edge existence travels as `NodePresence` / `EdgeEndpoints`
/// property ops rather than dedicated structural variants.
pub type GraphOp = Op<OpKind>;

/// Emitted once per server-confirmed graph op the moment the engine
/// has applied it. The typed-schema plugins
/// ([`SchemaSyncedComponentPlugin`]) subscribe to this stream via
/// their [`SchemaTarget::Inbound`](kyoso_sync::SchemaTarget::Inbound)
/// to route ops to per-schema [`SchemaDoc`](kyoso_sync::SchemaDoc)
/// resources after the engine's canonical apply has run.
#[derive(Message, Event, Clone, Debug)]
pub struct RemoteOpApplied(pub GraphOp);

/// The graph-sync plugin. Generics-free — the graph model is now
/// defined by the two structural [`SchemaSync`](kyoso_sync::SchemaSync)
/// components ([`NodePresence`], [`EdgeEndpoints`]) plus whatever
/// per-node / per-edge user components the app adds with
/// [`SchemaSyncedComponentPlugin`].
pub struct GraphSyncPlugin {
    /// When `Some`, this plugin also adds a
    /// [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) with
    /// these `(url, room)` parameters during `build` (single-model
    /// convenience). When `None`, the caller is expected to add the
    /// transport plugin separately (multi-model setup).
    transport: Option<(String, String)>,
}

impl Default for GraphSyncPlugin {
    fn default() -> Self {
        Self { transport: None }
    }
}

impl GraphSyncPlugin {
    /// Single-model convenience constructor. Bundles a
    /// [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) so the
    /// caller doesn't have to add it explicitly.
    pub fn new(url: impl Into<String>, room: impl Into<String>) -> Self {
        Self {
            transport: Some((url.into(), room.into())),
        }
    }
}

impl Plugin for GraphSyncPlugin {
    fn build(&self, app: &mut App) {
        if let Some((url, room)) = &self.transport {
            if !app.is_plugin_added::<kyoso_sync::SyncTransportPlugin>() {
                app.add_plugins(kyoso_sync::SyncTransportPlugin::new(
                    url.clone(),
                    room.clone(),
                ));
            }
        }

        // Idempotent — SyncTransportPlugin already inserted these.
        app.init_resource::<ModelRegistry>();
        app.init_resource::<PeerIdGen>();

        // Register the graph model so the transport's Hello announces it.
        app.world_mut()
            .resource_mut::<ModelRegistry>()
            .register(graph_model());

        let peer_ids = app.world().resource::<PeerIdGen>().handle();
        app.insert_resource(ClientSyncEngine::with_shared_ids(peer_ids));

        app.init_resource::<EntityCrdtIndex>();
        app.init_resource::<GraphLastAck>();
        app.add_message::<RemoteOpApplied>();

        // Sync-pipeline phase ordering. `SchemaSyncedComponentPlugin`
        // schedules itself between Structural and Outbound.
        app.configure_sets(Update, (SyncSet::Structural, SyncSet::Outbound).chain());

        // SyncSet::Structural: in this order —
        //   1. apply inbound ops (may spawn placeholder entities)
        //   2. resolve parked edges (now that endpoints might exist)
        //   3. assign ids to locally-spawned nodes / edges
        //   4. detect remote tombstones → despawn
        //   5. detect local despawn → emit tombstone op
        app.add_systems(
            Update,
            (
                graph_inbound_system,
                resolve_pending_edges,
                assign_local_node_ids,
                assign_local_edge_ids,
                despawn_tombstoned_nodes,
                despawn_tombstoned_edges,
                detect_local_node_despawn,
                detect_local_edge_despawn,
            )
                .chain()
                .in_set(SyncSet::Structural),
        );

        // SyncSet::Outbound: drain the engine's pending queue + ack
        // applied_seq to the server.
        app.add_systems(Update, outbound_system.in_set(SyncSet::Outbound));

        // The two structural schemas ride the standard component-sync
        // pipeline like any user component. Adding them here means
        // consumers only need `SchemaSyncedComponentPlugin` for their
        // own custom components — they get the structural ones for
        // free with this plugin.
        app.add_plugins((
            SchemaSyncedComponentPlugin::<crate::schema_sync::NodeTarget, NodePresence>::default(),
            SchemaSyncedComponentPlugin::<crate::schema_sync::EdgeTarget, EdgeEndpoints>::default(),
        ));
    }
}

#[derive(Resource, Default)]
pub(crate) struct GraphLastAck(pub(crate) kyoso_crdt::GlobalSeq);

// ---------------------------------------------------------------------------
// Inbound — read WsInbound, decode, apply, project structural creation
// ---------------------------------------------------------------------------

fn graph_inbound_system(
    mut commands: Commands,
    mut events: MessageReader<WsInbound>,
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    mut remote_op_events: MessageWriter<RemoteOpApplied>,
) {
    let graph = graph_model();
    for event in events.read() {
        match event {
            WsInbound::Welcome { peer, models, .. } => {
                engine.set_peer(*peer);
                let Some(greeting) = models.iter().find(|g| g.model == graph) else {
                    tracing::warn!("Welcome did not include the graph greeting");
                    continue;
                };
                if let Some(snap_bytes) = &greeting.snapshot_payload {
                    match postcard::from_bytes::<ServerSnapshot>(snap_bytes) {
                        Ok(snap) => {
                            // Engine still tracks applied_seq + (legacy)
                            // topology. Strip the typed-schema state out
                            // before handing to engine.restore — the
                            // per-component SchemaDocs handle that.
                            let structural = kyoso_crdt::Snapshot {
                                at_seq: snap.at_seq,
                                topology: snap.topology.clone(),
                                schemas: std::collections::BTreeMap::new(),
                            };
                            engine.restore(structural);
                            // Spawn placeholders for every CrdtId the
                            // snapshot mentions; classification is by
                            // which structural schema appears in the
                            // record's path set.
                            hydrate_snapshot_placeholders(
                                &mut commands,
                                &mut index,
                                &snap.schemas,
                            );
                            let typed_state = snap.schemas;
                            commands.queue(move |world: &mut World| {
                                hydrate_typed_schemas(world, typed_state);
                            });
                        }
                        Err(e) => tracing::warn!(?e, "decode graph snapshot"),
                    }
                }
                match postcard::from_bytes::<kyoso_crdt::Diff<OpKind>>(&greeting.diff_payload) {
                    Ok(diff) => {
                        for op in &diff.ops {
                            apply_one(
                                &mut commands,
                                &mut engine,
                                &mut index,
                                &mut remote_op_events,
                                op,
                            );
                        }
                    }
                    Err(e) => tracing::warn!(?e, "decode graph diff"),
                }
            }
            WsInbound::ModelApply { model, payload } if *model == graph => {
                if let Some(op) = decode_op(payload, "Apply") {
                    apply_one(
                        &mut commands,
                        &mut engine,
                        &mut index,
                        &mut remote_op_events,
                        &op,
                    );
                }
            }
            WsInbound::ModelApplyBatch { model, payloads } if *model == graph => {
                for payload in payloads {
                    if let Some(op) = decode_op(payload, "ApplyBatch") {
                        apply_one(
                            &mut commands,
                            &mut engine,
                            &mut index,
                            &mut remote_op_events,
                            &op,
                        );
                    }
                }
            }
            WsInbound::ModelCatchup { model, payload } if *model == graph => {
                match postcard::from_bytes::<kyoso_crdt::Diff<OpKind>>(payload) {
                    Ok(diff) => {
                        for op in &diff.ops {
                            apply_one(
                                &mut commands,
                                &mut engine,
                                &mut index,
                                &mut remote_op_events,
                                op,
                            );
                        }
                    }
                    Err(e) => tracing::warn!(?e, "decode graph catchup"),
                }
            }
            _ => {}
        }
    }
}

fn decode_op(payload: &[u8], stream: &str) -> Option<GraphOp> {
    match postcard::from_bytes(payload) {
        Ok(op) => Some(op),
        Err(e) => {
            tracing::warn!(?e, stream, "decode graph payload");
            None
        }
    }
}

fn apply_one(
    commands: &mut Commands,
    engine: &mut ClientSyncEngine,
    index: &mut EntityCrdtIndex,
    remote_op_events: &mut MessageWriter<RemoteOpApplied>,
    op: &GraphOp,
) {
    eprintln!("[apply_one] op={:?}", op);
    if let Err(e) = engine.apply_remote(op) {
        tracing::warn!(?e, "apply_remote rejected op {op:?}");
        return;
    }
    // For structural property ops (NodePresence / EdgeEndpoints),
    // ensure the local entity exists before downstream typed-schema
    // projection runs.
    ensure_inbound_entity(commands, index, op);
    remote_op_events.write(RemoteOpApplied(op.clone()));
}

// ---------------------------------------------------------------------------
// Outbound — drain pending engine ops + ack
// ---------------------------------------------------------------------------

fn outbound_system(
    mut engine: ResMut<ClientSyncEngine>,
    bridge: Option<Res<WsBridge>>,
    status: Res<SyncStatus>,
    mut last_ack: ResMut<GraphLastAck>,
) {
    if !status.is_connected() {
        return;
    }
    let Some(bridge) = bridge else { return };
    let graph = graph_model();
    let peer = engine.peer();
    let pending = engine.drain_pending();
    for op in pending {
        eprintln!("[outbound peer={peer:?}] ship op={op:?}");
        let payload = match postcard::to_allocvec(&op) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(?e, "encode pending graph op");
                continue;
            }
        };
        if !bridge.submit(graph.clone(), payload) {
            return;
        }
    }
    let applied = engine.applied_seq();
    if applied > last_ack.0 && bridge.ack(graph.clone(), applied) {
        last_ack.0 = applied;
    }
}

// ---------------------------------------------------------------------------
// Snapshot hydration — typed schema state from server snapshot
// ---------------------------------------------------------------------------

fn hydrate_typed_schemas(
    world: &mut World,
    typed_state: std::collections::BTreeMap<kyoso_crdt::CrdtId, OpaqueRecord>,
) {
    let hydrators = match world.get_resource::<SchemaHydrators>() {
        Some(h) if !h.is_empty() => h.all(),
        _ => return,
    };
    let (node_ids, edge_ids): (
        std::collections::HashSet<kyoso_crdt::CrdtId>,
        std::collections::HashSet<kyoso_crdt::CrdtId>,
    ) = {
        let index = world.resource::<EntityCrdtIndex>();
        (
            index.entity_of_node.keys().copied().collect(),
            index.entity_of_edge.keys().copied().collect(),
        )
    };

    for (target, opaque_state) in typed_state {
        let kind = if node_ids.contains(&target) {
            TargetKind::Node
        } else if edge_ids.contains(&target) {
            TargetKind::Edge
        } else {
            // Pre-refactor record (no NodePresence / EdgeEndpoints
            // field) so no placeholder was spawned. Skip.
            continue;
        };
        for (path, field) in opaque_state.fields {
            let Some(PathSegment::Field(name)) = path.0.first().cloned() else {
                continue;
            };
            let inner_path = kyoso_crdt::Path(path.0[1..].to_vec());
            // For the two structural schemas, the `SchemaSyncedComponentPlugin`'s
            // own registered hydrator handles the field — we already spawned the
            // entity, so the doc state + write_back will sync `alive` /
            // `from` / `to` automatically. Just re-route via the
            // hydrators table.
            let _ = (NODE_PRESENCE_SCHEMA, EDGE_ENDPOINTS_SCHEMA);
            if let Some(hydrate_fn) = hydrators.get(&(kind, name)).copied() {
                hydrate_fn(world, target, inner_path, field);
            }
        }
    }
}
