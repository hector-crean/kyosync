//! Core constraint traits.

use std::fmt;

use crate::domain::{EdgeLike, NodeLike};

// ---------------------------------------------------------------------------
// ConstraintViolation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ConstraintViolation {
    pub message: String,
    pub severity: Severity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Hard violation -- the graph is invalid.
    Error,
    /// Soft violation -- the graph is legal but sub-optimal.
    Warning,
}

impl fmt::Display for ConstraintViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}", self.severity, self.message)
    }
}

// ---------------------------------------------------------------------------
// GraphConstraint
// ---------------------------------------------------------------------------

/// A rule that can accept or reject a (node, neighbourhood) or (edge, endpoints)
/// configuration.
///
/// Constraints are used in two places:
///
/// 1. **Validation** -- a solver system iterates all nodes/edges each frame and
///    collects violations for the UI to display.
/// 2. **WFC filtering** -- the WFC solver calls `check_node` on candidate
///    placements to prune the domain set before collapse.
pub trait GraphConstraint<N: NodeLike, E: EdgeLike>: Send + Sync + 'static {
    fn name(&self) -> &str;

    /// Validate a node given its current neighbours.
    /// `edges` contains `(edge_value, neighbour_node_value)` pairs.
    fn check_node(&self, node: &N, edges: &[(E, N)]) -> Result<(), ConstraintViolation>;

    /// Validate an edge given its two endpoint nodes.
    fn check_edge(&self, edge: &E, from: &N, to: &N) -> Result<(), ConstraintViolation>;
}

// ---------------------------------------------------------------------------
// ConstraintSet
// ---------------------------------------------------------------------------

/// An ordered collection of constraints that are evaluated together.
pub struct ConstraintSet<N: NodeLike, E: EdgeLike> {
    constraints: Vec<Box<dyn GraphConstraint<N, E>>>,
}

impl<N: NodeLike, E: EdgeLike> Default for ConstraintSet<N, E> {
    fn default() -> Self {
        Self {
            constraints: Vec::new(),
        }
    }
}

impl<N: NodeLike, E: EdgeLike> ConstraintSet<N, E> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, c: impl GraphConstraint<N, E>) {
        self.constraints.push(Box::new(c));
    }

    pub fn check_node(&self, node: &N, edges: &[(E, N)]) -> Vec<ConstraintViolation> {
        self.constraints
            .iter()
            .filter_map(|c| c.check_node(node, edges).err())
            .collect()
    }

    pub fn check_edge(&self, edge: &E, from: &N, to: &N) -> Vec<ConstraintViolation> {
        self.constraints
            .iter()
            .filter_map(|c| c.check_edge(edge, from, to).err())
            .collect()
    }

    pub fn is_node_valid(&self, node: &N, edges: &[(E, N)]) -> bool {
        self.constraints
            .iter()
            .all(|c| c.check_node(node, edges).is_ok())
    }

    pub fn is_edge_valid(&self, edge: &E, from: &N, to: &N) -> bool {
        self.constraints
            .iter()
            .all(|c| c.check_edge(edge, from, to).is_ok())
    }

    pub fn constraints(&self) -> &[Box<dyn GraphConstraint<N, E>>] {
        &self.constraints
    }
}
