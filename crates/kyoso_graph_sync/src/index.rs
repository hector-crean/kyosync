//! Bidirectional `Entity ↔ CrdtId` index for the graph model.
//!
//! Owned by `kyoso_graph_sync` because both id-spaces (Bevy `Entity`
//! and CRDT `CrdtId`) live where the graph projection happens. Other
//! CRDT models (comments, presence) keep their own analogous indices.

use bevy::prelude::*;
use kyoso_crdt::CrdtId;
use kyoso_graph::traversal::NodeIdResolver;
use std::collections::HashMap;

/// Bidirectional mapping between Bevy `Entity`s and the [`CrdtId`]s
/// they're synced under.
///
/// Detection systems consult this index to find the `CrdtId` for an
/// entity (when generating ops); the inbound projector consults it to
/// find (or create) the entity for an incoming op's target.
#[derive(Resource, Default, Debug, Clone)]
pub struct EntityCrdtIndex {
    pub node_of_entity: HashMap<Entity, CrdtId>,
    pub entity_of_node: HashMap<CrdtId, Entity>,
    pub edge_of_entity: HashMap<Entity, CrdtId>,
    pub entity_of_edge: HashMap<CrdtId, Entity>,
}

impl EntityCrdtIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a Bevy entity to a node-level `CrdtId`. Idempotent if the
    /// pair is already bound.
    pub fn bind_node(&mut self, entity: Entity, id: CrdtId) {
        self.node_of_entity.insert(entity, id);
        self.entity_of_node.insert(id, entity);
    }

    /// Same for edges.
    pub fn bind_edge(&mut self, entity: Entity, id: CrdtId) {
        self.edge_of_entity.insert(entity, id);
        self.entity_of_edge.insert(id, entity);
    }

    /// Drop the node binding for `entity`, returning the `CrdtId` that
    /// was bound (if any).
    pub fn unbind_node(&mut self, entity: Entity) -> Option<CrdtId> {
        let id = self.node_of_entity.remove(&entity)?;
        self.entity_of_node.remove(&id);
        Some(id)
    }

    pub fn unbind_edge(&mut self, entity: Entity) -> Option<CrdtId> {
        let id = self.edge_of_entity.remove(&entity)?;
        self.entity_of_edge.remove(&id);
        Some(id)
    }

    #[must_use]
    pub fn node_id(&self, entity: Entity) -> Option<CrdtId> {
        self.node_of_entity.get(&entity).copied()
    }

    #[must_use]
    pub fn entity_for_node(&self, id: CrdtId) -> Option<Entity> {
        self.entity_of_node.get(&id).copied()
    }

    #[must_use]
    pub fn edge_id(&self, entity: Entity) -> Option<CrdtId> {
        self.edge_of_entity.get(&entity).copied()
    }

    #[must_use]
    pub fn entity_for_edge(&self, id: CrdtId) -> Option<Entity> {
        self.entity_of_edge.get(&id).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.node_of_entity.is_empty() && self.edge_of_entity.is_empty()
    }
}

/// Resolve `Entity → CrdtId` for nodes only. Edges have their own
/// id space and aren't covered here — the agent-facing traversal walks
/// nodes, not edges.
///
/// Plugged into `kyoso_graph::traversal::WorldGraphView::traverse_with`
/// to surface entities as [`kyoso_graph::traversal::NodeRef::Replicated`]
/// when they're bound in the index, or `Local` otherwise.
impl NodeIdResolver for EntityCrdtIndex {
    type Id = CrdtId;

    fn resolve(&self, entity: Entity) -> Option<CrdtId> {
        self.node_of_entity.get(&entity).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_and_lookup_node() {
        let mut idx = EntityCrdtIndex::new();
        let entity = Entity::from_bits(42);
        let id = CrdtId::new(1, 0);
        idx.bind_node(entity, id);
        assert_eq!(idx.node_id(entity), Some(id));
        assert_eq!(idx.entity_for_node(id), Some(entity));
    }

    #[test]
    fn unbind_removes_both_directions() {
        let mut idx = EntityCrdtIndex::new();
        let entity = Entity::from_bits(42);
        let id = CrdtId::new(1, 0);
        idx.bind_node(entity, id);
        let removed = idx.unbind_node(entity);
        assert_eq!(removed, Some(id));
        assert!(idx.node_id(entity).is_none());
        assert!(idx.entity_for_node(id).is_none());
    }

    #[test]
    fn edges_and_nodes_are_independent() {
        let mut idx = EntityCrdtIndex::new();
        let n_entity = Entity::from_bits(1);
        let e_entity = Entity::from_bits(2);
        let n_id = CrdtId::new(1, 0);
        let e_id = CrdtId::new(1, 1);
        idx.bind_node(n_entity, n_id);
        idx.bind_edge(e_entity, e_id);
        assert_eq!(idx.node_id(n_entity), Some(n_id));
        assert_eq!(idx.edge_id(e_entity), Some(e_id));
        assert!(idx.node_id(e_entity).is_none());
        assert!(idx.edge_id(n_entity).is_none());
    }
}
