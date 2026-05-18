//! Structural state as synced components.
//!
//! The graph's *structure* (which nodes/edges exist, which endpoints
//! each edge connects) is replicated as two normal `SchemaSync`
//! components on each entity:
//!
//! - [`NodePresence`] — a single `alive: bool` per node entity.
//!   Defaults to `false`; an [`assign_local_node_ids`] system flips it
//!   to `true` on local spawn (which the standard typed-schema diff
//!   pipeline ships as a [`kyoso_crdt::WireDelta::LwwReplace`]).
//!   Tombstoning is just `alive = false`.
//! - [`EdgeEndpoints`] — `from` / `to` as `Option<CrdtId>` plus an
//!   `alive: bool`. Endpoint ids are written once at local spawn (read
//!   off the entity's [`EdgeFrom`] / [`EdgeTo`] Bevy relationships and
//!   translated through [`EntityCrdtIndex`]) and never mutated; the
//!   Bevy relationships themselves are the ECS face for traversal.
//!
//! Cycle / dangle / cascade enforcement lives at the ECS command
//! layer — `kyoso_graph::queries::GraphQuery` already provides the
//! pre-submit cycle check. The sync layer trusts the client.

use bevy::prelude::*;
use kyoso_crdt::{CrdtId, Op, Path, PathSegment};
use kyoso_graph::components::{EdgeFrom, EdgeTo, IncomingEdges, OutgoingEdges};
use kyoso_graph_crdt::OpKind;
use kyoso_sync::{PeerIdGen, SchemaSync};

use crate::engine::ClientSyncEngine;
use crate::index::EntityCrdtIndex;

/// Schema name for `NodePresence` — chosen here (not on the component
/// derive) because the slim plugin's snapshot handler needs to match
/// against this string when classifying snapshot entries as node vs
/// edge. Must stay in sync with the `#[schema(name = ...)]` below.
pub(crate) const NODE_PRESENCE_SCHEMA: &str = "NodePresence";

/// Schema name for `EdgeEndpoints`. See [`NODE_PRESENCE_SCHEMA`].
pub(crate) const EDGE_ENDPOINTS_SCHEMA: &str = "EdgeEndpoints";

/// Per-node existence marker.
///
/// `alive == true` means "this node exists on the wire"; `alive ==
/// false` is a tombstone. The flip happens locally in two places:
/// - On local spawn: [`assign_local_node_ids`] sets `alive = true`
///   after binding the entity to a freshly-minted `CrdtId`.
/// - On local user despawn:
///   [`detect_local_node_despawn`] queues a `Set(false)` op and
///   unbinds.
#[derive(Component, Default, Debug, Clone, PartialEq, SchemaSync)]
#[schema(name = "NodePresence")]
pub struct NodePresence {
    pub alive: bool,
}

/// Per-edge endpoint pair + existence marker.
///
/// `from` / `to` are the endpoint nodes' `CrdtId`s; the local
/// [`EdgeFrom`] / [`EdgeTo`] Bevy relationships are derived from
/// these by [`resolve_pending_edges`] on inbound apply. They are set
/// once at local spawn and never mutated.
#[derive(Component, Default, Debug, Clone, PartialEq, SchemaSync)]
#[schema(name = "EdgeEndpoints")]
pub struct EdgeEndpoints {
    pub from: Option<CrdtId>,
    pub to: Option<CrdtId>,
    pub alive: bool,
}

/// Marker on edge entities created by inbound apply whose Bevy
/// relationships haven't yet been attached — at least one endpoint
/// `CrdtId` hasn't resolved to a local [`Entity`] in
/// [`EntityCrdtIndex`] yet. Removed by [`resolve_pending_edges`] once
/// both endpoints resolve.
#[derive(Component, Debug, Default)]
pub struct EdgePending;

// ---------------------------------------------------------------------------
// Local-spawn id assignment
// ---------------------------------------------------------------------------

/// On `Added<NodePresence>` for entities not yet in the index: mint a
/// `CrdtId`, bind it, and flip `alive = true` in place so the
/// standard component-sync diff pipeline emits the corresponding wire
/// op the same frame.
pub(crate) fn assign_local_node_ids(
    peer_ids: Res<PeerIdGen>,
    mut index: ResMut<EntityCrdtIndex>,
    mut added: Query<(Entity, &mut NodePresence), Added<NodePresence>>,
) {
    for (entity, mut presence) in &mut added {
        if index.node_id(entity).is_some() {
            continue;
        }
        let id = peer_ids.handle().next();
        index.bind_node(entity, id);
        eprintln!(
            "[assign_local_node_ids] entity={entity:?} id={id:?} alive_before={}",
            presence.alive
        );
        if !presence.alive {
            presence.alive = true;
        }
    }
}

/// On `Added<EdgeEndpoints>` for entities not yet in the index: mint a
/// `CrdtId`, bind it, and populate `EdgeEndpoints { from, to }` from
/// the local `EdgeFrom`/`EdgeTo` Bevy relationships (translated to
/// `CrdtId`s via the node side of the index).
pub(crate) fn assign_local_edge_ids(
    peer_ids: Res<PeerIdGen>,
    mut index: ResMut<EntityCrdtIndex>,
    mut added: Query<
        (Entity, &EdgeFrom, &EdgeTo, &mut EdgeEndpoints),
        Added<EdgeEndpoints>,
    >,
) {
    for (entity, from, to, mut endpoints) in &mut added {
        if index.edge_id(entity).is_some() {
            continue;
        }
        let (Some(from_id), Some(to_id)) = (index.node_id(from.0), index.node_id(to.0)) else {
            tracing::warn!(
                ?entity,
                "edge spawned before its endpoint nodes were bound; deferring"
            );
            continue;
        };
        let id = peer_ids.handle().next();
        index.bind_edge(entity, id);
        endpoints.from = Some(from_id);
        endpoints.to = Some(to_id);
        endpoints.alive = true;
    }
}

// ---------------------------------------------------------------------------
// Local-despawn detection
// ---------------------------------------------------------------------------

/// Detect locally-despawned node entities (the `RemovedComponents`
/// stream catches all sources, including user `commands.entity(e)
/// .despawn()`). For entities still bound in the index, queue a
/// `Set(false)` op against `NodePresence.alive` and unbind.
///
/// Inbound-driven tombstones are filtered out because
/// [`despawn_tombstoned_nodes`] unbinds *before* it issues the local
/// despawn — by the time `RemovedComponents` fires, the entity is no
/// longer in the index and this system skips it.
pub(crate) fn detect_local_node_despawn(
    mut removed: RemovedComponents<NodePresence>,
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
) {
    for entity in removed.read() {
        let Some(id) = index.unbind_node(entity) else {
            continue;
        };
        let op = build_tombstone_op(&mut engine, id, true);
        engine.enqueue(op);
    }
}

/// Same as [`detect_local_node_despawn`] but for edge entities.
pub(crate) fn detect_local_edge_despawn(
    mut removed: RemovedComponents<EdgeEndpoints>,
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
) {
    for entity in removed.read() {
        let Some(id) = index.unbind_edge(entity) else {
            continue;
        };
        let op = build_tombstone_op(&mut engine, id, false);
        engine.enqueue(op);
    }
}

/// Build a `SetNodeProperty` / `SetRefEdgeProperty` op flipping the
/// `alive` field on either `NodePresence` or `EdgeEndpoints` to
/// `false`. Used by the despawn-detection systems where no
/// `Changed<C>` event will ever fire (the component is gone) so the
/// standard diff pipeline can't see the tombstone.
fn build_tombstone_op(engine: &mut ClientSyncEngine, target: CrdtId, is_node: bool) -> Op<OpKind> {
    let schema = if is_node {
        NODE_PRESENCE_SCHEMA
    } else {
        EDGE_ENDPOINTS_SCHEMA
    };
    let mut path = Path::field(schema);
    path.0.push(PathSegment::Field("alive".to_string()));
    let delta = kyoso_crdt::WireDelta::LwwReplace {
        value: postcard::to_allocvec(&false).expect("bool serializes"),
    };
    let op_id = engine.ids().next();
    let kind = if is_node {
        OpKind::SetNodeProperty { target, path, delta }
    } else {
        OpKind::SetRefEdgeProperty { target, path, delta }
    };
    Op::new(op_id, kind)
}

// ---------------------------------------------------------------------------
// Remote-driven tombstone projection
// ---------------------------------------------------------------------------

/// When `NodePresence.alive` flips to `false` on an entity that's
/// still bound in the index (i.e. a remote tombstone landed via the
/// standard `project_to_components` path), cascade-despawn incident
/// edges, unbind from the index, then despawn the node entity.
///
/// Unbinding *before* despawning is the discriminator that prevents
/// [`detect_local_node_despawn`] from emitting a redundant tombstone
/// op on the resulting `RemovedComponents<NodePresence>`.
pub(crate) fn despawn_tombstoned_nodes(
    mut commands: Commands,
    mut index: ResMut<EntityCrdtIndex>,
    nodes: Query<
        (
            Entity,
            &NodePresence,
            Option<&OutgoingEdges>,
            Option<&IncomingEdges>,
        ),
        Changed<NodePresence>,
    >,
) {
    for (entity, presence, outgoing, incoming) in &nodes {
        if presence.alive {
            continue;
        }
        if index.node_id(entity).is_none() {
            continue;
        }
        if let Some(out) = outgoing {
            for edge in out.iter() {
                commands.entity(edge).despawn();
            }
        }
        if let Some(inc) = incoming {
            for edge in inc.iter() {
                commands.entity(edge).despawn();
            }
        }
        index.unbind_node(entity);
        commands.entity(entity).despawn();
    }
}

/// Same as [`despawn_tombstoned_nodes`] but for edges. No cascade —
/// edges have no incident structure of their own.
pub(crate) fn despawn_tombstoned_edges(
    mut commands: Commands,
    mut index: ResMut<EntityCrdtIndex>,
    edges: Query<(Entity, &EdgeEndpoints), Changed<EdgeEndpoints>>,
) {
    for (entity, endpoints) in &edges {
        if endpoints.alive {
            continue;
        }
        if index.edge_id(entity).is_none() {
            continue;
        }
        index.unbind_edge(entity);
        commands.entity(entity).despawn();
    }
}

// ---------------------------------------------------------------------------
// Edge resolution: parked → attached
// ---------------------------------------------------------------------------

/// For each `EdgePending` entity, if `EdgeEndpoints.{from, to}` both
/// resolve to a node `Entity` via the index, attach `EdgeFrom` /
/// `EdgeTo` Bevy relationships and remove the marker.
pub(crate) fn resolve_pending_edges(
    mut commands: Commands,
    index: Res<EntityCrdtIndex>,
    pending: Query<(Entity, &EdgeEndpoints), With<EdgePending>>,
) {
    for (entity, endpoints) in &pending {
        if !endpoints.alive {
            continue;
        }
        let (Some(from_id), Some(to_id)) = (endpoints.from, endpoints.to) else {
            continue;
        };
        let (Some(from_e), Some(to_e)) = (
            index.entity_for_node(from_id),
            index.entity_for_node(to_id),
        ) else {
            continue;
        };
        commands
            .entity(entity)
            .insert((EdgeFrom(from_e), EdgeTo(to_e)))
            .remove::<EdgePending>();
    }
}

// ---------------------------------------------------------------------------
// Inbound classification & placeholder spawning
// ---------------------------------------------------------------------------

/// Examine the head segment of an inbound property op's `Path` and,
/// for `NodePresence` / `EdgeEndpoints` payloads, ensure the local
/// entity exists. Returns `true` if the op was a structural property
/// op (caller still emits `RemoteOpApplied` for downstream
/// typed-schema processing).
///
/// Other property paths (e.g. `Transform`) leave the index untouched —
/// the entity must already exist via a prior `NodePresence` /
/// `EdgeEndpoints` op.
pub(crate) fn ensure_inbound_entity(
    commands: &mut Commands,
    index: &mut EntityCrdtIndex,
    op: &Op<OpKind>,
) {
    match &op.kind {
        OpKind::SetNodeProperty { target, path, .. } => {
            let head_match = head_field_is(path, NODE_PRESENCE_SCHEMA);
            let already = index.entity_of_node.contains_key(target);
            eprintln!(
                "[ensure_inbound_entity NodeProperty] target={:?} head_match={} already={}",
                target, head_match, already
            );
            if head_match && !already {
                let entity = commands.spawn(NodePresence::default()).id();
                index.bind_node(entity, *target);
                eprintln!(
                    "[ensure_inbound_entity] bound node entity={:?} id={:?} index_ptr={:p} map_ptr={:p} len={}",
                    entity,
                    target,
                    &*index,
                    &index.node_of_entity,
                    index.node_of_entity.len()
                );
            }
        }
        OpKind::SetRefEdgeProperty { target, path, .. } => {
            if head_field_is(path, EDGE_ENDPOINTS_SCHEMA)
                && !index.entity_of_edge.contains_key(target)
            {
                let entity = commands
                    .spawn((EdgeEndpoints::default(), EdgePending))
                    .id();
                index.bind_edge(entity, *target);
            }
        }
        _ => {}
    }
}

fn head_field_is(path: &Path, name: &str) -> bool {
    matches!(path.0.first(), Some(PathSegment::Field(f)) if f == name)
}

/// Hydrate placeholder entities from a server snapshot. For every
/// `(target, OpaqueRecord)` pair in the snapshot's schema table,
/// inspect the record's field paths and spawn a node or edge
/// placeholder accordingly. Per-schema state is installed separately
/// by the standard `kyoso_sync::SchemaHydrators` flow.
///
/// Pre-existing entries are skipped — re-running on reconnect is
/// safe.
pub(crate) fn hydrate_snapshot_placeholders(
    commands: &mut Commands,
    index: &mut EntityCrdtIndex,
    schemas: &std::collections::BTreeMap<CrdtId, kyoso_crdt::OpaqueRecord>,
) {
    for (id, record) in schemas {
        let kind = classify_record(record);
        match kind {
            Some(SchemaKind::Node) => {
                if !index.entity_of_node.contains_key(id) {
                    let entity = commands.spawn(NodePresence::default()).id();
                    index.bind_node(entity, *id);
                }
            }
            Some(SchemaKind::Edge) => {
                if !index.entity_of_edge.contains_key(id) {
                    let entity = commands
                        .spawn((EdgeEndpoints::default(), EdgePending))
                        .id();
                    index.bind_edge(entity, *id);
                }
            }
            None => {
                // Record has no NodePresence / EdgeEndpoints field —
                // either pre-refactor data or a property-only schema
                // we don't manage. Nothing to spawn.
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SchemaKind {
    Node,
    Edge,
}

fn classify_record(record: &kyoso_crdt::OpaqueRecord) -> Option<SchemaKind> {
    for path in record.fields.keys() {
        match path.0.first() {
            Some(PathSegment::Field(f)) if f == NODE_PRESENCE_SCHEMA => {
                return Some(SchemaKind::Node);
            }
            Some(PathSegment::Field(f)) if f == EDGE_ENDPOINTS_SCHEMA => {
                return Some(SchemaKind::Edge);
            }
            _ => {}
        }
    }
    None
}

