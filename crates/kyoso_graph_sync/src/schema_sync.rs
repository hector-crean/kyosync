//! Graph [`SchemaTarget`] providers.
//!
//! Binds `kyoso_sync`'s model-agnostic component-sync pipeline
//! ([`kyoso_sync::SchemaSyncedComponentPlugin`]) to the graph model:
//! [`NodeTarget`] attaches typed schemas to graph nodes, [`EdgeTarget`]
//! to reference edges.
//!
//! Each provider names the graph-specific resources — [`EntityCrdtIndex`]
//! for identity, [`ClientSyncEngine`] for the outbound queue,
//! [`RemoteOpApplied`] for the inbound stream — and bridges them to the
//! universal `(CrdtId, Path, WireDelta)` property-op shape via the graph
//! [`OpKind`]. The pipeline itself lives in `kyoso_sync` and knows
//! nothing about graphs; everything graph-aware is confined to this file.

use std::collections::HashMap;

use bevy::prelude::Entity;
use kyoso_crdt::{CrdtId, Op, Path, WireDelta};
use kyoso_graph_crdt::OpKind;
use kyoso_sync::{InboundProperty, SchemaTarget, TargetKind};

use crate::engine::ClientSyncEngine;
use crate::index::EntityCrdtIndex;
use crate::plugin::RemoteOpApplied;

/// Provider attaching typed schemas to graph **nodes**.
pub struct NodeTarget;

impl SchemaTarget for NodeTarget {
    const KIND: TargetKind = TargetKind::Node;
    type Index = EntityCrdtIndex;
    type Outbound = ClientSyncEngine;
    type Inbound = RemoteOpApplied;

    fn id_for(index: &EntityCrdtIndex, entity: Entity) -> Option<CrdtId> {
        index.node_id(entity)
    }

    fn pairs(index: &EntityCrdtIndex) -> &HashMap<Entity, CrdtId> {
        &index.node_of_entity
    }

    fn mint_id(engine: &mut ClientSyncEngine) -> CrdtId {
        engine.mint_id()
    }

    fn enqueue(
        engine: &mut ClientSyncEngine,
        op_id: CrdtId,
        target: CrdtId,
        path: Path,
        delta: WireDelta,
    ) {
        engine.enqueue(Op::new(
            op_id,
            OpKind::SetNodeProperty { target, path, delta },
        ));
    }

    fn match_inbound(event: &RemoteOpApplied) -> Option<InboundProperty<'_>> {
        let op = &event.0;
        if let OpKind::SetNodeProperty { target, path, delta } = &op.kind {
            Some(InboundProperty {
                op_id: op.id,
                seq: op.seq,
                target: *target,
                path,
                delta,
            })
        } else {
            None
        }
    }
}

/// Provider attaching typed schemas to graph **reference edges**.
pub struct EdgeTarget;

impl SchemaTarget for EdgeTarget {
    const KIND: TargetKind = TargetKind::Edge;
    type Index = EntityCrdtIndex;
    type Outbound = ClientSyncEngine;
    type Inbound = RemoteOpApplied;

    fn id_for(index: &EntityCrdtIndex, entity: Entity) -> Option<CrdtId> {
        index.edge_id(entity)
    }

    fn pairs(index: &EntityCrdtIndex) -> &HashMap<Entity, CrdtId> {
        &index.edge_of_entity
    }

    fn mint_id(engine: &mut ClientSyncEngine) -> CrdtId {
        engine.mint_id()
    }

    fn enqueue(
        engine: &mut ClientSyncEngine,
        op_id: CrdtId,
        target: CrdtId,
        path: Path,
        delta: WireDelta,
    ) {
        engine.enqueue(Op::new(
            op_id,
            OpKind::SetRefEdgeProperty { target, path, delta },
        ));
    }

    fn match_inbound(event: &RemoteOpApplied) -> Option<InboundProperty<'_>> {
        let op = &event.0;
        if let OpKind::SetRefEdgeProperty { target, path, delta } = &op.kind {
            Some(InboundProperty {
                op_id: op.id,
                seq: op.seq,
                target: *target,
                path,
                delta,
            })
        } else {
            None
        }
    }
}
