//! Subgraph patterns: a small declarative description of a shape to
//! search for in the graph.
//!
//! A [`Pattern`] is a tiny directed graph whose nodes carry boolean
//! predicates on `Entity` and whose edges optionally carry boolean
//! predicates on the edge entity. [`PatternBuilder`] is the ergonomic
//! constructor; [`Pattern::build`] precomputes a [`MatchPlan`] — the
//! order in which slots are bound during search — using a greedy
//! join-planning heuristic.
//!
//! The actual matching iterator lives in [`crate::subgraph`].
//!
//! # Predicates
//!
//! Node and edge predicates are plain closures `Fn(Entity) -> bool`.
//! Callers close over whatever they need to inspect components — typically
//! a separate `Query<…>` of their own. This matches the shape of
//! [`crate::queries::GraphQuery::find_paths_matching`].
//!
//! # Direction
//!
//! Each pattern edge is *directed*. To match a reverse-direction graph
//! edge, use [`Direction::Backward`]. To match an edge in either
//! direction, add two pattern edges (one in each direction) — but in
//! practice a graph edge always has a concrete direction, so this is
//! rarely needed.

use bevy::prelude::Entity;

// ============================================================================
// HANDLES
// ============================================================================

/// Identifier for a node slot in a pattern. Returned by
/// [`PatternBuilder::node`]; used to index into [`crate::subgraph::Match`].
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub struct PNode(pub usize);

/// Identifier for an edge slot in a pattern. Returned by
/// [`PatternBuilder::edge`] / [`PatternBuilder::edge_where`].
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub struct PEdge(pub usize);

/// Direction of a pattern edge.
///
/// - [`Direction::Forward`]: pattern edge `from -> to` matches a graph
///   edge whose `EdgeFrom` is the entity bound to `from` and whose
///   `EdgeTo` is the entity bound to `to`.
/// - [`Direction::Backward`]: same pattern edge matches a graph edge
///   whose `EdgeFrom` is bound to `to` and `EdgeTo` is bound to `from`.
///   I.e. the graph edge points opposite to the pattern arrow.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Direction {
    Forward,
    Backward,
}

// ============================================================================
// INTERNAL TYPES
// ============================================================================

pub(crate) struct PatternNodeDef<'p> {
    pub(crate) pred: Box<dyn Fn(Entity) -> bool + 'p>,
    pub(crate) anchor: Option<Entity>,
}

pub(crate) struct PatternEdgeDef<'p> {
    pub(crate) from: PNode,
    pub(crate) to: PNode,
    pub(crate) direction: Direction,
    pub(crate) pred: Option<Box<dyn Fn(Entity) -> bool + 'p>>,
}

// ============================================================================
// MATCH PLAN
// ============================================================================

/// Per-slot binding step computed at [`Pattern::build`] time.
///
/// `slot` is the pattern node being bound at this step. `join` describes
/// how to enumerate candidates: `None` means "no join edge yet — enumerate
/// over all graph nodes" (only legal for step 0). `Some(JoinSpec)` means
/// "candidates come from following a graph edge out of an already-bound
/// pattern neighbor". `constraints` lists additional pattern edges that
/// must be verified once `slot` is tentatively bound.
pub(crate) struct PlanStep {
    pub(crate) slot: PNode,
    pub(crate) join: Option<JoinSpec>,
    pub(crate) constraints: Vec<ConstraintEdge>,
}

#[derive(Clone, Copy)]
pub(crate) struct JoinSpec {
    /// Pattern edge used to enumerate candidates.
    pub(crate) edge: PEdge,
    /// The already-bound endpoint we expand from.
    pub(crate) anchor_slot: PNode,
    /// Whether to walk `outgoing` (Forward) or `incoming` (Backward)
    /// from the anchored entity. This is derived from the pattern edge's
    /// [`Direction`] and which endpoint is anchored.
    pub(crate) search: SearchDir,
}

#[derive(Clone, Copy)]
pub(crate) enum SearchDir {
    Outgoing,
    Incoming,
}

#[derive(Clone, Copy)]
pub(crate) struct ConstraintEdge {
    pub(crate) edge: PEdge,
    pub(crate) from_slot: PNode,
    pub(crate) to_slot: PNode,
    pub(crate) direction: Direction,
}

pub(crate) struct MatchPlan {
    pub(crate) steps: Vec<PlanStep>,
}

// ============================================================================
// PATTERN
// ============================================================================

/// A compiled subgraph pattern. Construct via [`PatternBuilder`].
pub struct Pattern<'p> {
    pub(crate) nodes: Vec<PatternNodeDef<'p>>,
    pub(crate) edges: Vec<PatternEdgeDef<'p>>,
    pub(crate) plan: MatchPlan,
}

impl<'p> Pattern<'p> {
    /// Number of node slots in this pattern.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edge slots in this pattern.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// True iff at least one pattern node has an anchor binding.
    pub fn is_anchored(&self) -> bool {
        self.nodes.iter().any(|n| n.anchor.is_some())
    }
}

// ============================================================================
// BUILDER
// ============================================================================

/// Build a [`Pattern`].
pub struct PatternBuilder<'p> {
    nodes: Vec<PatternNodeDef<'p>>,
    edges: Vec<PatternEdgeDef<'p>>,
}

impl<'p> Default for PatternBuilder<'p> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'p> PatternBuilder<'p> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Declare a new pattern node with a predicate on its eventual binding.
    pub fn node<F>(&mut self, pred: F) -> PNode
    where
        F: Fn(Entity) -> bool + 'p,
    {
        let id = PNode(self.nodes.len());
        self.nodes.push(PatternNodeDef {
            pred: Box::new(pred),
            anchor: None,
        });
        id
    }

    /// Pin pattern node `n` to a specific graph entity. The entity must
    /// still satisfy the node's predicate for the pattern to match.
    pub fn anchor(&mut self, n: PNode, entity: Entity) -> &mut Self {
        self.nodes[n.0].anchor = Some(entity);
        self
    }

    /// Declare a forward-direction edge `from -> to` with no edge predicate.
    pub fn edge(&mut self, from: PNode, to: PNode) -> PEdge {
        self.edge_dir(from, to, Direction::Forward)
    }

    /// Declare an edge with an explicit direction.
    pub fn edge_dir(&mut self, from: PNode, to: PNode, direction: Direction) -> PEdge {
        let id = PEdge(self.edges.len());
        self.edges.push(PatternEdgeDef {
            from,
            to,
            direction,
            pred: None,
        });
        id
    }

    /// Declare an edge with a predicate on its eventual edge-entity binding.
    pub fn edge_where<F>(&mut self, from: PNode, to: PNode, pred: F) -> PEdge
    where
        F: Fn(Entity) -> bool + 'p,
    {
        self.edge_where_dir(from, to, Direction::Forward, pred)
    }

    /// Declare an edge with explicit direction and an edge predicate.
    pub fn edge_where_dir<F>(
        &mut self,
        from: PNode,
        to: PNode,
        direction: Direction,
        pred: F,
    ) -> PEdge
    where
        F: Fn(Entity) -> bool + 'p,
    {
        let id = PEdge(self.edges.len());
        self.edges.push(PatternEdgeDef {
            from,
            to,
            direction,
            pred: Some(Box::new(pred)),
        });
        id
    }

    /// Finalize. Computes a [`MatchPlan`] greedily:
    ///
    /// 1. First slot: any anchored node (one candidate); else the node
    ///    with the most pattern-edges (most join constraints when bound).
    /// 2. Subsequent slots: the unbound node with the most pattern-edges
    ///    to already-bound slots. Ties broken by total pattern-degree.
    pub fn build(self) -> Pattern<'p> {
        let plan = compute_plan(&self.nodes, &self.edges);
        Pattern {
            nodes: self.nodes,
            edges: self.edges,
            plan,
        }
    }
}

// ============================================================================
// PLAN COMPUTATION
// ============================================================================

fn compute_plan(nodes: &[PatternNodeDef<'_>], edges: &[PatternEdgeDef<'_>]) -> MatchPlan {
    let n = nodes.len();
    let mut placed = vec![false; n];
    let mut order: Vec<PNode> = Vec::with_capacity(n);

    // Total pattern-degree per node (forward + backward, counted once per
    // edge — orientation doesn't matter for join planning).
    let total_degree: Vec<usize> = (0..n)
        .map(|i| {
            edges
                .iter()
                .filter(|e| e.from.0 == i || e.to.0 == i)
                .count()
        })
        .collect();

    // Step 0: anchored node if any (1 candidate), else max-degree node.
    let first = if let Some((i, _)) = nodes.iter().enumerate().find(|(_, nd)| nd.anchor.is_some()) {
        i
    } else {
        (0..n).max_by_key(|&i| total_degree[i]).unwrap_or(0)
    };
    order.push(PNode(first));
    placed[first] = true;

    // Subsequent steps: greedily pick the unplaced node with the most
    // pattern-edges to already-placed nodes.
    while order.len() < n {
        let mut best: Option<(usize, usize, usize)> = None; // (idx, joins, total_deg)
        for i in 0..n {
            if placed[i] {
                continue;
            }
            let joins = edges
                .iter()
                .filter(|e| {
                    (e.from.0 == i && placed[e.to.0]) || (e.to.0 == i && placed[e.from.0])
                })
                .count();
            // Only nodes with at least one join edge are valid candidates
            // (otherwise the pattern is disconnected; we still place them,
            // but prefer connected nodes first).
            let score = (joins, total_degree[i]);
            match best {
                None => best = Some((i, score.0, score.1)),
                Some((_, bj, btd)) if (score.0, score.1) > (bj, btd) => {
                    best = Some((i, score.0, score.1))
                }
                _ => {}
            }
        }
        let pick = best.expect("at least one unplaced node").0;
        order.push(PNode(pick));
        placed[pick] = true;
    }

    // Now turn the order into PlanSteps. For each step k>0, choose a
    // single join edge — the pattern edge connecting `slot` to its
    // earliest already-placed neighbor. Remaining edges to already-placed
    // neighbors become constraints to verify.
    let position: Vec<usize> = {
        let mut pos = vec![0usize; n];
        for (k, slot) in order.iter().enumerate() {
            pos[slot.0] = k;
        }
        pos
    };

    let mut steps: Vec<PlanStep> = Vec::with_capacity(n);
    for (k, &slot) in order.iter().enumerate() {
        if k == 0 {
            steps.push(PlanStep {
                slot,
                join: None,
                constraints: Vec::new(),
            });
            continue;
        }

        // Edges touching `slot` with the other endpoint already-placed
        // (i.e. position < k).
        let mut linking: Vec<(PEdge, PNode, Direction, bool)> = Vec::new();
        // tuple = (edge_id, anchor_slot, dir, slot_is_to)
        for (ei, e) in edges.iter().enumerate() {
            if e.from == slot && position[e.to.0] < k {
                linking.push((PEdge(ei), e.to, e.direction, false /* slot_is_from */));
            } else if e.to == slot && position[e.from.0] < k {
                linking.push((PEdge(ei), e.from, e.direction, true /* slot_is_to */));
            }
        }

        // Pick the join edge: prefer the one whose anchor was placed
        // earliest (gives the deepest pruning).
        linking.sort_by_key(|(_, anchor, _, _)| position[anchor.0]);
        let (join_edge, join_anchor, join_dir, slot_is_to) = linking[0];

        let search = match (join_dir, slot_is_to) {
            // Forward edge from anchor -> slot: walk outgoing from anchor.
            (Direction::Forward, true) => SearchDir::Outgoing,
            // Forward edge from slot -> anchor: walk incoming on anchor.
            (Direction::Forward, false) => SearchDir::Incoming,
            // Backward edge from anchor -> slot in pattern means the
            // graph edge points slot -> anchor: walk incoming on anchor.
            (Direction::Backward, true) => SearchDir::Incoming,
            // Backward edge from slot -> anchor: graph edge points
            // anchor -> slot: walk outgoing from anchor.
            (Direction::Backward, false) => SearchDir::Outgoing,
        };

        let join = JoinSpec {
            edge: join_edge,
            anchor_slot: join_anchor,
            search,
        };

        // Remaining linking edges become constraints.
        let constraints: Vec<ConstraintEdge> = linking[1..]
            .iter()
            .map(|&(pe, _, dir, _)| {
                let edef = &edges[pe.0];
                ConstraintEdge {
                    edge: pe,
                    from_slot: edef.from,
                    to_slot: edef.to,
                    direction: dir,
                }
            })
            .collect();

        steps.push(PlanStep {
            slot,
            join: Some(join),
            constraints,
        });
    }

    MatchPlan { steps }
}
