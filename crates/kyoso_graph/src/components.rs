//! Core directed-graph topology components.
//!
//! An edge is its own entity carrying [`EdgeFrom`] (source node) and
//! [`EdgeTo`] (target node). Bevy relationships maintain reverse indices
//! ([`OutgoingEdges`], [`IncomingEdges`]) on each node automatically.
//!
//! Visual / domain attributes (color, position, weight overrides etc.)
//! are not modelled here — consumers attach their own components.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Edge endpoint: the source node of this edge.
///
/// The entity with `EdgeFrom` is an edge entity, not a node.
#[derive(Component, Debug, Clone, Copy)]
#[relationship(relationship_target = OutgoingEdges)]
pub struct EdgeFrom(#[relationship] pub Entity);

/// Edge endpoint: the target node of this edge.
///
/// The entity with `EdgeTo` is an edge entity, not a node.
#[derive(Component, Debug, Clone, Copy)]
#[relationship(relationship_target = IncomingEdges)]
pub struct EdgeTo(#[relationship] pub Entity);

/// Reverse index: all edges that start at this node (outgoing).
#[derive(Component, Debug, Default)]
#[relationship_target(relationship = EdgeFrom)]
pub struct OutgoingEdges(Vec<Entity>);

impl OutgoingEdges {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Reverse index: all edges that end at this node (incoming).
#[derive(Component, Debug, Default)]
#[relationship_target(relationship = EdgeTo)]
pub struct IncomingEdges(Vec<Entity>);

impl IncomingEdges {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Weight/cost associated with an edge (used for shortest-path, MST, etc.).
#[derive(Component, Debug, Clone, Copy, Default, Reflect, Serialize, Deserialize)]
#[reflect(Component, Serialize, Deserialize)]
pub struct EdgeWeight(pub f32);
