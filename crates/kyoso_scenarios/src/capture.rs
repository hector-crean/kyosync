//! Snapshot a single peer's view of the circuit world.
//!
//! Captures the things a divergence diff would need to see:
//! - Per-node: kind, layer, transform, kind-specific property value
//!   (resistance / capacitance / inductance / voltage / ground label).
//! - Per-edge: endpoints and kind.
//! - Engine state: `applied_seq`, peer id.
//!
//! Everything is keyed by `CrdtId` (peer-stable) — not Bevy `Entity`
//! ids, which are per-peer and would never compare equal.

use std::collections::BTreeMap;

use bevy::prelude::*;
use kyoso_circuit::{
    Capacitor, CircuitEdge, CircuitEdgeKind, CircuitLayer, CircuitNode, ComponentKind,
    DifferentialPairMarker, Ground, Inductor, OnLayer, Resistor, SameNetMarker, VoltageSource,
    WireMarker,
};
use kyoso_crdt::CrdtId;
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph_sync::{ClientSyncEngine, EntityCrdtIndex};
use serde::{Deserialize, Serialize};

/// One peer's full observable state at a checkpoint moment. Used by
/// the diff machinery — all maps are `BTreeMap` so iteration order is
/// stable for deterministic reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerState {
    /// Stable label used to refer to this peer in diffs.
    pub label: String,
    /// Engine-level book-keeping.
    pub applied_seq: u64,
    pub peer_id: u32,
    /// `CrdtId.to_string()` → per-node properties.
    pub nodes: BTreeMap<String, NodeState>,
    /// `CrdtId.to_string()` → per-edge properties.
    pub edges: BTreeMap<String, EdgeState>,
}

/// What we capture per node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeState {
    pub kind: Option<String>,
    pub layer: Option<String>,
    pub transform: Pos3,
    /// Kind-specific scalar — populated for whichever per-kind schema
    /// the entity carries. `None` if no kind component has projected
    /// yet (race window during late-join hydration).
    pub property: Option<f64>,
    /// Ground label, only present for `Ground` nodes.
    pub label_str: Option<String>,
}

/// Capture-time transform — Pos3 instead of Bevy's `Vec3` so the
/// report is fully self-describing for downstream JSON consumers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pos3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl From<Vec3> for Pos3 {
    fn from(v: Vec3) -> Self {
        Self { x: v.x, y: v.y, z: v.z }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeState {
    pub from: String,
    pub to: String,
    pub kind: Option<String>,
}

/// Walk every `CircuitNode` and `CircuitEdge` in `app`'s world, look
/// up each entity's `CrdtId` via `EntityCrdtIndex`, and build a stable
/// snapshot indexed by id. Mutates the world's `Query` cache (Bevy
/// requires `&mut World` for `query::<...>().iter(world)`) but doesn't
/// alter any component values.
pub fn capture_peer_state(app: &mut App, label: impl Into<String>) -> PeerState {
    let world = app.world_mut();
    let engine = world.resource::<ClientSyncEngine>();
    let applied_seq = engine.applied_seq();
    let peer_id = engine.peer();
    // Clone the index out so we don't hold a borrow while running
    // queries (the index lives as a `Res<...>` and would conflict
    // with the `query::<...>().iter()` borrows below otherwise).
    let index: EntityCrdtIndex = world.resource::<EntityCrdtIndex>().clone();

    // Node capture: per-entity property fetch via separate queries
    // per kind so we don't accidentally lean on a `World::get` that
    // doesn't exist for typed schemas.
    let mut nodes: BTreeMap<String, NodeState> = BTreeMap::new();
    let mut entity_kinds: BTreeMap<Entity, ComponentKind> = BTreeMap::new();

    {
        let mut q = world.query::<(Entity, &Transform, Option<&OnLayer>)>();
        let node_filter: bevy::ecs::query::QueryState<Entity, With<CircuitNode>> =
            world.query_filtered::<Entity, With<CircuitNode>>();
        let circuit_entities: std::collections::HashSet<Entity> =
            node_filter.iter_manual(world).collect();
        for (entity, transform, on_layer) in q.iter(world) {
            if !circuit_entities.contains(&entity) {
                continue;
            }
            let Some(&id) = index.node_of_entity.get(&entity) else {
                continue;
            };
            let layer = on_layer
                .and_then(|l| l.layer())
                .map(layer_label);
            nodes.insert(
                crdt_id_key(&id),
                NodeState {
                    kind: None,
                    layer,
                    transform: transform.translation.into(),
                    property: None,
                    label_str: None,
                },
            );
        }
    }

    // Per-kind passes. Each annotates the existing entry with kind +
    // property; if a node is in the index but no kind schema has
    // projected onto it yet (race window), the `kind` stays `None`.
    capture_kind::<Resistor, _>(
        world,
        &index,
        &mut nodes,
        &mut entity_kinds,
        ComponentKind::Resistor,
        |r: &Resistor| r.resistance_ohms as f64,
    );
    capture_kind::<Capacitor, _>(
        world,
        &index,
        &mut nodes,
        &mut entity_kinds,
        ComponentKind::Capacitor,
        |c: &Capacitor| c.capacitance_farads as f64,
    );
    capture_kind::<Inductor, _>(
        world,
        &index,
        &mut nodes,
        &mut entity_kinds,
        ComponentKind::Inductor,
        |i: &Inductor| i.inductance_henries as f64,
    );
    capture_kind::<VoltageSource, _>(
        world,
        &index,
        &mut nodes,
        &mut entity_kinds,
        ComponentKind::VoltageSource,
        |v: &VoltageSource| v.voltage_volts as f64,
    );
    // Ground stores a string label, not a scalar.
    {
        let mut q = world.query::<(Entity, &Ground)>();
        for (entity, g) in q.iter(world) {
            let Some(&id) = index.node_of_entity.get(&entity) else {
                continue;
            };
            let Some(node) = nodes.get_mut(&crdt_id_key(&id)) else {
                continue;
            };
            node.kind = Some(format!("{:?}", ComponentKind::Ground));
            node.label_str = Some(g.label.clone());
            entity_kinds.insert(entity, ComponentKind::Ground);
        }
    }

    // Edge capture.
    let mut edges: BTreeMap<String, EdgeState> = BTreeMap::new();
    {
        let mut q = world.query::<(Entity, &EdgeFrom, &EdgeTo)>();
        let edge_filter = world.query_filtered::<Entity, With<CircuitEdge>>();
        let edge_entities: std::collections::HashSet<Entity> =
            edge_filter.iter_manual(world).collect();
        let wires = world.query_filtered::<Entity, With<WireMarker>>();
        let same_net = world.query_filtered::<Entity, With<SameNetMarker>>();
        let diff_pair = world.query_filtered::<Entity, With<DifferentialPairMarker>>();
        let wire_set: std::collections::HashSet<Entity> = wires.iter_manual(world).collect();
        let same_net_set: std::collections::HashSet<Entity> =
            same_net.iter_manual(world).collect();
        let diff_pair_set: std::collections::HashSet<Entity> =
            diff_pair.iter_manual(world).collect();
        for (entity, from, to) in q.iter(world) {
            if !edge_entities.contains(&entity) {
                continue;
            }
            let Some(&edge_id) = index.edge_of_entity.get(&entity) else {
                continue;
            };
            let Some(&from_id) = index.node_of_entity.get(&from.0) else {
                continue;
            };
            let Some(&to_id) = index.node_of_entity.get(&to.0) else {
                continue;
            };
            let kind = if wire_set.contains(&entity) {
                Some(edge_kind_label(CircuitEdgeKind::Wire))
            } else if same_net_set.contains(&entity) {
                Some(edge_kind_label(CircuitEdgeKind::SameNet))
            } else if diff_pair_set.contains(&entity) {
                Some(edge_kind_label(CircuitEdgeKind::DifferentialPair))
            } else {
                None
            };
            edges.insert(
                crdt_id_key(&edge_id),
                EdgeState {
                    from: crdt_id_key(&from_id),
                    to: crdt_id_key(&to_id),
                    kind,
                },
            );
        }
    }

    PeerState {
        label: label.into(),
        applied_seq,
        peer_id,
        nodes,
        edges,
    }
}

fn capture_kind<C, F>(
    world: &mut World,
    index: &EntityCrdtIndex,
    nodes: &mut BTreeMap<String, NodeState>,
    entity_kinds: &mut BTreeMap<Entity, ComponentKind>,
    kind: ComponentKind,
    extract: F,
) where
    C: Component,
    F: Fn(&C) -> f64,
{
    let mut q = world.query::<(Entity, &C)>();
    for (entity, c) in q.iter(world) {
        let Some(&id) = index.node_of_entity.get(&entity) else {
            continue;
        };
        let Some(node) = nodes.get_mut(&crdt_id_key(&id)) else {
            continue;
        };
        node.kind = Some(format!("{kind:?}"));
        node.property = Some(extract(c));
        entity_kinds.insert(entity, kind);
    }
}

fn layer_label(layer: CircuitLayer) -> String {
    match layer {
        CircuitLayer::Signal => "Signal".to_string(),
        CircuitLayer::Power => "Power".to_string(),
        CircuitLayer::Ground => "Ground".to_string(),
        CircuitLayer::Mechanical => "Mechanical".to_string(),
    }
}

fn edge_kind_label(kind: CircuitEdgeKind) -> String {
    match kind {
        CircuitEdgeKind::Wire => "Wire".to_string(),
        CircuitEdgeKind::SameNet => "SameNet".to_string(),
        CircuitEdgeKind::DifferentialPair => "DifferentialPair".to_string(),
    }
}

/// Stable per-id string key used everywhere in the report so JSON
/// keys compare across peers regardless of internal `CrdtId`
/// formatting choices. Format: `{peer}:{seq}`.
pub fn crdt_id_key(id: &CrdtId) -> String {
    format!("{}:{}", id.peer, id.seq)
}
