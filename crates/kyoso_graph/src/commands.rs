//! Graph command utilities.
//!
//! Provides ergonomic helpers for spawning and removing edges.

use bevy::prelude::*;
use bevy::ecs::query::{QueryData, QueryFilter};

use super::components::{EdgeFrom, EdgeTo};
use super::queries::GraphQuery;

// ============================================================================
// Spawning convenience
// ============================================================================

/// Spawn an edge entity related to `from` (as `EdgeFrom`) and targeting `to` (as `EdgeTo`).
pub fn spawn_edge(commands: &mut Commands, from: Entity, to: Entity) {
    commands
        .entity(from)
        .with_related_entities::<EdgeFrom>(|rel| {
            rel.spawn(EdgeTo(to));
        });
}

/// Remove the edge entity `from -> to` if it exists.
pub fn remove_edge<N, E, NF, EF>(
    commands: &mut Commands,
    graph: &GraphQuery<N, E, NF, EF>,
    from: Entity,
    to: Entity,
) where
    N: QueryData,
    E: QueryData,
    NF: QueryFilter,
    EF: QueryFilter,
{
    if let Some(edge) = graph.find_edge(from, to) {
        commands.entity(edge).despawn();
    }
}

// ============================================================================
// Commands extensions
// ============================================================================

/// Trait for spawning edges (doesn't require type parameters).
pub trait GraphSpawnExt {
    fn spawn_edge(&mut self, from: Entity, to: Entity);
    fn spawn_edges<I: IntoIterator<Item = (Entity, Entity)>>(&mut self, pairs: I);
}

impl<'w, 's> GraphSpawnExt for Commands<'w, 's> {
    fn spawn_edge(&mut self, from: Entity, to: Entity) {
        spawn_edge(self, from, to)
    }

    fn spawn_edges<I: IntoIterator<Item = (Entity, Entity)>>(&mut self, pairs: I) {
        for (from, to) in pairs {
            spawn_edge(self, from, to);
        }
    }
}

pub trait GraphCommandsExt<N: QueryData, E: QueryData, NF: QueryFilter = (), EF: QueryFilter = ()> {
    fn remove_edge(&mut self, graph: &GraphQuery<N, E, NF, EF>, from: Entity, to: Entity);
    fn remove_all_outgoing(&mut self, graph: &GraphQuery<N, E, NF, EF>, node: Entity);
    fn remove_all_incoming(&mut self, graph: &GraphQuery<N, E, NF, EF>, node: Entity);
}

impl<'w, 's, N, E, NF, EF> GraphCommandsExt<N, E, NF, EF> for Commands<'w, 's>
where
    N: QueryData,
    E: QueryData,
    NF: QueryFilter,
    EF: QueryFilter,
{
    fn remove_edge(&mut self, graph: &GraphQuery<N, E, NF, EF>, from: Entity, to: Entity) {
        remove_edge(self, graph, from, to)
    }

    fn remove_all_outgoing(&mut self, graph: &GraphQuery<N, E, NF, EF>, node: Entity) {
        for edge in graph.outgoing_edges(node) {
            self.entity(edge).despawn();
        }
    }

    fn remove_all_incoming(&mut self, graph: &GraphQuery<N, E, NF, EF>, node: Entity) {
        for edge in graph.incoming_edges(node) {
            self.entity(edge).despawn();
        }
    }
}

pub trait GraphEntityCommandsExt<'a, N: QueryData, E: QueryData, NF: QueryFilter = (), EF: QueryFilter = ()> {
    fn connect_to(&mut self, to: Entity) -> &mut Self;
    fn disconnect_from(&mut self, graph: &GraphQuery<N, E, NF, EF>, to: Entity) -> &mut Self;
    fn clear_outgoing(&mut self, graph: &GraphQuery<N, E, NF, EF>) -> &mut Self;
    fn clear_incoming(&mut self, graph: &GraphQuery<N, E, NF, EF>) -> &mut Self;
}

impl<'a, N, E, NF, EF> GraphEntityCommandsExt<'a, N, E, NF, EF> for bevy::ecs::system::EntityCommands<'a>
where
    N: QueryData,
    E: QueryData,
    NF: QueryFilter,
    EF: QueryFilter,
{
    fn connect_to(&mut self, to: Entity) -> &mut Self {
        self.with_related_entities::<EdgeFrom>(|rel| {
            rel.spawn(EdgeTo(to));
        });
        self
    }

    fn disconnect_from(&mut self, graph: &GraphQuery<N, E, NF, EF>, to: Entity) -> &mut Self {
        let from = self.id();
        if let Some(edge) = graph.find_edge(from, to) {
            self.commands().entity(edge).despawn();
        }
        self
    }

    fn clear_outgoing(&mut self, graph: &GraphQuery<N, E, NF, EF>) -> &mut Self {
        let from = self.id();
        for edge in graph.outgoing_edges(from) {
            self.commands().entity(edge).despawn();
        }
        self
    }

    fn clear_incoming(&mut self, graph: &GraphQuery<N, E, NF, EF>) -> &mut Self {
        let node = self.id();
        for edge in graph.incoming_edges(node) {
            self.commands().entity(edge).despawn();
        }
        self
    }
}
