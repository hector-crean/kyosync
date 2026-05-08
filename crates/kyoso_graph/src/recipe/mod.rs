//! Subgraph pattern matching and recipe application (TinyGlade-inspired).
//!
//! A **recipe** is a small template graph (the *pattern*) together with a
//! *transform* that is applied whenever the pattern is found in the main
//! graph.  Transforms can annotate matched entities, collapse subgraphs into
//! a single composite node, or upgrade edge types.

pub mod matcher;
pub mod pattern;
pub mod transform;

pub use matcher::*;
pub use pattern::*;
pub use transform::*;
