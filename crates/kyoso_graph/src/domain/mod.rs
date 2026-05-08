//! Generic domain abstraction for graph-based configurators.
//!
//! These traits define *what* can sit on graph nodes and edges, independent of
//! any particular domain (UI scene graphs, workflow nodes, electronics,
//! architecture, ...). Concrete domains implement these traits and plug into
//! the WFC solver, constraint checker, and recipe matcher.

use std::fmt;

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

/// A connection point on a node that can be wired to other nodes via edges.
///
/// Ports carry an identifier, a compatibility predicate, and a capacity
/// (how many edges may attach to this port before it is saturated).
pub trait Port: Clone + fmt::Debug + Send + Sync + 'static {
    fn id(&self) -> &str;

    /// Whether `self` can be connected to `other` via an edge.
    fn is_compatible_with(&self, other: &Self) -> bool;

    /// Maximum number of edges that may attach to this port.
    fn capacity(&self) -> usize;
}

// ---------------------------------------------------------------------------
// NodeLike
// ---------------------------------------------------------------------------

/// A domain value that can inhabit a graph node.
///
/// Analogous to `Block3DLike` in `kyoso_block3d_core`, but graph-centric
/// rather than grid-centric.
pub trait NodeLike: Clone + fmt::Debug + Send + Sync + 'static {
    type Port: Port;

    /// The ports this node type exposes for edge connections.
    fn ports(&self) -> &[Self::Port];

    /// A short human-readable label (e.g. "C", "R1k", "Wall").
    fn symbol(&self) -> &str;

    /// Relative weight / priority for WFC heuristic selection.
    /// Higher values make this node type more likely to be chosen.
    fn weight(&self) -> f32 {
        1.0
    }
}

// ---------------------------------------------------------------------------
// EdgeLike
// ---------------------------------------------------------------------------

/// A domain value that can inhabit a graph edge.
pub trait EdgeLike: Clone + fmt::Debug + Send + Sync + 'static {
    /// Multiplicity of this edge (bond order, wire count, lane count, ...).
    fn order(&self) -> u8 {
        1
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// The full set of node/edge types available in a given domain, used by the
/// WFC solver to enumerate possibilities.
pub trait Catalog: Send + Sync + 'static {
    type Node: NodeLike;
    type Edge: EdgeLike;

    /// All node variants that the WFC solver may place.
    fn node_variants(&self) -> &[Self::Node];

    /// All edge variants that the WFC solver may place.
    fn edge_variants(&self) -> &[Self::Edge];
}
