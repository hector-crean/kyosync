//! Heuristic strategies for WFC node selection and state selection.

use rand::Rng;

use super::domain_set::{Candidate, DomainSet};

/// Re-export so callers don't need to add `rand` as a direct dependency just
/// for the trait object.
pub use rand::RngCore;

/// Selects which open slot to collapse next, and which candidate to pick.
///
/// `select_candidate` takes a `&[f32]` of per-node-variant weights (obtained
/// via `NodeLike::weight()`) so the trait remains dyn-compatible.
pub trait WfcHeuristic: Send + Sync + 'static {
    /// Choose the index (into `domains`) of the next slot to collapse.
    /// Only slots with `domain.len() > 1` should be considered.
    fn select_slot(&self, domains: &[DomainSet]) -> Option<usize>;

    /// Choose one candidate from the domain of the selected slot.
    /// `node_weights[i]` is the weight for node variant index `i`.
    fn select_candidate(
        &self,
        domain: &DomainSet,
        node_weights: &[f32],
        rng: &mut dyn RngCore,
    ) -> Option<Candidate>;
}

// ---------------------------------------------------------------------------
// MinEntropyHeuristic
// ---------------------------------------------------------------------------

/// Classic WFC heuristic: pick the slot with the smallest non-trivial domain
/// (lowest entropy), breaking ties randomly.  Select a candidate weighted by
/// `NodeLike::weight()`.
pub struct MinEntropyHeuristic;

impl WfcHeuristic for MinEntropyHeuristic {
    fn select_slot(&self, domains: &[DomainSet]) -> Option<usize> {
        domains
            .iter()
            .enumerate()
            .filter(|(_, d)| d.len() > 1)
            .min_by(|(_, a), (_, b)| {
                a.entropy()
                    .partial_cmp(&b.entropy())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    fn select_candidate(
        &self,
        domain: &DomainSet,
        node_weights: &[f32],
        rng: &mut dyn RngCore,
    ) -> Option<Candidate> {
        weighted_random_select(domain, node_weights, rng)
    }
}

// ---------------------------------------------------------------------------
// UniformRandomHeuristic
// ---------------------------------------------------------------------------

/// Pick a random uncollapsed slot, then a uniformly random candidate.
pub struct UniformRandomHeuristic;

impl WfcHeuristic for UniformRandomHeuristic {
    fn select_slot(&self, domains: &[DomainSet]) -> Option<usize> {
        let uncollapsed: Vec<usize> = domains
            .iter()
            .enumerate()
            .filter(|(_, d)| d.len() > 1)
            .map(|(i, _)| i)
            .collect();
        if uncollapsed.is_empty() {
            return None;
        }
        Some(uncollapsed[0])
    }

    fn select_candidate(
        &self,
        domain: &DomainSet,
        _node_weights: &[f32],
        rng: &mut dyn RngCore,
    ) -> Option<Candidate> {
        let candidates: Vec<&Candidate> = domain.iter().collect();
        if candidates.is_empty() {
            return None;
        }
        let idx = rng.gen_range(0..candidates.len());
        Some(candidates[idx].clone())
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn weighted_random_select(
    domain: &DomainSet,
    node_weights: &[f32],
    rng: &mut dyn RngCore,
) -> Option<Candidate> {
    let candidates: Vec<&Candidate> = domain.iter().collect();
    if candidates.is_empty() {
        return None;
    }

    let weights: Vec<f64> = candidates
        .iter()
        .map(|c| {
            node_weights
                .get(c.node_idx)
                .copied()
                .unwrap_or(1.0) as f64
        })
        .collect();
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return Some(candidates[0].clone());
    }

    let mut dart = rng.gen_range(0.0..total);
    for (i, w) in weights.iter().enumerate() {
        dart -= w;
        if dart <= 0.0 {
            return Some(candidates[i].clone());
        }
    }
    candidates.last().map(|c| (*c).clone())
}
