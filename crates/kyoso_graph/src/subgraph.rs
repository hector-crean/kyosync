//! Streaming subgraph isomorphism: find all bindings of a [`Pattern`]
//! into a graph.
//!
//! The search is a classic VF2-style backtrack — one frame per pattern
//! slot, each frame carrying an iterator over candidate graph entities.
//! When a complete mapping is found, it's yielded; the iterator then
//! resumes the search where it left off.
//!
//! Cost is computed once at construction (see [`CostHint`]) using only
//! topology counts — no per-archetype statistics. The estimate is a
//! worst-case upper bound: pattern node count, graph node count, and
//! average degree. That's enough for an agent to compare alternatives.
//!
//! # Semantics
//!
//! - **Injective on nodes**: no two pattern slots bind the same graph
//!   entity.
//! - **Injective on edges**: no two pattern edges bind the same graph
//!   edge entity. (Parallel graph edges between the same node pair are
//!   resolved by taking the first-found one for a given constraint.)
//! - **Directed**: pattern edges have a [`Direction`]; matching honors
//!   it.

use bevy::prelude::Entity;
use std::collections::HashSet;

use crate::cost::{Cost, CostHint};
use crate::pattern::{Direction, PEdge, PNode, Pattern, SearchDir};
use crate::traverse::{GraphNodes, GraphTraverseEdges};

// ============================================================================
// MATCH
// ============================================================================

/// A complete binding of a [`Pattern`] into the graph.
///
/// Indices match the order in which [`PatternBuilder::node`] /
/// [`PatternBuilder::edge`] returned handles.
#[derive(Clone, Debug)]
pub struct Match {
    /// Graph entity bound to each pattern node, indexed by `PNode.0`.
    pub nodes: Vec<Entity>,
    /// Graph edge entity bound to each pattern edge, indexed by `PEdge.0`.
    pub edges: Vec<Entity>,
}

impl Match {
    pub fn node(&self, n: PNode) -> Entity {
        self.nodes[n.0]
    }
    pub fn edge(&self, e: PEdge) -> Entity {
        self.edges[e.0]
    }
}

// ============================================================================
// ITERATOR
// ============================================================================

/// Streaming subgraph-isomorphism iterator. Construct via
/// [`GraphTraverseEdges::subgraph_matches`].
pub struct SubgraphMatches<'a, 'p, G: GraphTraverseEdges + GraphNodes> {
    graph: &'a G,
    pattern: &'p Pattern<'p>,
    stack: Vec<Frame<'a>>,
    mapping: Vec<Option<Entity>>,
    edge_mapping: Vec<Option<Entity>>,
    used_nodes: HashSet<Entity>,
    used_edges: HashSet<Entity>,
    estimated_cost: Cost,
    done: bool,
}

struct Frame<'a> {
    step_idx: usize,
    iter: Box<dyn Iterator<Item = (Option<Entity>, Entity)> + 'a>,
    /// Bindings committed by the most recent successful candidate at
    /// this frame. Unbound when we advance past that candidate.
    committed: Option<FrameBindings>,
}

struct FrameBindings {
    slot_node: Entity,
    join_edge: Option<Entity>,
    constraint_edges: Vec<Entity>,
}

impl<'a, 'p, G: GraphTraverseEdges + GraphNodes> SubgraphMatches<'a, 'p, G> {
    pub(crate) fn new(graph: &'a G, pattern: &'p Pattern<'p>) -> Self {
        let n = pattern.node_count();
        let e = pattern.edge_count();
        let estimated_cost = estimate_cost(graph, pattern);

        let mut this = Self {
            graph,
            pattern,
            stack: Vec::with_capacity(n),
            mapping: vec![None; n],
            edge_mapping: vec![None; e],
            used_nodes: HashSet::new(),
            used_edges: HashSet::new(),
            estimated_cost,
            done: n == 0,
        };
        if !this.done {
            let iter = this.make_step_iter(0);
            this.stack.push(Frame {
                step_idx: 0,
                iter,
                committed: None,
            });
        }
        this
    }

    fn make_step_iter(&self, step_idx: usize) -> Box<dyn Iterator<Item = (Option<Entity>, Entity)> + 'a> {
        let step = &self.pattern.plan.steps[step_idx];
        match &step.join {
            None => {
                // Step 0: anchor (one candidate) or all nodes.
                if let Some(anchor) = self.pattern.nodes[step.slot.0].anchor {
                    Box::new(std::iter::once((None, anchor)))
                } else {
                    Box::new(self.graph.nodes().map(|n| (None, n)))
                }
            }
            Some(join) => {
                let anchor_entity = self.mapping[join.anchor_slot.0]
                    .expect("anchor slot must be bound before this step");
                match join.search {
                    SearchDir::Outgoing => Box::new(
                        self.graph
                            .outgoing(anchor_entity)
                            .map(|(e, n)| (Some(e), n)),
                    ),
                    SearchDir::Incoming => Box::new(
                        self.graph
                            .incoming(anchor_entity)
                            .map(|(e, n)| (Some(e), n)),
                    ),
                }
            }
        }
    }

    fn unbind_top(&mut self) {
        let frame = self.stack.last_mut().expect("stack non-empty");
        let b = match frame.committed.take() {
            Some(b) => b,
            None => return,
        };
        let step = &self.pattern.plan.steps[frame.step_idx];
        self.used_nodes.remove(&b.slot_node);
        self.mapping[step.slot.0] = None;
        if let (Some(je), Some(j)) = (b.join_edge, step.join.as_ref()) {
            self.used_edges.remove(&je);
            self.edge_mapping[j.edge.0] = None;
        }
        for (i, c) in step.constraints.iter().enumerate() {
            let ge = b.constraint_edges[i];
            self.used_edges.remove(&ge);
            self.edge_mapping[c.edge.0] = None;
        }
    }

    /// Try to commit (`edge_opt`, `node`) at `step_idx`. Returns `true`
    /// on success; the bindings live on the topmost frame.
    fn try_commit(&mut self, step_idx: usize, edge_opt: Option<Entity>, node: Entity) -> bool {
        let step = &self.pattern.plan.steps[step_idx];

        // 1. Node injectivity.
        if self.used_nodes.contains(&node) {
            return false;
        }

        // 2. Node predicate.
        if !(self.pattern.nodes[step.slot.0].pred)(node) {
            return false;
        }

        // 3. Join-edge checks.
        if let (Some(e), Some(j)) = (edge_opt, step.join.as_ref()) {
            if self.used_edges.contains(&e) {
                return false;
            }
            if let Some(p) = &self.pattern.edges[j.edge.0].pred {
                if !p(e) {
                    return false;
                }
            }
        }

        // 4. Constraint edges. Tentatively bind the node first so
        //    constraint lookups can resolve via mapping.
        self.mapping[step.slot.0] = Some(node);

        let mut constraint_edges: Vec<Entity> = Vec::with_capacity(step.constraints.len());
        for c in &step.constraints {
            let from_e = self.mapping[c.from_slot.0]
                .expect("constraint endpoint must already be bound");
            let to_e = self.mapping[c.to_slot.0]
                .expect("constraint endpoint must already be bound");
            let edge_e = match find_edge_dir(self.graph, from_e, to_e, c.direction) {
                Some(e) => e,
                None => {
                    self.mapping[step.slot.0] = None;
                    return false;
                }
            };
            if self.used_edges.contains(&edge_e) || constraint_edges.contains(&edge_e) {
                self.mapping[step.slot.0] = None;
                return false;
            }
            if let Some(p) = &self.pattern.edges[c.edge.0].pred {
                if !p(edge_e) {
                    self.mapping[step.slot.0] = None;
                    return false;
                }
            }
            constraint_edges.push(edge_e);
        }

        // 5. Commit.
        self.used_nodes.insert(node);
        let join_edge = match (edge_opt, step.join.as_ref()) {
            (Some(e), Some(j)) => {
                self.used_edges.insert(e);
                self.edge_mapping[j.edge.0] = Some(e);
                Some(e)
            }
            _ => None,
        };
        for (i, c) in step.constraints.iter().enumerate() {
            self.used_edges.insert(constraint_edges[i]);
            self.edge_mapping[c.edge.0] = Some(constraint_edges[i]);
        }

        let frame = self.stack.last_mut().expect("stack non-empty");
        frame.committed = Some(FrameBindings {
            slot_node: node,
            join_edge,
            constraint_edges,
        });
        true
    }

    fn current_match(&self) -> Match {
        Match {
            nodes: self.mapping.iter().map(|x| x.expect("complete")).collect(),
            edges: self
                .edge_mapping
                .iter()
                .map(|x| x.expect("complete"))
                .collect(),
        }
    }
}

impl<'a, 'p, G: GraphTraverseEdges + GraphNodes> Iterator for SubgraphMatches<'a, 'p, G> {
    type Item = Match;

    fn next(&mut self) -> Option<Match> {
        if self.done {
            return None;
        }
        let total_steps = self.pattern.plan.steps.len();
        loop {
            // 1. Unbind whatever the topmost frame previously committed
            //    so we can advance to the next candidate.
            self.unbind_top();

            // 2. Pull next candidate from the topmost frame.
            let (edge_opt, node) = {
                let frame = self.stack.last_mut().expect("stack non-empty");
                match frame.iter.next() {
                    Some(x) => x,
                    None => {
                        // Exhausted; pop and continue at parent.
                        self.stack.pop();
                        if self.stack.is_empty() {
                            self.done = true;
                            return None;
                        }
                        continue;
                    }
                }
            };

            // 3. Try to bind.
            let step_idx = self.stack.last().unwrap().step_idx;
            if !self.try_commit(step_idx, edge_opt, node) {
                continue;
            }

            // 4. If pattern complete, emit. Otherwise descend.
            if step_idx + 1 == total_steps {
                return Some(self.current_match());
            } else {
                let iter = self.make_step_iter(step_idx + 1);
                self.stack.push(Frame {
                    step_idx: step_idx + 1,
                    iter,
                    committed: None,
                });
            }
        }
    }
}

impl<'a, 'p, G: GraphTraverseEdges + GraphNodes> CostHint for SubgraphMatches<'a, 'p, G> {
    fn cost(&self) -> Cost {
        self.estimated_cost
    }
}

// ============================================================================
// HELPERS
// ============================================================================

fn find_edge_dir<G: GraphTraverseEdges>(
    graph: &G,
    from: Entity,
    to: Entity,
    direction: Direction,
) -> Option<Entity> {
    let (a, b) = match direction {
        Direction::Forward => (from, to),
        Direction::Backward => (to, from),
    };
    graph.outgoing(a).find_map(|(e, n)| (n == b).then_some(e))
}

#[cfg(test)]
#[path = "subgraph_tests.rs"]
mod subgraph_tests;

fn estimate_cost<G: GraphTraverseEdges + GraphNodes>(graph: &G, pattern: &Pattern<'_>) -> Cost {
    let n_nodes = graph.node_count_hint() as u64;
    let n_edges = graph.edge_count_hint() as u64;
    let avg_deg = if n_nodes == 0 { 0 } else { n_edges / n_nodes.max(1) };
    // Bound by 8 to keep estimates from saturating instantly on dense
    // graphs while still being a meaningful comparison signal.
    let branching = avg_deg.max(1).min(64);

    let mut items: u64 = if pattern.is_anchored() { 1 } else { n_nodes };
    let mut work: u64 = items;
    for _ in 1..pattern.node_count() {
        items = items.saturating_mul(branching);
        work = work.saturating_add(items);
    }
    Cost {
        estimated_items: items,
        estimated_work: work,
    }
}

