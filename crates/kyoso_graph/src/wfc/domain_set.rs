//! Per-node possibility tracking for WFC.
//!
//! Each uncollapsed node maintains a *domain* -- the set of (node_variant,
//! edge_variant) pairs that are still considered valid.  The AC-3 propagator
//! shrinks these domains until a node can be collapsed.

use std::collections::BTreeSet;

use crate::domain::NodeLike;

/// Index into the catalog's `node_variants()` / `edge_variants()` arrays.
pub type VariantIdx = usize;

/// A single candidate placement: a specific node variant connected via a
/// specific edge variant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Candidate {
    pub node_idx: VariantIdx,
    pub edge_idx: VariantIdx,
}

/// The set of still-valid candidates for an open port on a graph node.
#[derive(Clone, Debug)]
pub struct DomainSet {
    candidates: BTreeSet<Candidate>,
}

impl DomainSet {
    /// Create a full domain from all (node, edge) combinations.
    pub fn full(num_node_variants: usize, num_edge_variants: usize) -> Self {
        let mut candidates = BTreeSet::new();
        for ni in 0..num_node_variants {
            for ei in 0..num_edge_variants {
                candidates.insert(Candidate {
                    node_idx: ni,
                    edge_idx: ei,
                });
            }
        }
        Self { candidates }
    }

    pub fn empty() -> Self {
        Self {
            candidates: BTreeSet::new(),
        }
    }

    pub fn from_candidates(candidates: impl IntoIterator<Item = Candidate>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Entropy heuristic: fewer candidates = lower entropy = higher priority
    /// for collapse.
    pub fn entropy(&self) -> f64 {
        self.candidates.len() as f64
    }

    pub fn contains(&self, c: &Candidate) -> bool {
        self.candidates.contains(c)
    }

    pub fn remove(&mut self, c: &Candidate) -> bool {
        self.candidates.remove(c)
    }

    /// Retain only candidates that satisfy the predicate.
    /// Returns `true` if any candidates were removed.
    pub fn retain(&mut self, mut f: impl FnMut(&Candidate) -> bool) -> bool {
        let before = self.candidates.len();
        self.candidates.retain(|c| f(c));
        self.candidates.len() != before
    }

    pub fn iter(&self) -> impl Iterator<Item = &Candidate> {
        self.candidates.iter()
    }

    /// Collapse to a single candidate (the chosen one).
    pub fn collapse_to(&mut self, chosen: Candidate) {
        self.candidates.clear();
        self.candidates.insert(chosen);
    }

    pub fn candidates(&self) -> &BTreeSet<Candidate> {
        &self.candidates
    }

    /// Compute the weighted entropy using node weights from a catalog.
    pub fn weighted_entropy<N: NodeLike>(
        &self,
        node_variants: &[N],
    ) -> f64 {
        if self.candidates.is_empty() {
            return 0.0;
        }
        let total_weight: f64 = self
            .candidates
            .iter()
            .map(|c| node_variants.get(c.node_idx).map_or(1.0, |n| n.weight() as f64))
            .sum();
        if total_weight <= 0.0 {
            return 0.0;
        }
        // Shannon entropy
        let mut entropy = 0.0f64;
        for c in &self.candidates {
            let w = node_variants
                .get(c.node_idx)
                .map_or(1.0, |n| n.weight() as f64);
            let p = w / total_weight;
            if p > 0.0 {
                entropy -= p * p.ln();
            }
        }
        entropy
    }
}
