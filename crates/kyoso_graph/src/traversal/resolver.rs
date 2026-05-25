//! Entity → stable id resolution, abstracted via [`NodeIdResolver`].
//!
//! `kyoso_graph` doesn't depend on any sync/CRDT layer, so the
//! "what is the durable id of this entity?" question is delegated to
//! whichever upstream crate manages durable ids. The sync layer
//! (`kyoso_graph_sync::EntityCrdtIndex`) provides an impl yielding
//! `CrdtId`; tests can provide trivial impls; future stores (URL ids,
//! UUIDs, etc.) plug in identically.

use bevy::ecs::entity::Entity;

/// Abstracts "look up the durable id of an entity, if it has one".
///
/// Implementations live alongside whatever store owns the durable-id
/// mapping (the sync layer, an asset store, a session table, …). The
/// traversal layer calls this through trait dispatch so nothing inside
/// `kyoso_graph` has to know the concrete id type.
///
/// `'static` so trait objects / type parameters compose cleanly with
/// Bevy's `Resource` machinery; pair with `+ bevy::ecs::system::Resource`
/// at the call site to allow lookup via `World::get_resource::<R>()`.
pub trait NodeIdResolver: 'static {
    /// The durable id type yielded by this resolver. Typically `Copy`
    /// + `Eq` so [`NodeRef`] can be cheaply held and compared.
    type Id: Copy + Eq + core::fmt::Debug + 'static;

    /// Return the durable id of `entity`, or `None` if no such id is
    /// known (e.g. the entity is a session-local overlay).
    fn resolve(&self, entity: Entity) -> Option<Self::Id>;
}

/// Stable identity yielded by a walk. `Local` handles are NOT durable
/// across sessions — they wrap `Entity::to_bits()` for entities that
/// don't have a resolved durable id (debug overlays, interaction
/// visualisers, any other ephemerals).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeRef<Id> {
    /// Entity has a durable id from a [`NodeIdResolver`].
    Replicated(Id),
    /// Entity has no durable id; the `u64` is `Entity::to_bits()`,
    /// stable for the lifetime of this session only.
    Local(u64),
}

/// Look an entity up in `resolver` (if present); fall back to a
/// session-local `Entity::to_bits()` handle. This is what makes
/// non-replicated entities visible to the public API.
pub fn resolve_node_ref<R: NodeIdResolver>(
    entity: Entity,
    resolver: Option<&R>,
) -> NodeRef<R::Id> {
    match resolver.and_then(|r| r.resolve(entity)) {
        Some(id) => NodeRef::Replicated(id),
        None => NodeRef::Local(entity.to_bits()),
    }
}
