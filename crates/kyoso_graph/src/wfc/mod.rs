//! Wave Function Collapse solver for graph-based configurators.
//!
//! Operates on the petgraph mirror maintained by [`GraphManagerPlugin`] and
//! emits [`GraphCommand`]s to materialise suggestions into the ECS world.

pub mod compatibility;
pub mod domain_set;
pub mod error;
pub mod heuristic;
pub mod propagator;
pub mod solver;

pub use compatibility::*;
pub use domain_set::*;
pub use error::*;
pub use heuristic::*;
pub use propagator::*;
pub use solver::*;
