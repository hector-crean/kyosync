//! Generic constraint system for graph-based configurators.
//!
//! Constraints validate the current graph state and are also consulted by the
//! WFC solver to prune impossible placements early. Domain-specific
//! constraint implementations (valence, octet, etc.) live in their consuming
//! crate, not here.

pub mod traits;

pub use traits::*;
