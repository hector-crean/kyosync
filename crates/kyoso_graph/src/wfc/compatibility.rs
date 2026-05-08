//! Compatibility rules determining which (node, edge) pairs may be adjacent.
//!
//! Ported from `kyoso_block3d_algorithm` but made generic over domain traits.

use crate::domain::{EdgeLike, NodeLike};

/// A single compatibility rule expressed as a predicate.
pub struct CompatibilityRule<N: NodeLike, E: EdgeLike> {
    pub check: Box<dyn Fn(&N, &E, &N) -> bool + Send + Sync>,
    pub description: Option<String>,
}

impl<N: NodeLike, E: EdgeLike> CompatibilityRule<N, E> {
    pub fn new(
        check: impl Fn(&N, &E, &N) -> bool + Send + Sync + 'static,
        description: impl Into<Option<String>>,
    ) -> Self {
        Self {
            check: Box::new(check),
            description: description.into(),
        }
    }
}

/// A collection of compatibility rules.  All rules must pass for a
/// (from_node, edge, to_node) triple to be considered compatible.
pub struct CompatibilityTable<N: NodeLike, E: EdgeLike> {
    rules: Vec<CompatibilityRule<N, E>>,
}

impl<N: NodeLike, E: EdgeLike> Default for CompatibilityTable<N, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: NodeLike, E: EdgeLike> CompatibilityTable<N, E> {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add_rule(&mut self, rule: CompatibilityRule<N, E>) {
        self.rules.push(rule);
    }

    /// Returns `true` when *all* rules accept the triple.
    /// An empty rule set is vacuously compatible.
    pub fn is_compatible(&self, from: &N, edge: &E, to: &N) -> bool {
        self.rules.iter().all(|r| (r.check)(from, edge, to))
    }

    pub fn rules(&self) -> &[CompatibilityRule<N, E>] {
        &self.rules
    }
}
