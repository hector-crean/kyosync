//! Per-category typed-edge dispatch on top of [`EdgeCategory`].
//!
//! The base [`crate::GraphSyncPlugin`] treats every reference edge as
//! [`EdgeCategory::Reference`]. To get richer semantics — different
//! conflict-resolution policies, different dangle policies, distinct
//! reverse-index lookups — register an [`EdgeCategoryMarker`] type per
//! category via [`SyncedEdgeCategoryPlugin`].
//!
//! Each marker is a Bevy [`Component`] whose presence on an edge
//! entity tells the sync layer "this edge belongs to category X."
//! Detection systems prefer the typed path (one per registered
//! category) over the generic [`Reference`](EdgeCategory::Reference)
//! path. Inbound projection inserts the matching marker when an
//! [`OpKind::AddRefEdge`](kyoso_graph_crdt::OpKind::AddRefEdge) for
//! that category arrives.
//!
//! ## Example
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_graph_crdt::EdgeCategory;
//! use kyoso_graph_sync::{EdgeCategoryMarker, SyncedEdgeCategoryPlugin};
//!
//! #[derive(Component, Default, Debug, Clone)]
//! struct InstanceOfEdge;
//! impl EdgeCategoryMarker for InstanceOfEdge {
//!     fn category() -> EdgeCategory { EdgeCategory::InstanceOf }
//! }
//!
//! app.add_plugins(SyncedEdgeCategoryPlugin::<MyNode, MyEdge, InstanceOfEdge>::default());
//! ```
//!
//! Now `commands.spawn((EdgeFrom(a), EdgeTo(b), MyEdge::default(), InstanceOfEdge))`
//! produces an `AddRefEdge { category: InstanceOf, .. }` op on the wire,
//! and remote `InstanceOf` edges arrive with the `InstanceOfEdge`
//! component pre-attached.

use std::collections::HashMap;
use std::marker::PhantomData;

use bevy::prelude::*;
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph::tree::TreeEdge;
use kyoso_graph_crdt::EdgeCategory;

use crate::engine::ClientSyncEngine;
use crate::index::EntityCrdtIndex;
use crate::plugin::Syncable;

/// Marker trait for Bevy components that identify an edge's category.
pub trait EdgeCategoryMarker:
    Component<Mutability = bevy::ecs::component::Mutable> + Default + Send + Sync + 'static
{
    /// The CRDT-level category this marker represents.
    fn category() -> EdgeCategory;
}

/// Inbound projection: when an [`OpKind::AddRefEdge`](kyoso_graph_crdt::OpKind::AddRefEdge)
/// arrives with a known category, look up the matching marker insertion
/// fn and apply it to the freshly-spawned edge entity.
#[derive(Resource, Default)]
pub struct EdgeCategoryProjectors {
    by_category: HashMap<String, fn(&mut World, Entity)>,
}

impl EdgeCategoryProjectors {
    pub fn register<M: EdgeCategoryMarker>(&mut self) {
        let key = format!("{:?}", M::category());
        self.by_category.insert(key, |world: &mut World, entity: Entity| {
            world.entity_mut(entity).insert(M::default());
        });
    }

    pub fn project(&self, world: &mut World, entity: Entity, category: &EdgeCategory) {
        let key = format!("{category:?}");
        if let Some(insert) = self.by_category.get(&key) {
            insert(world, entity);
        }
    }
}

pub struct SyncedEdgeCategoryPlugin<N, E, M> {
    _phantom: PhantomData<fn() -> (N, E, M)>,
}

impl<N, E, M> Default for SyncedEdgeCategoryPlugin<N, E, M> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<N, E, M> Plugin for SyncedEdgeCategoryPlugin<N, E, M>
where
    N: Syncable,
    E: Syncable,
    M: EdgeCategoryMarker,
{
    fn build(&self, app: &mut App) {
        app.init_resource::<EdgeCategoryProjectors>();
        app.world_mut()
            .resource_mut::<EdgeCategoryProjectors>()
            .register::<M>();
        app.add_systems(
            Update,
            detect_added_categorized_edges::<E, M>
                .before(crate::plugin::detect_added_edges::<N, E>),
        );
    }
}

/// Deferred command issued by inbound projection: insert the marker
/// for `category` on `entity`, if a marker for that category has been
/// registered.
pub(crate) struct ApplyEdgeCategory {
    pub entity: Entity,
    pub category: EdgeCategory,
}

impl bevy::ecs::system::Command for ApplyEdgeCategory {
    type Out = ();
    fn apply(self, world: &mut World) {
        let projectors = world
            .get_resource::<EdgeCategoryProjectors>()
            .map(|r| r.by_category.clone());
        let Some(by_category) = projectors else {
            return;
        };
        let key = format!("{:?}", self.category);
        if let Some(insert) = by_category.get(&key) {
            insert(world, self.entity);
        }
    }
}

/// One-per-category detection system: edges with the marker `M` are
/// emitted as `OpKind::AddRefEdge { category: M::category(), .. }`.
fn detect_added_categorized_edges<E, M>(
    mut engine: ResMut<ClientSyncEngine>,
    mut index: ResMut<EntityCrdtIndex>,
    edges: Query<(Entity, &EdgeFrom, &EdgeTo), (Added<M>, With<E>, Without<TreeEdge>)>,
) where
    E: Syncable,
    M: EdgeCategoryMarker,
{
    for (edge_entity, from, to) in edges.iter() {
        if index.edge_id(edge_entity).is_some() {
            continue;
        }
        let (Some(from_id), Some(to_id)) = (index.node_id(from.0), index.node_id(to.0)) else {
            continue;
        };
        let edge_id = engine.add_ref_edge_with_category(from_id, to_id, M::category());
        index.bind_edge(edge_entity, edge_id);
    }
}
