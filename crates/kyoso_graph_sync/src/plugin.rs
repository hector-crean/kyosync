//! Graph CRDT Bevy plugin.
//!
//! Sits on top of [`kyoso_sync::SyncTransportPlugin`]:
//!
//! - Registers [`graph_model()`](kyoso_graph_crdt::graph_model) with
//!   [`ModelRegistry`](kyoso_sync::ModelRegistry) so the transport's
//!   initial Hello includes the graph model.
//! - Inserts a [`ClientSyncEngine`] resource sharing
//!   [`PeerIdGen`](kyoso_sync::PeerIdGen) so cross-model IDs stay safe.
//! - Inbound: reads [`WsInbound`](kyoso_sync::WsInbound) Bevy events,
//!   filters for graph traffic, decodes graph payloads, applies + projects
//!   them to ECS, and emits [`RemoteOpApplied`] for downstream typed
//!   schema plugins.
//! - Outbound: drains [`ClientSyncEngine::drain_pending`], encodes each
//!   op as bytes, and submits via [`WsBridge`](kyoso_sync::WsBridge).
//! - Detection: the structural systems
//!   ([`detect_added_nodes`], [`detect_added_edges`],
//!   [`detect_tree_position_changes`], [`detect_removed_nodes`],
//!   [`detect_removed_edges`]) are unchanged from the pre-transport
//!   refactor.

use std::fmt::Debug;
use std::marker::PhantomData;

use bevy::prelude::*;
use kyoso_crdt::{Op, OpaqueSchemaState, PathSegment};
use kyoso_graph::components::{EdgeFrom, EdgeTo, IncomingEdges};
use kyoso_graph::queries::GraphComponent;
use kyoso_graph::tree::{OrderKey, TreeEdge, TreeParent};
use kyoso_graph_crdt::{GraphSnapshot, OpKind, graph_model};
use kyoso_sync::{ModelRegistry, PeerIdGen, SyncStatus, WsBridge, WsInbound};

use crate::category::ApplyEdgeCategory;
use crate::engine::{ClientSyncEngine, EngineSnapshot, ServerSnapshot};
use crate::index::EntityCrdtIndex;
use crate::schema_sync::{SchemaHydrators, TargetKind};

type GraphOp = Op<OpKind>;

/// Trait alias for components that can be replicated as graph nodes /
/// edges. The bound is intentionally minimal: the typed-schema property
/// pipeline does its own per-field CRDT plumbing, so `Syncable` here
/// only needs the `GraphComponent + Clone + Debug` shape that the
/// structural inbound/detection systems rely on.
pub trait Syncable:
    GraphComponent<Mutability = bevy::ecs::component::Mutable> + Clone + Debug
{
}
impl<T> Syncable for T where
    T: GraphComponent<Mutability = bevy::ecs::component::Mutable> + Clone + Debug
{
}

/// Emitted once per server-confirmed graph op as soon as the engine has
/// applied it. Typed plugins
/// ([`crate::SchemaSyncedNodeComponentPlugin`],
/// [`crate::SyncedEdgeCategoryPlugin`] projection) subscribe to this
/// stream to route ops to per-schema [`crate::SchemaDoc<S>`] instances
/// after the engine's canonical apply has run.
#[derive(Message, Event, Clone, Debug)]
pub struct RemoteOpApplied(pub GraphOp);

/// Graph-model Bevy plugin.
///
/// Two ways to add it:
///
/// - **Multi-model** (one socket carries graph + comments + …): add
///   [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) yourself,
///   then `GraphSyncPlugin::<N, E>::default()`.
/// - **Single-model** (graph only): use [`Self::new`], which also pulls
///   in `SyncTransportPlugin` so the connection is set up automatically.
///   Mirrors [`kyoso_comments_sync::CommentsSyncPlugin::new`].
///
/// ```ignore
/// // Multi-model
/// App::new()
///     .add_plugins((
///         SyncTransportPlugin::new("ws://...", "demo"),
///         GraphSyncPlugin::<MyNode, MyEdge>::default(),
///         CommentsSyncPlugin::default(),
///     ))
///     .run();
///
/// // Single-model graph (legacy convenience)
/// App::new()
///     .add_plugins(GraphSyncPlugin::<MyNode, MyEdge>::new("ws://...", "demo"))
///     .run();
/// ```
pub struct GraphSyncPlugin<N, E> {
    /// When `Some`, this plugin also adds a `SyncTransportPlugin` with
    /// these `(url, room)` parameters during `build` (single-model
    /// convenience). When `None`, the caller is expected to add the
    /// transport plugin separately (multi-model).
    transport: Option<(String, String)>,
    _phantom: PhantomData<fn() -> (N, E)>,
}

impl<N, E> Default for GraphSyncPlugin<N, E> {
    fn default() -> Self {
        Self {
            transport: None,
            _phantom: PhantomData,
        }
    }
}

impl<N, E> GraphSyncPlugin<N, E> {
    /// Single-model convenience constructor. Bundles a
    /// [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) with
    /// `url` + `room` during `build`. For multi-model apps prefer
    /// [`Self::default`] and add the transport plugin yourself.
    pub fn new(url: impl Into<String>, room: impl Into<String>) -> Self {
        Self {
            transport: Some((url.into(), room.into())),
            _phantom: PhantomData,
        }
    }
}

impl<N, E> Plugin for GraphSyncPlugin<N, E>
where
    N: Syncable,
    E: Syncable,
{
    fn build(&self, app: &mut App) {
        // Single-model convenience: pull in the transport plugin if the
        // caller didn't add one explicitly.
        if let Some((url, room)) = &self.transport {
            if !app.is_plugin_added::<kyoso_sync::SyncTransportPlugin>() {
                app.add_plugins(kyoso_sync::SyncTransportPlugin::new(
                    url.clone(),
                    room.clone(),
                ));
            }
        }
        // Idempotent: SyncTransportPlugin already inserted these, but
        // GraphSyncPlugin can be added in either order without panicking.
        app.init_resource::<ModelRegistry>();
        app.init_resource::<PeerIdGen>();

        // Register the graph model so the transport's Hello includes it.
        app.world_mut()
            .resource_mut::<ModelRegistry>()
            .register(graph_model());

        // Construct the engine sharing the peer-level IdGen handle.
        let peer_ids = app.world().resource::<PeerIdGen>().handle();
        app.insert_resource(ClientSyncEngine::with_shared_ids(peer_ids));

        app.init_resource::<EntityCrdtIndex>();
        app.add_message::<RemoteOpApplied>();
        app.init_resource::<GraphLastAck>();

        app.add_systems(
            Update,
            (
                graph_inbound_system::<N, E>,
                detect_added_nodes::<N, E>,
                detect_added_edges::<N, E>,
                detect_tree_position_changes::<N, E>,
                detect_removed_nodes::<N, E>,
                detect_removed_edges::<N, E>,
                outbound_system::<N, E>,
            )
                .chain(),
        );
    }
}

/// Tracks the last `applied_seq` we sent to the server via Ack so we
/// only emit a Ping when something actually changed.
#[derive(Resource, Default)]
pub(crate) struct GraphLastAck(pub(crate) kyoso_crdt::GlobalSeq);

// ---------------------------------------------------------------------------
// Inbound — read WsInbound events, filter for graph, apply + project
// ---------------------------------------------------------------------------

fn graph_inbound_system<N, E>(
    mut commands: Commands,
    mut events: MessageReader<WsInbound>,
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    mut remote_op_events: MessageWriter<RemoteOpApplied>,
    incoming: Query<&IncomingEdges>,
    tree_edges: Query<(), With<TreeEdge>>,
) where
    N: Syncable,
    E: Syncable,
{
    let graph = graph_model();
    for event in events.read() {
        match event {
            WsInbound::Welcome { peer, models, .. } => {
                // PeerIdGen was already updated by the transport's
                // drain_inbound_system, but call set_peer for parity
                // with the engine's internal applied_seq book-keeping.
                engine.set_peer(*peer);
                let Some(greeting) = models.iter().find(|g| g.model == graph) else {
                    tracing::warn!("Welcome did not include the graph greeting");
                    continue;
                };
                if let Some(snap_bytes) = &greeting.snapshot_payload {
                    match postcard::from_bytes::<ServerSnapshot>(snap_bytes) {
                        Ok(snap) => {
                            // The engine holds only structural state
                            // (EmptySchema). Strip the opaque typed
                            // schemas off the snapshot before handing
                            // it to engine.restore — the typed state
                            // is hydrated separately into the per-
                            // component SchemaDoc resources below.
                            let structural: EngineSnapshot = kyoso_crdt::Snapshot {
                                at_seq: snap.at_seq,
                                topology: snap.topology.clone(),
                                schemas: std::collections::BTreeMap::new(),
                            };
                            engine.restore(structural);
                            project_snapshot::<N, E>(
                                &mut commands,
                                &mut index,
                                &snap.topology,
                            );
                            // Hand the opaque typed-schema state to
                            // the registered hydrators. Deferred via
                            // Commands because the hydrator fns need
                            // `&mut World` to access per-schema
                            // `SchemaDoc<S>` resources without
                            // statically knowing the `S`.
                            let typed_state = snap.schemas;
                            commands.queue(move |world: &mut World| {
                                hydrate_typed_schemas(world, typed_state);
                            });
                        }
                        Err(e) => tracing::warn!(?e, "decode graph snapshot"),
                    }
                }
                let diff = match postcard::from_bytes::<kyoso_crdt::Diff<OpKind>>(
                    &greeting.diff_payload,
                ) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(?e, "decode graph diff");
                        continue;
                    }
                };
                for op in &diff.ops {
                    apply_one::<N, E>(
                        &mut commands,
                        &mut engine,
                        &mut index,
                        &incoming,
                        &tree_edges,
                        op,
                    );
                    remote_op_events.write(RemoteOpApplied(op.clone()));
                }
            }
            WsInbound::ModelApply { model, payload } if *model == graph => {
                let op: GraphOp = match postcard::from_bytes(payload) {
                    Ok(op) => op,
                    Err(e) => {
                        tracing::warn!(?e, "decode graph Apply payload");
                        continue;
                    }
                };
                apply_one::<N, E>(
                    &mut commands,
                    &mut engine,
                    &mut index,
                    &incoming,
                    &tree_edges,
                    &op,
                );
                remote_op_events.write(RemoteOpApplied(op));
            }
            WsInbound::ModelApplyBatch { model, payloads } if *model == graph => {
                for payload in payloads {
                    let op: GraphOp = match postcard::from_bytes(payload) {
                        Ok(op) => op,
                        Err(e) => {
                            tracing::warn!(?e, "decode graph ApplyBatch payload");
                            continue;
                        }
                    };
                    apply_one::<N, E>(
                        &mut commands,
                        &mut engine,
                        &mut index,
                        &incoming,
                        &tree_edges,
                        &op,
                    );
                    remote_op_events.write(RemoteOpApplied(op));
                }
            }
            WsInbound::ModelCatchup { model, payload } if *model == graph => {
                let diff: kyoso_crdt::Diff<OpKind> = match postcard::from_bytes(payload) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(?e, "decode graph Catchup payload");
                        continue;
                    }
                };
                for op in &diff.ops {
                    apply_one::<N, E>(
                        &mut commands,
                        &mut engine,
                        &mut index,
                        &incoming,
                        &tree_edges,
                        op,
                    );
                    remote_op_events.write(RemoteOpApplied(op.clone()));
                }
            }
            // Ignore non-graph events; other model plugins handle them.
            _ => {}
        }
    }
}

fn apply_one<N, E>(
    commands: &mut Commands,
    engine: &mut ClientSyncEngine,
    index: &mut EntityCrdtIndex,
    incoming: &Query<&IncomingEdges>,
    tree_edges: &Query<(), With<TreeEdge>>,
    op: &GraphOp,
) where
    N: Syncable,
    E: Syncable,
{
    if let Err(e) = engine.apply_remote(op) {
        tracing::warn!(?e, "apply_remote rejected op {op:?}");
        return;
    }
    if let OpKind::Move {
        target,
        new_parent,
        position,
    } = &op.kind
    {
        if engine.tree_parent(*target) == *new_parent {
            project_move::<N, E>(
                commands,
                index,
                incoming,
                tree_edges,
                *target,
                *new_parent,
                position,
            );
        }
        return;
    }
    project_op::<N, E>(commands, index, op);
}

fn project_op<N, E>(commands: &mut Commands, index: &mut EntityCrdtIndex, op: &GraphOp)
where
    N: Syncable,
    E: Syncable,
{
    match &op.kind {
        OpKind::AddNode => {
            if !index.entity_of_node.contains_key(&op.id) {
                let entity = commands.spawn(N::default()).id();
                index.node_of_entity.insert(entity, op.id);
                index.entity_of_node.insert(op.id, entity);
            }
        }
        OpKind::AddRefEdge { from, to, category } => {
            project_edge::<N, E>(commands, index, op.id, *from, *to);
            if let Some(entity) = index.entity_of_edge.get(&op.id).copied() {
                let category = category.clone();
                commands.queue(ApplyEdgeCategory { entity, category });
            }
        }
        OpKind::SetNodeProperty { .. } | OpKind::SetRefEdgeProperty { .. } => {
            // Handled by per-schema typed plugins via RemoteOpApplied.
        }
        OpKind::RemoveNode { target } => {
            if let Some(entity) = index.entity_of_node.remove(target) {
                index.node_of_entity.remove(&entity);
                commands.entity(entity).despawn();
            }
        }
        OpKind::RemoveRefEdge { target } => {
            if let Some(entity) = index.entity_of_edge.remove(target) {
                index.edge_of_entity.remove(&entity);
                commands.entity(entity).despawn();
            }
        }
        OpKind::Move { .. } => {
            // Handled in `apply_one` directly so it can read the tree-edge query.
        }
    }
}

fn project_move<N, E>(
    commands: &mut Commands,
    index: &EntityCrdtIndex,
    incoming: &Query<&IncomingEdges>,
    tree_edges: &Query<(), With<TreeEdge>>,
    target: kyoso_crdt::CrdtId,
    new_parent: Option<kyoso_crdt::CrdtId>,
    position: &str,
) where
    N: Syncable,
    E: Syncable,
{
    let Some(&target_entity) = index.entity_of_node.get(&target) else {
        return;
    };
    if let Ok(inc) = incoming.get(target_entity) {
        for edge_entity in inc.iter().collect::<Vec<_>>() {
            if tree_edges.get(edge_entity).is_ok() {
                commands.entity(edge_entity).despawn();
            }
        }
    }
    let new_parent_entity = new_parent.and_then(|p| index.entity_of_node.get(&p).copied());
    commands
        .entity(target_entity)
        .insert((TreeParent(new_parent_entity), OrderKey(position.to_string())));
    if let Some(parent_entity) = new_parent_entity {
        commands
            .entity(parent_entity)
            .with_related_entities::<EdgeFrom>(|rel| {
                rel.spawn((EdgeTo(target_entity), TreeEdge));
            });
    }
}

fn project_edge<N, E>(
    commands: &mut Commands,
    index: &mut EntityCrdtIndex,
    edge_id: kyoso_crdt::CrdtId,
    from: kyoso_crdt::CrdtId,
    to: kyoso_crdt::CrdtId,
) where
    N: Syncable,
    E: Syncable,
{
    if index.entity_of_edge.contains_key(&edge_id) {
        return;
    }
    let (Some(&from_entity), Some(&to_entity)) = (
        index.entity_of_node.get(&from),
        index.entity_of_node.get(&to),
    ) else {
        tracing::warn!(?edge_id, ?from, ?to, "endpoint nodes missing for edge");
        return;
    };
    let entity = commands
        .spawn((EdgeFrom(from_entity), EdgeTo(to_entity), E::default()))
        .id();
    index.edge_of_entity.insert(entity, edge_id);
    index.entity_of_edge.insert(edge_id, entity);
}

fn project_snapshot<N, E>(
    commands: &mut Commands,
    index: &mut EntityCrdtIndex,
    topo: &GraphSnapshot,
) where
    N: Syncable,
    E: Syncable,
{
    for n in &topo.nodes {
        if !index.entity_of_node.contains_key(&n.id) {
            let entity = commands.spawn(N::default()).id();
            index.node_of_entity.insert(entity, n.id);
            index.entity_of_node.insert(n.id, entity);
            if let Some(key) = &n.order_key {
                commands.entity(entity).insert(OrderKey(key.clone()));
            }
        }
    }
    for n in &topo.nodes {
        if let Some(parent_id) = n.tree_parent {
            let (Some(&child_e), Some(&parent_e)) = (
                index.entity_of_node.get(&n.id),
                index.entity_of_node.get(&parent_id),
            ) else {
                continue;
            };
            commands
                .entity(child_e)
                .insert(TreeParent(Some(parent_e)));
            commands
                .entity(parent_e)
                .with_related_entities::<EdgeFrom>(|rel| {
                    rel.spawn((EdgeTo(child_e), TreeEdge));
                });
        }
    }
    for e in &topo.edges {
        project_edge::<N, E>(commands, index, e.id, e.from, e.to);
    }
}

/// Install the opaque typed-schema state from a snapshot into the
/// registered per-component `SchemaDoc` resources.
///
/// Runs deferred via `commands.queue(...)` from the Welcome handler so
/// it has `&mut World` (needed to call the runtime-dispatched
/// [`HydratorFn`](crate::schema_sync::HydratorFn)s — each one talks to
/// a different `SchemaDoc<C::Schema>` resource type, so the dispatch
/// table is keyed by `(TargetKind, schema name)` and resolved at run
/// time).
///
/// By the time this runs, [`project_snapshot`] has already created and
/// indexed every entity present in the snapshot, so the node/edge
/// classification below is reliable.
fn hydrate_typed_schemas(
    world: &mut World,
    typed_state: std::collections::BTreeMap<kyoso_crdt::CrdtId, OpaqueSchemaState>,
) {
    let hydrators = match world.get_resource::<SchemaHydrators>() {
        Some(h) if !h.by_key.is_empty() => h.by_key.clone(),
        _ => return,
    };
    // Snapshot the index keys for classification — release the borrow
    // before invoking hydrators (they need mutable World access).
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
            // Target was tombstoned before the snapshot, so it isn't in
            // the topology projection. Nothing to hydrate.
            continue;
        };
        for (path, field) in opaque_state.fields {
            let Some(PathSegment::Field(name)) = path.0.first().cloned() else {
                continue;
            };
            let inner_path = kyoso_crdt::Path(path.0[1..].to_vec());
            if let Some(hydrate_fn) = hydrators.get(&(kind, name)).copied() {
                hydrate_fn(world, target, inner_path, field);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Detection — local mutations → CRDT ops
// ---------------------------------------------------------------------------

pub(crate) fn detect_added_nodes<N, E>(
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    added: Query<Entity, Added<N>>,
) where
    N: Syncable,
    E: Syncable,
{
    for entity in added.iter() {
        if index.node_id(entity).is_some() {
            continue;
        }
        let id = engine.add_node();
        index.bind_node(entity, id);
    }
}

fn detect_tree_position_changes<N, E>(
    mut engine: ResMut<ClientSyncEngine>,
    index: Res<EntityCrdtIndex>,
    nodes: Query<
        (Entity, &TreeParent, &OrderKey),
        Or<(Changed<TreeParent>, Changed<OrderKey>)>,
    >,
) where
    N: Syncable,
    E: Syncable,
{
    for (entity, parent, key) in nodes.iter() {
        let Some(&child_id) = index.node_of_entity.get(&entity) else {
            continue;
        };
        let new_parent_id = parent
            .0
            .and_then(|p| index.node_of_entity.get(&p).copied());
        if engine.tree_parent(child_id) == new_parent_id
            && engine.node_order_key(child_id) == Some(key.0.as_str())
        {
            continue;
        }
        engine.move_node(child_id, new_parent_id, key.0.clone());
    }
}

pub(crate) fn detect_added_edges<N, E>(
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), (Added<E>, Without<TreeEdge>)>,
) where
    N: Syncable,
    E: Syncable,
{
    for (edge_entity, from, to) in edges.iter() {
        if index.edge_of_entity.contains_key(&edge_entity) {
            continue;
        }
        let (Some(&from_id), Some(&to_id)) = (
            index.node_of_entity.get(&from.0),
            index.node_of_entity.get(&to.0),
        ) else {
            continue;
        };
        let edge_id = engine.add_edge(from_id, to_id);
        index.edge_of_entity.insert(edge_entity, edge_id);
        index.entity_of_edge.insert(edge_id, edge_entity);
    }
}

fn detect_removed_nodes<N, E>(
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    mut removed: RemovedComponents<N>,
) where
    N: Syncable,
    E: Syncable,
{
    for entity in removed.read() {
        let Some(node_id) = index.node_of_entity.remove(&entity) else {
            continue;
        };
        index.entity_of_node.remove(&node_id);
        engine.remove_node(node_id);
    }
}

fn detect_removed_edges<N, E>(
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    mut removed: RemovedComponents<E>,
) where
    N: Syncable,
    E: Syncable,
{
    for entity in removed.read() {
        let Some(edge_id) = index.edge_of_entity.remove(&entity) else {
            continue;
        };
        index.entity_of_edge.remove(&edge_id);
        engine.remove_edge(edge_id);
    }
}

// ---------------------------------------------------------------------------
// Outbound — drain pending + ack
// ---------------------------------------------------------------------------

pub(crate) fn outbound_system<N, E>(
    mut engine: ResMut<ClientSyncEngine>,
    bridge: Option<Res<WsBridge>>,
    status: Res<SyncStatus>,
    mut last_ack: ResMut<GraphLastAck>,
) where
    N: Syncable,
    E: Syncable,
{
    if !status.is_connected() {
        return;
    }
    let Some(bridge) = bridge else { return };
    let graph = graph_model();
    let pending = engine.drain_pending();
    for op in pending {
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
