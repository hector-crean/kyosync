//! [`TraversalQuery`] — the granular search the agent-facing API takes:
//! start, order, max depth, and component-presence filters. Run via
//! [`super::runner::run_traversal`] / [`super::runner::run_traversal_with`]
//! against any [`GraphTraverse`](crate::traverse::GraphTraverse) impl
//! (tree, entity-edge graph, or a domain-typed combination). Each
//! `WorldXxxView` exposes inherent methods that forward to those
//! runners.

use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::world::EntityRef;

use crate::traverse::{Step, TraversalNode};

use super::resolver::NodeRef;

/// One row of walk output with a resolved durable id. `Id` matches
/// the [`NodeIdResolver::Id`] of whichever resolver was used by the
/// `traverse_with` call.
#[derive(Clone, Debug)]
pub struct WorldEntityRef<Id> {
    pub id: NodeRef<Id>,
    pub entity: Entity,
    /// Depth from the query's start node (0 = start).
    pub depth: usize,
    /// Parent entity in the traversal tree (None for the start node).
    pub parent: Option<Entity>,
}

/// Walk order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Bfs,
    Dfs,
}

/// Component-presence filter closure type.
///
/// `'static` because `require::<T>` / `exclude::<T>` closures capture
/// nothing. `for<'a>` (higher-ranked) so each call can hand the closure
/// a fresh `EntityRef` borrowed from the live `&World`.
type Filter = Box<dyn for<'a> Fn(EntityRef<'a>) -> bool + 'static>;

/// Caller-supplied policy: given the traversal node and a borrowed
/// view of every component on its entity, decide a [`Step`].
///
/// `for<'a>` (HRTB) so the policy can be called with any `EntityRef`
/// lifetime; `'static` because the closures we want to take from
/// callers don't borrow shorter-lived data.
type StepFn = Box<dyn for<'a> Fn(&TraversalNode, EntityRef<'a>) -> Step + 'static>;

/// Granular walk search parameters: where to start, how to walk, how
/// deep, and which entities to keep.
///
/// ## Two-stage filter, on purpose
///
/// Structural pruning ([`max_depth`](Self::max_depth)) runs **inside**
/// the walk policy via [`Step::Skip`] — it controls expansion. Value
/// filtering ([`require`](Self::require) / [`exclude`](Self::exclude))
/// runs as a **post-filter on the yielded stream** — a non-matching
/// parent is hidden but its subtree is still explored, so the agent
/// doesn't lose matching grandchildren under a non-matching parent.
///
/// The four-variant [`Step`] enum (`Visit` / `Skip` / `Prune` /
/// `Stop`) has no "hide but keep descending" mode, which is exactly
/// why component filtering happens outside the walk.
pub struct TraversalQuery {
    pub(crate) root: Option<Entity>,
    pub(crate) order: Order,
    pub(crate) max_depth: Option<usize>,
    pub(crate) filters: Vec<Filter>,
    pub(crate) step: Option<StepFn>,
}

impl Default for TraversalQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl TraversalQuery {
    pub fn new() -> Self {
        Self {
            root: None,
            order: Order::Dfs,
            max_depth: None,
            filters: Vec::new(),
            step: None,
        }
    }

    /// Start the walk from `root`. Required — no scene-root inference.
    pub fn start_at(mut self, root: Entity) -> Self {
        self.root = Some(root);
        self
    }

    /// Walk order: BFS or DFS. Default is DFS.
    pub fn order(mut self, order: Order) -> Self {
        self.order = order;
        self
    }

    /// Cap the depth of expanded successors. Nodes at exactly `depth`
    /// are yielded; their successors are not enqueued.
    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Keep only entities that have component `T`.
    pub fn require<T: Component>(mut self) -> Self {
        self.filters.push(Box::new(|er| er.contains::<T>()));
        self
    }

    /// Drop entities that have component `T`.
    pub fn exclude<T: Component>(mut self) -> Self {
        self.filters.push(Box::new(|er| !er.contains::<T>()));
        self
    }

    /// Caller-supplied per-node policy. The closure gets the traversal
    /// node *and* a borrowed view of every component on its entity,
    /// and returns a [`Step`] controlling whether the node is yielded,
    /// expanded, pruned, or halts the walk.
    ///
    /// Composes with [`max_depth`](Self::max_depth): depth pruning
    /// runs first; the `step_with` closure only sees nodes that
    /// survived the depth cap.
    ///
    /// Strictly more expressive than [`require`](Self::require) /
    /// [`exclude`](Self::exclude) — sees component *values*, can
    /// return [`Step::Prune`] to skip an entire subtree, and can
    /// return [`Step::Stop`] to halt the walk early.
    pub fn step_with<F>(mut self, f: F) -> Self
    where
        F: for<'a> Fn(&TraversalNode, EntityRef<'a>) -> Step + 'static,
    {
        self.step = Some(Box::new(f));
        self
    }
}

// `traverse` / `traverse_with` / `component_names` methods live on
// each `WorldXxxView` in `super::view`; they forward to the shared
// runners in `super::runner`.
