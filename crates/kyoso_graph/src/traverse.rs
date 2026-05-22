//! Graph traversal traits and iterators.
//!
//! Three layered traits decouple "what to walk" from "how to walk":
//!
//! - [`GraphTraverse`] — minimum surface: forward (`successors`) and
//!   reverse (`predecessors`) neighbor enumeration. BFS/DFS iterators
//!   are generic over any `G: GraphTraverse`.
//! - [`GraphTraverseEdges`] — edge-aware extension yielding
//!   `(edge, neighbor)` pairs so consumers can inspect / filter on
//!   edge data (weights, edge variants, …).
//! - [`OrderedTraverse`] — order-preserving enumeration via
//!   `ordered_successors`. Trees impl this naturally (`OrderKey`);
//!   general directed graphs don't, because order isn't intrinsic.
//!
//! [`Reverse`] is a newtype wrapper that flips direction, so reverse
//! traversal doesn't need its own iterator types:
//! `BfsIter::new(&Reverse(&graph), root)` is reverse BFS.
//!
//! [`GraphQuery`] implements `GraphTraverse` + `GraphTraverseEdges`.
//! `OrderedTraverse` is left for tree-shaped views (forthcoming
//! `TreeSnapshotQuery`) to implement.

use bevy::{
    ecs::query::{QueryData, QueryFilter},
    prelude::*,
};
use std::collections::{HashSet, VecDeque};

use crate::cost::{Cost, CostHint};
use crate::pattern::Pattern;
use crate::queries::GraphQuery;
use crate::subgraph::SubgraphMatches;

// ============================================================================
// TRAITS
// ============================================================================

/// Minimum surface for graph traversal: forward and reverse neighbor
/// enumeration. Order is unspecified.
///
/// Default methods provide BFS / DFS iterators that any implementor
/// gets for free.
pub trait GraphTraverse {
    /// Iterate outgoing-direction neighbors of `node`.
    fn successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_;

    /// Iterate incoming-direction neighbors of `node`.
    fn predecessors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_;

    /// BFS iterator following `successors`. Visits each entity at most once.
    fn bfs_iter(&self, start: Entity) -> BfsIter<'_, Self>
    where
        Self: Sized,
    {
        BfsIter::new(self, start)
    }

    /// DFS iterator following `successors`. Visits each entity at most once.
    fn dfs_iter(&self, start: Entity) -> DfsIter<'_, Self>
    where
        Self: Sized,
    {
        DfsIter::new(self, start)
    }

    /// BFS iterator that yields [`TraversalNode`] (entity + depth + parent).
    fn bfs_iter_with_depth(&self, start: Entity) -> BfsIterWithDepth<'_, Self>
    where
        Self: Sized,
    {
        BfsIterWithDepth::new(self, start)
    }

    /// DFS iterator that yields [`TraversalNode`] (entity + depth + parent).
    fn dfs_iter_with_depth(&self, start: Entity) -> DfsIterWithDepth<'_, Self>
    where
        Self: Sized,
    {
        DfsIterWithDepth::new(self, start)
    }

    /// Closure-driven BFS walk. The closure receives each candidate node
    /// and returns a [`Step`] controlling whether to yield it, expand
    /// its successors, and continue or halt the walk. See [`Step`] for
    /// the exact semantics.
    fn bfs_walk<F>(&self, start: Entity, policy: F) -> BfsWalk<'_, Self, F>
    where
        Self: Sized,
        F: FnMut(&TraversalNode) -> Step,
    {
        BfsWalk::new(self, start, policy)
    }

    /// DFS variant of [`bfs_walk`](Self::bfs_walk).
    fn dfs_walk<F>(&self, start: Entity, policy: F) -> DfsWalk<'_, Self, F>
    where
        Self: Sized,
        F: FnMut(&TraversalNode) -> Step,
    {
        DfsWalk::new(self, start, policy)
    }
}

/// Per-node control flow for closure-driven walks ([`GraphTraverse::bfs_walk`] /
/// [`GraphTraverse::dfs_walk`]).
///
/// | Variant | Yield this node? | Expand successors? | Continue walking? |
/// |---|---|---|---|
/// | `Visit` | yes | yes | yes |
/// | `Skip`  | yes | no  | yes |
/// | `Prune` | no  | no  | yes |
/// | `Stop`  | yes | no  | no  |
///
/// "Skip + halt" is omitted — compose via `.take_while` outside the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Yield this node and expand its successors.
    Visit,
    /// Yield this node but do not expand its successors (subtree pruning).
    Skip,
    /// Hide this node and do not expand its successors.
    Prune,
    /// Yield this node, then halt the walk.
    Stop,
}

/// Edge-aware enumeration. Yields `(edge, neighbor)` pairs so consumers
/// can inspect or filter on edge entities.
pub trait GraphTraverseEdges: GraphTraverse {
    /// Iterate outgoing edges of `node` as `(edge, target)` pairs.
    fn outgoing(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_;

    /// Iterate incoming edges of `node` as `(edge, source)` pairs.
    fn incoming(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_;

    /// BFS iterator yielding `(edge_used_to_reach_this_node, node)`. The
    /// start node yields `(None, start)`; every other node yields
    /// `(Some(edge), node)`.
    fn edge_bfs_iter(&self, start: Entity) -> EdgeBfsIter<'_, Self>
    where
        Self: Sized,
    {
        EdgeBfsIter::new(self, start)
    }

    /// DFS variant of [`edge_bfs_iter`].
    fn edge_dfs_iter(&self, start: Entity) -> EdgeDfsIter<'_, Self>
    where
        Self: Sized,
    {
        EdgeDfsIter::new(self, start)
    }

    /// Stream all subgraphs of `self` that match `pattern`.
    ///
    /// See [`crate::subgraph::SubgraphMatches`] for semantics. Requires
    /// [`GraphNodes`] so the iterator can enumerate candidate roots
    /// when the pattern is unanchored.
    fn subgraph_matches<'a, 'p>(
        &'a self,
        pattern: &'p Pattern<'p>,
    ) -> SubgraphMatches<'a, 'p, Self>
    where
        Self: Sized + GraphNodes,
    {
        SubgraphMatches::new(self, pattern)
    }
}

/// Whole-graph node enumeration + cardinality hints.
///
/// Separate from [`GraphTraverse`] (which is one-hop only) because not
/// every traversal view can cheaply list every node. [`GraphQuery`]
/// can — it has a typed ECS query over nodes. Tree-shaped views
/// usually can too.
///
/// Required for unanchored [`subgraph_matches`](GraphTraverseEdges::subgraph_matches)
/// queries and for cost estimation.
pub trait GraphNodes {
    /// Iterate all node entities in the graph.
    fn nodes(&self) -> impl Iterator<Item = Entity> + '_;

    /// Upper-bound hint on total node count. May be approximate;
    /// used only for cost estimation.
    fn node_count_hint(&self) -> usize {
        self.nodes().count()
    }

    /// Upper-bound hint on total edge count. May be approximate;
    /// used only for cost estimation. Default falls back to summing
    /// out-degrees, but `GraphTraverseEdges` impls should override
    /// this with something cheaper when they can.
    fn edge_count_hint(&self) -> usize {
        0
    }
}

/// Order-preserving enumeration. Tree-shaped graphs implement this
/// naturally via an ordering signal stored on nodes (e.g. `OrderKey`).
/// General directed graphs don't impl this — order isn't intrinsic.
pub trait OrderedTraverse: GraphTraverse {
    /// Iterate successors of `node` in a stable, implementor-defined order.
    fn ordered_successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_;

    /// BFS iterator using [`ordered_successors`](Self::ordered_successors)
    /// for neighbor expansion. Same yield as
    /// [`bfs_iter`](GraphTraverse::bfs_iter), but children of each node
    /// are queued in the implementor-defined order.
    fn ordered_bfs_iter(&self, start: Entity) -> OrderedBfsIter<'_, Self>
    where
        Self: Sized,
    {
        OrderedBfsIter::new(self, start)
    }

    /// DFS iterator using [`ordered_successors`](Self::ordered_successors).
    /// Leftmost child is visited first (children are pushed onto the stack
    /// in reverse order so the first listed pops first).
    fn ordered_dfs_iter(&self, start: Entity) -> OrderedDfsIter<'_, Self>
    where
        Self: Sized,
    {
        OrderedDfsIter::new(self, start)
    }
}

// ============================================================================
// BLANKET IMPLS — `&G` forwards to `G`
// ============================================================================
//
// Lets `Reverse(&graph_query)` work without consuming `graph_query`,
// and generally lets any borrowed graph view plug into the iterators.

impl<G: GraphTraverse + ?Sized> GraphTraverse for &G {
    fn successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        (**self).successors(node)
    }
    fn predecessors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        (**self).predecessors(node)
    }
}

impl<G: GraphTraverseEdges + ?Sized> GraphTraverseEdges for &G {
    fn outgoing(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        (**self).outgoing(node)
    }
    fn incoming(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        (**self).incoming(node)
    }
}

impl<G: OrderedTraverse + ?Sized> OrderedTraverse for &G {
    fn ordered_successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        (**self).ordered_successors(node)
    }
}

impl<G: GraphNodes + ?Sized> GraphNodes for &G {
    fn nodes(&self) -> impl Iterator<Item = Entity> + '_ {
        (**self).nodes()
    }
    fn node_count_hint(&self) -> usize {
        (**self).node_count_hint()
    }
    fn edge_count_hint(&self) -> usize {
        (**self).edge_count_hint()
    }
}

// ============================================================================
// REVERSE WRAPPER
// ============================================================================

/// Direction-reversing newtype. Flips `successors` ↔ `predecessors`
/// (and `outgoing` ↔ `incoming` for `GraphTraverseEdges`) without
/// introducing reverse-iterator variants.
///
/// ```ignore
/// // Reverse BFS from `node`:
/// for e in Reverse(&graph_query).bfs_iter(node) { /* … */ }
/// ```
pub struct Reverse<G>(pub G);

impl<G: GraphTraverse> GraphTraverse for Reverse<G> {
    fn successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.0.predecessors(node)
    }
    fn predecessors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.0.successors(node)
    }
}

impl<G: GraphTraverseEdges> GraphTraverseEdges for Reverse<G> {
    fn outgoing(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        self.0.incoming(node)
    }
    fn incoming(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        self.0.outgoing(node)
    }
}

impl<G: GraphNodes> GraphNodes for Reverse<G> {
    fn nodes(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.nodes()
    }
    fn node_count_hint(&self) -> usize {
        self.0.node_count_hint()
    }
    fn edge_count_hint(&self) -> usize {
        self.0.edge_count_hint()
    }
}

// ============================================================================
// IMPLS FOR GRAPHQUERY
// ============================================================================

impl<'w, 's, N, E, NF, EF> GraphTraverse for GraphQuery<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.neighbors(node)
    }
    fn predecessors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        GraphQuery::predecessors(self, node)
    }
}

impl<'w, 's, N, E, NF, EF> GraphTraverseEdges for GraphQuery<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn outgoing(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        self.neighbors_with_edges(node)
    }
    fn incoming(&self, node: Entity) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        self.incoming_edges(node)
            .filter_map(move |edge| self.edges_from.get(edge).ok().map(|ef| (edge, ef.0)))
    }
}

impl<'w, 's, N, E, NF, EF> GraphNodes for GraphQuery<'w, 's, N, E, NF, EF>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    fn nodes(&self) -> impl Iterator<Item = Entity> + '_ {
        self.nodes_q.iter().map(|(e, _, _, _)| e)
    }
    fn node_count_hint(&self) -> usize {
        self.node_count()
    }
    fn edge_count_hint(&self) -> usize {
        self.edge_count()
    }
}

// NOTE: `GraphQuery` deliberately does *not* impl `OrderedTraverse`.
// Directed graphs have no intrinsic order on neighbors. Tree-shaped
// views (e.g. `TreeSnapshotQuery`) will impl it using `OrderKey`.

// ============================================================================
// TRAVERSAL NODE
// ============================================================================

/// Traversal result that includes hierarchical metadata.
#[derive(Clone, Debug)]
pub struct TraversalNode {
    /// The entity being visited.
    pub entity: Entity,
    /// Depth from the start node (0 = start node).
    pub depth: usize,
    /// Parent entity in the traversal tree (None for start node).
    pub parent: Option<Entity>,
}

// ============================================================================
// BFS ITERATOR
// ============================================================================

pub struct BfsIter<'a, G: GraphTraverse> {
    graph: &'a G,
    queue: VecDeque<Entity>,
    visited: HashSet<Entity>,
}

impl<'a, G: GraphTraverse> BfsIter<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(start);
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
        }
    }
}

impl<'a, G: GraphTraverse> Iterator for BfsIter<'a, G> {
    type Item = Entity;
    fn next(&mut self) -> Option<Self::Item> {
        let current = self.queue.pop_front()?;
        for neighbor in self.graph.successors(current) {
            if self.visited.insert(neighbor) {
                self.queue.push_back(neighbor);
            }
        }
        Some(current)
    }
}

// ============================================================================
// DFS ITERATOR
// ============================================================================

pub struct DfsIter<'a, G: GraphTraverse> {
    graph: &'a G,
    stack: Vec<Entity>,
    visited: HashSet<Entity>,
}

impl<'a, G: GraphTraverse> DfsIter<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        Self {
            graph,
            stack: vec![start],
            visited: HashSet::new(),
        }
    }
}

impl<'a, G: GraphTraverse> Iterator for DfsIter<'a, G> {
    type Item = Entity;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(current) = self.stack.pop() {
            if self.visited.insert(current) {
                for neighbor in self.graph.successors(current) {
                    self.stack.push(neighbor);
                }
                return Some(current);
            }
        }
        None
    }
}

// ============================================================================
// BFS WITH DEPTH ITERATOR
// ============================================================================

/// BFS iterator that tracks depth and parent for each visited node.
pub struct BfsIterWithDepth<'a, G: GraphTraverse> {
    graph: &'a G,
    queue: VecDeque<(Entity, usize, Option<Entity>)>,
    visited: HashSet<Entity>,
}

impl<'a, G: GraphTraverse> BfsIterWithDepth<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back((start, 0, None));
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
        }
    }
}

impl<'a, G: GraphTraverse> Iterator for BfsIterWithDepth<'a, G> {
    type Item = TraversalNode;
    fn next(&mut self) -> Option<Self::Item> {
        let (current, depth, parent) = self.queue.pop_front()?;
        for neighbor in self.graph.successors(current) {
            if self.visited.insert(neighbor) {
                self.queue.push_back((neighbor, depth + 1, Some(current)));
            }
        }
        Some(TraversalNode {
            entity: current,
            depth,
            parent,
        })
    }
}

// ============================================================================
// DFS WITH DEPTH ITERATOR
// ============================================================================

/// DFS iterator that tracks depth and parent for each visited node.
pub struct DfsIterWithDepth<'a, G: GraphTraverse> {
    graph: &'a G,
    stack: Vec<(Entity, usize, Option<Entity>)>,
    visited: HashSet<Entity>,
}

impl<'a, G: GraphTraverse> DfsIterWithDepth<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        Self {
            graph,
            stack: vec![(start, 0, None)],
            visited: HashSet::new(),
        }
    }
}

impl<'a, G: GraphTraverse> Iterator for DfsIterWithDepth<'a, G> {
    type Item = TraversalNode;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some((current, depth, parent)) = self.stack.pop() {
            if self.visited.insert(current) {
                for neighbor in self.graph.successors(current) {
                    self.stack.push((neighbor, depth + 1, Some(current)));
                }
                return Some(TraversalNode {
                    entity: current,
                    depth,
                    parent,
                });
            }
        }
        None
    }
}

// ============================================================================
// EDGE-AWARE BFS ITERATOR
// ============================================================================

/// BFS iterator that yields `(edge_used_to_reach_node, node)` pairs.
/// The start node yields `(None, start)`; every other node yields
/// `(Some(edge), node)` where `edge` is the entity of the edge that
/// got the iterator to that node.
pub struct EdgeBfsIter<'a, G: GraphTraverseEdges> {
    graph: &'a G,
    queue: VecDeque<(Option<Entity>, Entity)>,
    visited: HashSet<Entity>,
}

impl<'a, G: GraphTraverseEdges> EdgeBfsIter<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back((None, start));
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
        }
    }
}

impl<'a, G: GraphTraverseEdges> Iterator for EdgeBfsIter<'a, G> {
    type Item = (Option<Entity>, Entity);
    fn next(&mut self) -> Option<Self::Item> {
        let (edge, current) = self.queue.pop_front()?;
        for (e, n) in self.graph.outgoing(current) {
            if self.visited.insert(n) {
                self.queue.push_back((Some(e), n));
            }
        }
        Some((edge, current))
    }
}

// ============================================================================
// EDGE-AWARE DFS ITERATOR
// ============================================================================

/// DFS analogue of [`EdgeBfsIter`].
pub struct EdgeDfsIter<'a, G: GraphTraverseEdges> {
    graph: &'a G,
    stack: Vec<(Option<Entity>, Entity)>,
    visited: HashSet<Entity>,
}

impl<'a, G: GraphTraverseEdges> EdgeDfsIter<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        Self {
            graph,
            stack: vec![(None, start)],
            visited: HashSet::new(),
        }
    }
}

impl<'a, G: GraphTraverseEdges> Iterator for EdgeDfsIter<'a, G> {
    type Item = (Option<Entity>, Entity);
    fn next(&mut self) -> Option<Self::Item> {
        while let Some((edge, current)) = self.stack.pop() {
            if self.visited.insert(current) {
                for (e, n) in self.graph.outgoing(current) {
                    self.stack.push((Some(e), n));
                }
                return Some((edge, current));
            }
        }
        None
    }
}

// ============================================================================
// ORDERED BFS ITERATOR
// ============================================================================

/// BFS iterator that expands neighbors via
/// [`OrderedTraverse::ordered_successors`]. Yields the same shape as
/// [`BfsIter`] — only the queueing order is implementor-defined.
pub struct OrderedBfsIter<'a, G: OrderedTraverse> {
    graph: &'a G,
    queue: VecDeque<Entity>,
    visited: HashSet<Entity>,
}

impl<'a, G: OrderedTraverse> OrderedBfsIter<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(start);
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
        }
    }
}

impl<'a, G: OrderedTraverse> Iterator for OrderedBfsIter<'a, G> {
    type Item = Entity;
    fn next(&mut self) -> Option<Self::Item> {
        let current = self.queue.pop_front()?;
        for n in self.graph.ordered_successors(current) {
            if self.visited.insert(n) {
                self.queue.push_back(n);
            }
        }
        Some(current)
    }
}

// ============================================================================
// ORDERED DFS ITERATOR
// ============================================================================

/// DFS iterator that expands neighbors via
/// [`OrderedTraverse::ordered_successors`]. Children are pushed onto
/// the stack in reverse order so the first listed child pops first —
/// i.e. leftmost child is visited first.
pub struct OrderedDfsIter<'a, G: OrderedTraverse> {
    graph: &'a G,
    stack: Vec<Entity>,
    visited: HashSet<Entity>,
}

impl<'a, G: OrderedTraverse> OrderedDfsIter<'a, G> {
    pub(crate) fn new(graph: &'a G, start: Entity) -> Self {
        Self {
            graph,
            stack: vec![start],
            visited: HashSet::new(),
        }
    }
}

impl<'a, G: OrderedTraverse> Iterator for OrderedDfsIter<'a, G> {
    type Item = Entity;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(current) = self.stack.pop() {
            if self.visited.insert(current) {
                // Push reversed so leftmost child pops first.
                let kids: Vec<Entity> = self.graph.ordered_successors(current).collect();
                for kid in kids.into_iter().rev() {
                    self.stack.push(kid);
                }
                return Some(current);
            }
        }
        None
    }
}

// ============================================================================
// CLOSURE-DRIVEN BFS WALK
// ============================================================================

/// BFS walk driven by a per-node [`Step`] policy. Returned by
/// [`GraphTraverse::bfs_walk`].
pub struct BfsWalk<'a, G: GraphTraverse, F> {
    graph: &'a G,
    queue: VecDeque<(Entity, usize, Option<Entity>)>,
    visited: HashSet<Entity>,
    policy: F,
    halted: bool,
}

impl<'a, G: GraphTraverse, F> BfsWalk<'a, G, F>
where
    F: FnMut(&TraversalNode) -> Step,
{
    pub(crate) fn new(graph: &'a G, start: Entity, policy: F) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back((start, 0, None));
        visited.insert(start);
        Self {
            graph,
            queue,
            visited,
            policy,
            halted: false,
        }
    }

    fn expand(&mut self, node: Entity, depth: usize) {
        for n in self.graph.successors(node) {
            if self.visited.insert(n) {
                self.queue.push_back((n, depth + 1, Some(node)));
            }
        }
    }
}

impl<'a, G: GraphTraverse, F> Iterator for BfsWalk<'a, G, F>
where
    F: FnMut(&TraversalNode) -> Step,
{
    type Item = TraversalNode;
    fn next(&mut self) -> Option<Self::Item> {
        if self.halted {
            return None;
        }
        loop {
            let (entity, depth, parent) = self.queue.pop_front()?;
            let node = TraversalNode { entity, depth, parent };
            match (self.policy)(&node) {
                Step::Visit => {
                    self.expand(entity, depth);
                    return Some(node);
                }
                Step::Skip => return Some(node),
                Step::Prune => continue,
                Step::Stop => {
                    self.halted = true;
                    return Some(node);
                }
            }
        }
    }
}

// ============================================================================
// CLOSURE-DRIVEN DFS WALK
// ============================================================================

// ============================================================================
// COST HINTS
// ============================================================================
//
// All iterators upper-bound their remaining work by `node_count - visited`
// items and `node_count + edge_count - visited` work units. Cheap to
// compute; correct as an upper bound; honest about how much of the graph
// the iterator can still touch.

fn traversal_cost<G: GraphNodes>(graph: &G, visited: usize) -> Cost {
    let n = graph.node_count_hint() as u64;
    let e = graph.edge_count_hint() as u64;
    let v = visited as u64;
    Cost {
        estimated_items: n.saturating_sub(v),
        estimated_work: n.saturating_add(e).saturating_sub(v),
    }
}

impl<G: GraphTraverse + GraphNodes> CostHint for BfsIter<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverse + GraphNodes> CostHint for DfsIter<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverse + GraphNodes> CostHint for BfsIterWithDepth<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverse + GraphNodes> CostHint for DfsIterWithDepth<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverseEdges + GraphNodes> CostHint for EdgeBfsIter<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverseEdges + GraphNodes> CostHint for EdgeDfsIter<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: OrderedTraverse + GraphNodes> CostHint for OrderedBfsIter<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: OrderedTraverse + GraphNodes> CostHint for OrderedDfsIter<'_, G> {
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverse + GraphNodes, F> CostHint for BfsWalk<'_, G, F>
where
    F: FnMut(&TraversalNode) -> Step,
{
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}
impl<G: GraphTraverse + GraphNodes, F> CostHint for DfsWalk<'_, G, F>
where
    F: FnMut(&TraversalNode) -> Step,
{
    fn cost(&self) -> Cost {
        traversal_cost(self.graph, self.visited.len())
    }
}

// ============================================================================
// CLOSURE-DRIVEN DFS WALK
// ============================================================================

/// DFS analogue of [`BfsWalk`].
pub struct DfsWalk<'a, G: GraphTraverse, F> {
    graph: &'a G,
    stack: Vec<(Entity, usize, Option<Entity>)>,
    visited: HashSet<Entity>,
    policy: F,
    halted: bool,
}

impl<'a, G: GraphTraverse, F> DfsWalk<'a, G, F>
where
    F: FnMut(&TraversalNode) -> Step,
{
    pub(crate) fn new(graph: &'a G, start: Entity, policy: F) -> Self {
        Self {
            graph,
            stack: vec![(start, 0, None)],
            visited: HashSet::new(),
            policy,
            halted: false,
        }
    }

    fn expand(&mut self, node: Entity, depth: usize) {
        for n in self.graph.successors(node) {
            if !self.visited.contains(&n) {
                self.stack.push((n, depth + 1, Some(node)));
            }
        }
    }
}

impl<'a, G: GraphTraverse, F> Iterator for DfsWalk<'a, G, F>
where
    F: FnMut(&TraversalNode) -> Step,
{
    type Item = TraversalNode;
    fn next(&mut self) -> Option<Self::Item> {
        if self.halted {
            return None;
        }
        while let Some((entity, depth, parent)) = self.stack.pop() {
            if !self.visited.insert(entity) {
                continue;
            }
            let node = TraversalNode { entity, depth, parent };
            match (self.policy)(&node) {
                Step::Visit => {
                    self.expand(entity, depth);
                    return Some(node);
                }
                Step::Skip => return Some(node),
                Step::Prune => continue,
                Step::Stop => {
                    self.halted = true;
                    return Some(node);
                }
            }
        }
        None
    }
}
