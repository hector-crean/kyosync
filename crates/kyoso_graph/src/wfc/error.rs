//! WFC error types.

use std::fmt;

use petgraph::graph::NodeIndex;

#[derive(Clone, Debug)]
pub enum WfcError {
    /// A node's domain was reduced to zero -- no valid placement exists.
    Contradiction(NodeIndex),
    /// Backtracking exhausted all options.
    NoSolution,
    /// The heuristic could not select a node to collapse.
    HeuristicFailure,
    /// A referenced node was not found in the graph.
    NodeNotFound(NodeIndex),
}

impl fmt::Display for WfcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contradiction(ni) => write!(f, "contradiction at node {}", ni.index()),
            Self::NoSolution => write!(f, "no solution found after exhaustive search"),
            Self::HeuristicFailure => write!(f, "heuristic failed to select a node"),
            Self::NodeNotFound(ni) => write!(f, "node {} not found in graph", ni.index()),
        }
    }
}

impl std::error::Error for WfcError {}
