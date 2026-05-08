//! Bevy plugin wiring a [`ClientSyncEngine`] resource and its ECS
//! projection to a [`WsClient`] talking `kyoso_server`.
//!
//! # System pipeline (per `Update`)
//!
//! 1. **`inbound_system`** — drains `WsClient::try_recv`, applies remote
//!    ops to the engine, projects structural ops (`AddNode`, `Move`,
//!    `AddRefEdge`, etc.) into the ECS, and emits a [`RemoteOpApplied`]
//!    event per applied op.
//! 2. **Detection systems** (structural) — capture local ECS additions
//!    and removals and emit `AddNode` / `AddRefEdge` / `Move` /
//!    `Remove*` ops via the engine. All skip entities already present
//!    in the index (the case when inbound just spawned them — that's
//!    how we avoid echoing remote ops back to the server).
//! 3. **Per-component schema chains** — each `SchemaSyncedNodeComponentPlugin<N, E, C>`
//!    or `SchemaSyncedEdgeComponentPlugin<N, E, C>` adds its own chain
//!    of (detect typed-changes, route inbound, project to ECS) systems
//!    that consume `RemoteOpApplied` and emit `SetNodeProperty` /
//!    `SetRefEdgeProperty` ops via typed schema deltas.
//! 4. **`outbound_system`** — drains pending ops to the WS, then pushes
//!    a `Ping` ack with the current `applied_seq`.
//!
//! Tree-shape mutations are atomic Kleppmann moves: a single
//! [`OpKind::Move`] op carries the new parent + position. Cycle
//! detection lives on the engine; on-the-wire we ship one op per
//! reparent.
//!
//! Order is enforced via `chain()`: inbound first so the index is
//! authoritative before detection runs; outbound last so any ops
//! generated this frame ship immediately.
//!
//! # Property sync is typed-only
//!
//! Property sync (per-field CRDT mutations) is opt-in per Bevy component
//! via [`crate::SchemaSyncedNodeComponentPlugin`] /
//! [`crate::SchemaSyncedEdgeComponentPlugin`]. There is no longer a
//! reflection-driven property pipeline — every consumed component must
//! ship with a parallel `derive(Crdt)` schema struct and a
//! [`crate::SchemaSync`] impl. See [`crate::builtin_schemas`] for
//! Bevy-builtin examples (`Transform`).

use std::fmt::Debug;
use std::marker::PhantomData;

use bevy::prelude::*;
use kyoso_crdt::{GlobalSeq, PeerId};
use kyoso_graph_crdt::OpKind;

type Op = kyoso_crdt::Op<OpKind>;
use kyoso_graph::components::{EdgeFrom, EdgeTo, IncomingEdges};
use kyoso_graph::queries::GraphComponent;
use kyoso_graph::tree::{OrderKey, TreeEdge, TreeParent};

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

use crate::client::{Inbound, WsClient};

/// Connection lifecycle observable from the host. Consumers can gate
/// game logic on `is_synced()` returning true.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    AwaitingWelcome,
    Connected { peer: PeerId },
    Disconnected,
}

/// Per-peer ephemeral presence/awareness state. Bytes are opaque —
/// each consumer postcard-encodes their own struct (cursor + selection
/// + display name + colour + …). Updated by [`inbound_system`] as
/// `PresenceUpdate` / `PresenceLeft` arrive; cleared on disconnect.
///
/// **Not** part of the CRDT: no `GlobalSeq`, no log, no persistence.
/// Lost when peers disconnect.
#[derive(Resource, Default, Debug)]
pub struct RawPresence(pub std::collections::HashMap<PeerId, Vec<u8>>);

/// Push a new presence state for the local peer. Emit this from a
/// consumer-side system whenever the host's own presence changes
/// (cursor moves, selection changes, name set). The bytes are sent
/// verbatim as [`kyoso_crdt::ClientMsg::Presence`] — typically you
/// postcard-encode a typed struct on the way in.
///
/// Spam-friendly: the outbound system simply forwards every event to
/// the wire. Coalesce on the consumer side if you want to throttle
/// (e.g. cursor moves).
#[derive(Message, Event, Debug, Clone)]
pub struct SetLocalPresence(pub Vec<u8>);

/// Clear the local peer's presence (server also clears on disconnect).
#[derive(Message, Event, Debug, Clone, Copy)]
pub struct ClearLocalPresence;

/// Observation of remote peers' presence changes. Consumers read this
/// to update typed projections of [`RawPresence`].
#[derive(Message, Event, Debug, Clone)]
pub enum RawPresenceEvent {
    /// Initial room snapshot, delivered once per `Welcome`. Replaces
    /// the resource's contents (RawPresence is also overwritten in the
    /// inbound system).
    Snapshot(Vec<(PeerId, Vec<u8>)>),
    /// Peer `peer` updated their presence.
    Updated { peer: PeerId, state: Vec<u8> },
    /// Peer `peer` cleared their presence (explicit leave or disconnect).
    Left { peer: PeerId },
}

impl SyncStatus {
    pub fn is_connected(self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Emitted once per server-confirmed op as soon as the engine has
/// applied it. Typed plugins (`SchemaSyncedNodeComponentPlugin`,
/// `SyncedEdgeCategoryPlugin` projection) subscribe to this stream to
/// route ops to per-schema `Document<S>` instances after the engine's
/// canonical apply has run.
#[derive(Message, Event, Clone, Debug)]
pub struct RemoteOpApplied(pub Op);

#[derive(Resource)]
pub(crate) struct WsBridge {
    pub(crate) client: WsClient,
    pub(crate) last_acked: GlobalSeq,
}

pub struct CrdtSyncPlugin<N, E> {
    pub url: String,
    pub room: String,
    _phantom: PhantomData<fn() -> (N, E)>,
}

impl<N, E> CrdtSyncPlugin<N, E> {
    pub fn new(url: impl Into<String>, room: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            room: room.into(),
            _phantom: PhantomData,
        }
    }
}

impl<N, E> Plugin for CrdtSyncPlugin<N, E>
where
    N: Syncable,
    E: Syncable,
{
    fn build(&self, app: &mut App) {
        let client = WsClient::connect(&self.url, &self.room)
            .expect("kyoso_sync: connect to server");
        app.insert_resource(WsBridge {
            client,
            last_acked: 0,
        });
        app.insert_resource(SyncStatus::AwaitingWelcome);
        app.init_resource::<RawPresence>();
        app.add_message::<SetLocalPresence>();
        app.add_message::<ClearLocalPresence>();
        app.add_message::<RawPresenceEvent>();
        app.add_message::<RemoteOpApplied>();
        app.init_resource::<crate::ClientSyncEngine>();
        app.init_resource::<crate::EntityCrdtIndex>();

        app.add_systems(
            Update,
            (
                inbound_system::<N, E>,
                detect_added_nodes::<N, E>,
                detect_added_edges::<N, E>,
                detect_tree_position_changes::<N, E>,
                detect_removed_nodes::<N, E>,
                detect_removed_edges::<N, E>,
                outbound_system::<N, E>,
                presence_outbound_system,
            )
                .chain(),
        );
    }
}

// ---------------------------------------------------------------------------
// Inbound — apply remote ops + project into ECS
// ---------------------------------------------------------------------------

fn inbound_system<N, E>(
    mut commands: Commands,
    mut engine: ResMut<crate::ClientSyncEngine>,
    mut index: ResMut<crate::EntityCrdtIndex>,
    bridge: Res<WsBridge>,
    mut status: ResMut<SyncStatus>,
    mut raw_presence: ResMut<RawPresence>,
    mut presence_events: MessageWriter<RawPresenceEvent>,
    mut remote_op_events: MessageWriter<RemoteOpApplied>,
    incoming: Query<&IncomingEdges>,
    tree_edges: Query<(), With<TreeEdge>>,
) where
    N: Syncable,
    E: Syncable,
{
    while let Some(event) = bridge.client.try_recv() {
        match event {
            Inbound::Welcome {
                peer,
                snapshot,
                diff,
                presence,
            } => {
                engine.set_peer(peer);
                if let Some(snap) = snapshot {
                    engine.restore(snap.clone());
                    project_snapshot::<N, E>(&mut commands, &mut index, &snap);
                }
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
                raw_presence.0.clear();
                raw_presence
                    .0
                    .extend(presence.iter().map(|(p, s)| (*p, s.clone())));
                presence_events.write(RawPresenceEvent::Snapshot(presence));
                *status = SyncStatus::Connected { peer };
            }
            Inbound::Apply(op) => {
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
            Inbound::Catchup(diff) => {
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
            Inbound::PresenceUpdate { peer, state } => {
                raw_presence.0.insert(peer, state.clone());
                presence_events.write(RawPresenceEvent::Updated { peer, state });
            }
            Inbound::PresenceLeft { peer } => {
                raw_presence.0.remove(&peer);
                presence_events.write(RawPresenceEvent::Left { peer });
            }
            Inbound::ServerError(msg) => {
                tracing::warn!(message = %msg, "server error");
            }
            Inbound::Disconnected => {
                *status = SyncStatus::Disconnected;
                raw_presence.0.clear();
            }
        }
    }
}

fn apply_one<N, E>(
    commands: &mut Commands,
    engine: &mut crate::ClientSyncEngine,
    index: &mut crate::EntityCrdtIndex,
    incoming: &Query<&IncomingEdges>,
    tree_edges: &Query<(), With<TreeEdge>>,
    op: &Op,
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
        // Cycle-rejected moves leave the backend unchanged. We still
        // checked apply_remote returned Ok, but the projection should
        // reflect what the backend actually did — read it back.
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

fn project_op<N, E>(
    commands: &mut Commands,
    index: &mut crate::EntityCrdtIndex,
    op: &Op,
) where
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
            // Insert the per-category marker if a `SyncedEdgeCategoryPlugin`
            // has registered one for this category. Defer via `commands`
            // to run on the next flush so the freshly-spawned entity is
            // available.
            if let Some(entity) = index.entity_of_edge.get(&op.id).copied() {
                let category = category.clone();
                commands.queue(crate::category::ApplyEdgeCategory {
                    entity,
                    category,
                });
            }
        }
        OpKind::SetNodeProperty { .. } | OpKind::SetRefEdgeProperty { .. } => {
            // Property ops are projected by per-schema typed plugins
            // (`SchemaSyncedNodeComponentPlugin` /
            // `SchemaSyncedEdgeComponentPlugin`), which subscribe to the
            // `RemoteOpApplied` event emitted by `inbound_system` after
            // this `apply_one` call. This system intentionally does not
            // touch the ECS for these op kinds.
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
            // Move ops need access to the world's TreeEdge query for
            // despawning the previous tree edge; that lives in
            // `project_move`, called separately from `apply_one`.
        }
    }
}

/// Project a remote `Move` op into the ECS:
/// - Despawn the existing TreeEdge entity whose `EdgeTo` is `target`.
/// - Insert (or refresh) the `TreeParent` + `OrderKey` components on
///   the target entity.
/// - If `new_parent` is `Some`, spawn a new TreeEdge entity from
///   parent to target.
fn project_move<N, E>(
    commands: &mut Commands,
    index: &crate::EntityCrdtIndex,
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

    // Despawn the previous tree edge (if any). The old edge entity is
    // not tracked by `entity_of_edge` because it was created either
    // by a previous Move projection or by tree.rs's apply_tree_commands
    // — both routes use the relationship machinery, not CrdtId entries.
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
    index: &mut crate::EntityCrdtIndex,
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
    index: &mut crate::EntityCrdtIndex,
    snap: &kyoso_graph_crdt::Snapshot,
) where
    N: Syncable,
    E: Syncable,
{
    // Spawn all live nodes first so edges below find their endpoints.
    for n in &snap.nodes {
        if !index.entity_of_node.contains_key(&n.id) {
            let entity = commands.spawn(N::default()).id();
            index.node_of_entity.insert(entity, n.id);
            index.entity_of_node.insert(n.id, entity);
            if let Some(key) = &n.order_key {
                commands.entity(entity).insert(OrderKey(key.clone()));
            }
        }
    }
    // Reconstruct tree-parent edges from the per-node `tree_parent`
    // annotation. Non-tree edges follow.
    for n in &snap.nodes {
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
    for e in &snap.edges {
        project_edge::<N, E>(commands, index, e.id, e.from, e.to);
    }
}

// ---------------------------------------------------------------------------
// Detection — local mutations → CRDT ops
// ---------------------------------------------------------------------------

pub(crate) fn detect_added_nodes<N, E>(
    mut engine: ResMut<crate::ClientSyncEngine>,
    mut index: ResMut<crate::EntityCrdtIndex>,
    added: Query<Entity, Added<N>>,
) where
    N: Syncable,
    E: Syncable,
{
    for entity in added.iter() {
        // Skip if the entity is already bound — that's the inbound
        // projector having just spawned it from a remote op.
        if index.node_id(entity).is_some() {
            continue;
        }
        let id = engine.add_node();
        index.bind_node(entity, id);
    }
}

/// Detect local tree-position changes (parent or order-key) and emit a
/// single atomic `Move` op per child. Watches `Changed<TreeParent>` *or*
/// `Changed<OrderKey>` so reorder-only and reparent-only flows both
/// route through the same code path. Echo prevention compares against
/// the backend's current state — re-applying a remote op produces no
/// new outbound op.
fn detect_tree_position_changes<N, E>(
    mut engine: ResMut<crate::ClientSyncEngine>,
    index: Res<crate::EntityCrdtIndex>,
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
        // Echo prevention: backend already reflects this state.
        if engine.tree_parent(child_id) == new_parent_id
            && engine.node_order_key(child_id) == Some(key.0.as_str())
        {
            continue;
        }
        engine.move_node(child_id, new_parent_id, key.0.clone());
    }
}

pub(crate) fn detect_added_edges<N, E>(
    mut engine: ResMut<crate::ClientSyncEngine>,
    mut index: ResMut<crate::EntityCrdtIndex>,
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

/// Despawn-detection: when an entity with `N` is removed (despawn or
/// component-remove), emit the matching `RemoveNode` op — but only if
/// the entity is still tracked. Inbound's `project_op` already cleans
/// the index when it processes a remote `RemoveNode`, so re-entries
/// here just no-op.
fn detect_removed_nodes<N, E>(
    mut engine: ResMut<crate::ClientSyncEngine>,
    mut index: ResMut<crate::EntityCrdtIndex>,
    mut removed: RemovedComponents<N>,
) where
    N: Syncable,
    E: Syncable,
{
    for entity in removed.read() {
        let Some(node_id) = index.node_of_entity.remove(&entity) else {
            continue; // already cleaned by inbound apply
        };
        index.entity_of_node.remove(&node_id);
        engine.remove_node(node_id);
    }
}

fn detect_removed_edges<N, E>(
    mut engine: ResMut<crate::ClientSyncEngine>,
    mut index: ResMut<crate::EntityCrdtIndex>,
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
    mut engine: ResMut<crate::ClientSyncEngine>,
    mut bridge: ResMut<WsBridge>,
    status: Res<SyncStatus>,
) where
    N: Syncable,
    E: Syncable,
{
    if !status.is_connected() {
        return;
    }
    let pending = engine.drain_pending();
    for op in pending {
        if !bridge.client.send_op(op) {
            return;
        }
    }
    let applied = engine.applied_seq();
    if applied > bridge.last_acked && bridge.client.send_ack(applied) {
        bridge.last_acked = applied;
    }
}

/// Drain `SetLocalPresence` / `ClearLocalPresence` messages and forward
/// to the wire. Each event is one frame — coalesce upstream if you want
/// to throttle (cursor-move spam, e.g.).
fn presence_outbound_system(
    bridge: Res<WsBridge>,
    status: Res<SyncStatus>,
    mut sets: MessageReader<SetLocalPresence>,
    mut clears: MessageReader<ClearLocalPresence>,
) {
    if !status.is_connected() {
        // Drop pending presence updates — they're valid only against a
        // live connection. The next reconnect's Welcome will give us a
        // fresh map and the consumer can re-emit.
        sets.clear();
        clears.clear();
        return;
    }
    for SetLocalPresence(state) in sets.read() {
        let _ = bridge.client.send_presence(state.clone());
    }
    for _ in clears.read() {
        let _ = bridge.client.leave_presence();
    }
}
