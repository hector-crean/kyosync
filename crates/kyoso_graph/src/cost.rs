//! Cost hints for graph iterators.
//!
//! [`Cost`] is an order-of-magnitude estimate, not an exact bound. It
//! exists so an agent (or a planner) can *compare* candidate iterators
//! and pick the cheapest one before it starts walking — without
//! committing to actually consuming any of them.
//!
//! Two fields, intentionally:
//!
//! - [`Cost::estimated_items`] — upper bound on yield count.
//! - [`Cost::estimated_work`] — upper bound on neighbor inspections.
//!
//! They diverge for filter-heavy iterators (e.g. subgraph matching),
//! which can do enormous work to yield zero items. A planner that
//! collapses them into one number loses signal.
//!
//! All iterators in [`crate::traverse`] and [`crate::subgraph`] impl
//! [`CostHint`].

/// Coarse upper-bound cost estimate for a streaming graph operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// Upper bound on the number of items the iterator will yield.
    pub estimated_items: u64,
    /// Upper bound on total work units, where a "work unit" is one
    /// neighbor inspection (one `successors` step, one candidate check,
    /// etc.). Diverges from `estimated_items` for filter-heavy ops.
    pub estimated_work: u64,
}

impl Cost {
    pub const ZERO: Cost = Cost { estimated_items: 0, estimated_work: 0 };

    /// Sum two costs, saturating at `u64::MAX`.
    pub fn saturating_add(self, other: Cost) -> Cost {
        Cost {
            estimated_items: self.estimated_items.saturating_add(other.estimated_items),
            estimated_work: self.estimated_work.saturating_add(other.estimated_work),
        }
    }
}

/// Iterator self-estimate. Implemented by all kyoso_graph iterators
/// so a planner has one uniform API across BFS, DFS, subgraph match,
/// etc. Returning `Cost::default()` is acceptable when an estimate
/// can't be computed cheaply, but prefer a saturating upper bound.
pub trait CostHint {
    fn cost(&self) -> Cost;
}
