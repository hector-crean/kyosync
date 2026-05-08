//! WFC solver resource and Bevy systems.
//!
//! The solver provides two modes:
//!
//! 1. **Focused autocomplete** -- given an anchor node and its current state,
//!    enumerate all valid `(node_variant, edge_variant)` placements using port
//!    capacity, compatibility rules, and constraints.  This is the primary API
//!    consumed by interactive editors.
//!
//! 2. **Background solve** -- the gather/solve/scatter systems listen for
//!    topology changes and pre-compute suggestions for every open node in the
//!    graph.

use std::collections::HashMap;
use std::fmt::Debug;

use bevy::prelude::*;
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::Directed;

use crate::domain::{Catalog, EdgeLike, NodeLike, Port};
use crate::constraint::ConstraintSet;
use crate::solver::{GraphSolver, SolverSet};
use crate::wfc::compatibility::CompatibilityTable;
use crate::wfc::domain_set::DomainSet;
use crate::wfc::heuristic::{MinEntropyHeuristic, WfcHeuristic};
use crate::wfc::propagator::propagate_ac3;
use crate::{
    GraphMessage,
    queries::{GraphComponent, GraphQuery},
};

// ---------------------------------------------------------------------------
// WfcSuggestion component
// ---------------------------------------------------------------------------

/// Attached to a *ghost entity* to indicate that the WFC solver suggests
/// placing a particular node variant here, connected to `anchor` via the
/// given edge variant.
#[derive(Component, Clone, Debug)]
pub struct WfcSuggestion {
    /// Index into the catalog's `node_variants()`.
    pub node_variant_idx: usize,
    /// Index into the catalog's `edge_variants()`.
    pub edge_variant_idx: usize,
    /// The existing node entity that the suggestion would be connected to.
    pub anchor: Entity,
    /// Human-readable label (e.g. "C", "O").
    pub label: String,
    /// Human-readable edge label (e.g. "Single", "Double").
    pub edge_label: String,
}

// ---------------------------------------------------------------------------
// AnchorContext
// ---------------------------------------------------------------------------

/// Describes the current state of a node that the solver should compute
/// suggestions for.
///
/// The caller (usually example/app code) populates this by reading ECS
/// components and mapping to catalog indices.
pub struct AnchorContext {
    /// The ECS entity of the anchor node.
    pub entity: Entity,
    /// Index into `catalog.node_variants()` identifying what this node *is*.
    pub node_variant_idx: usize,
    /// Sum of `EdgeLike::order()` for all edges currently attached to this
    /// node.  Used together with port capacity to determine remaining slots.
    pub used_port_capacity: u8,
    /// The existing neighbours as `(edge_variant_idx, node_variant_idx)` pairs,
    /// used for constraint checking.
    pub existing_edges: Vec<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// SuggestionRecord
// ---------------------------------------------------------------------------

/// A pending suggestion not yet materialised in the ECS.
#[derive(Clone, Debug)]
pub struct SuggestionRecord {
    pub anchor_entity: Entity,
    pub node_variant_idx: usize,
    pub edge_variant_idx: usize,
    pub label: String,
    pub edge_label: String,
}

// ---------------------------------------------------------------------------
// WfcSolverState
// ---------------------------------------------------------------------------

/// Resource that drives the WFC autocomplete loop.
///
/// Generic over the catalog so it can work with any domain.
#[derive(Resource)]
pub struct WfcSolverState<C: Catalog> {
    pub catalog: C,
    pub compat: CompatibilityTable<C::Node, C::Edge>,
    pub constraints: ConstraintSet<C::Node, C::Edge>,
    pub heuristic: Box<dyn WfcHeuristic>,
    pub dirty: bool,
    pub enabled: bool,
    pub max_suggestions: usize,
    pub suggestions: Vec<SuggestionRecord>,
}

impl<C: Catalog> WfcSolverState<C> {
    pub fn new(catalog: C) -> Self {
        Self {
            catalog,
            compat: CompatibilityTable::new(),
            constraints: ConstraintSet::new(),
            heuristic: Box::new(MinEntropyHeuristic),
            dirty: false,
            enabled: true,
            max_suggestions: 64,
            suggestions: Vec::new(),
        }
    }

    pub fn with_compat(mut self, compat: CompatibilityTable<C::Node, C::Edge>) -> Self {
        self.compat = compat;
        self
    }

    pub fn with_constraints(mut self, constraints: ConstraintSet<C::Node, C::Edge>) -> Self {
        self.constraints = constraints;
        self
    }

    pub fn with_heuristic(mut self, heuristic: impl WfcHeuristic) -> Self {
        self.heuristic = Box::new(heuristic);
        self
    }

    // -----------------------------------------------------------------------
    // Focused autocomplete
    // -----------------------------------------------------------------------

    /// Compute **all** valid `(node_variant, edge_variant)` placements that
    /// could be attached to the given anchor, respecting:
    ///
    /// - Port capacity (total port slots minus used slots)
    /// - New-node minimum capacity (must support the proposed edge)
    /// - [`CompatibilityTable`] rules
    /// - [`ConstraintSet`] edge checks
    pub fn suggestions_for(&self, anchor: &AnchorContext) -> Vec<SuggestionRecord> {
        let node_variants = self.catalog.node_variants();
        let edge_variants = self.catalog.edge_variants();

        let Some(anchor_node) = node_variants.get(anchor.node_variant_idx) else {
            return vec![];
        };

        let anchor_total_capacity: u8 = anchor_node
            .ports()
            .iter()
            .map(|p| p.capacity() as u8)
            .sum();
        let remaining = anchor_total_capacity.saturating_sub(anchor.used_port_capacity);
        if remaining == 0 {
            return vec![];
        }

        let mut out = Vec::new();

        for (ni, new_node) in node_variants.iter().enumerate() {
            let new_total_capacity: u8 = new_node
                .ports()
                .iter()
                .map(|p| p.capacity() as u8)
                .sum();

            for (ei, edge) in edge_variants.iter().enumerate() {
                let order = edge.order();

                if order > remaining {
                    continue;
                }
                if order > new_total_capacity {
                    continue;
                }
                if !self.compat.is_compatible(anchor_node, edge, new_node) {
                    continue;
                }
                if !self.constraints.is_edge_valid(edge, anchor_node, new_node) {
                    continue;
                }

                out.push(SuggestionRecord {
                    anchor_entity: anchor.entity,
                    node_variant_idx: ni,
                    edge_variant_idx: ei,
                    label: new_node.symbol().to_string(),
                    edge_label: format!("{:?}", edge),
                });
            }
        }

        out
    }
}

impl<C: Catalog> GraphSolver for WfcSolverState<C> {
    fn needs_solve(&self) -> bool {
        self.dirty && self.enabled
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn clear_dirty(&mut self) {
        self.dirty = false;
    }
}

// ---------------------------------------------------------------------------
// Bevy systems (background solve -- optional)
// ---------------------------------------------------------------------------

/// Gather: mark solver dirty when graph topology changes.
pub fn wfc_gather<C: Catalog>(
    mut solver: ResMut<WfcSolverState<C>>,
    mut reader: MessageReader<GraphMessage>,
) {
    for msg in reader.read() {
        match msg {
            GraphMessage::NodeAdded { .. }
            | GraphMessage::NodeRemoved { .. }
            | GraphMessage::EdgeAdded { .. }
            | GraphMessage::EdgeRemoved { .. }
            | GraphMessage::NodeConnected { .. }
            | GraphMessage::NodeDisconnected { .. } => {
                solver.mark_dirty();
            }
            _ => {}
        }
    }
}

/// Background solve: compute one suggestion per open node via AC-3.
///
/// This is a coarse pass.  For interactive autocomplete prefer calling
/// [`WfcSolverState::suggestions_for`] with a specific [`AnchorContext`].
pub fn wfc_solve<C, Node, Edge>(
    mut solver: ResMut<WfcSolverState<C>>,
    q: GraphQuery<'_, '_, Node, Edge>,
) where
    C: Catalog,
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    if !solver.needs_solve() {
        return;
    }
    solver.suggestions.clear();

    let node_variants: Vec<_> = solver.catalog.node_variants().to_vec();
    let edge_variants: Vec<_> = solver.catalog.edge_variants().to_vec();

    if node_variants.is_empty() || edge_variants.is_empty() {
        solver.clear_dirty();
        return;
    }

    // Build a working `StableGraph` from ECS topology. This replaces the
    // old `graph.backend().0` mirror access — same logical graph, ECS as
    // source. The AC-3 propagation operates on the working graph
    // unchanged.
    let mut ac3_graph: StableGraph<(), (), Directed> = StableGraph::new();
    let mut entity_to_ac3: HashMap<Entity, NodeIndex> = HashMap::new();
    let mut ac3_to_entity: HashMap<NodeIndex, Entity> = HashMap::new();
    for (entity, _, _, _) in q.nodes_iter() {
        let ac3_ni = ac3_graph.add_node(());
        entity_to_ac3.insert(entity, ac3_ni);
        ac3_to_entity.insert(ac3_ni, entity);
    }
    for (_, edge_from, edge_to, _) in q.edges_iter() {
        if let (Some(&a), Some(&b)) = (
            entity_to_ac3.get(&edge_from.0),
            entity_to_ac3.get(&edge_to.0),
        ) {
            ac3_graph.add_edge(a, b, ());
        }
    }

    let num_n = node_variants.len();
    let num_e = edge_variants.len();
    let mut domains: Vec<DomainSet> = Vec::new();
    let mut slot_of_node: HashMap<NodeIndex, usize> = HashMap::new();
    let mut node_of_slot: Vec<NodeIndex> = Vec::new();

    for &ac3_ni in entity_to_ac3.values() {
        let slot = domains.len();
        slot_of_node.insert(ac3_ni, slot);
        node_of_slot.push(ac3_ni);
        domains.push(DomainSet::full(num_n, num_e));
    }

    let dirty_ac3: Vec<NodeIndex> = entity_to_ac3.values().copied().collect();
    let _ = propagate_ac3(
        &ac3_graph,
        &mut domains,
        &slot_of_node,
        &node_of_slot,
        &dirty_ac3,
        &node_variants,
        &edge_variants,
        &solver.compat,
    );

    let mut rng = rand::thread_rng();
    let max = solver.max_suggestions;
    let mut count = 0usize;
    let node_weights: Vec<f32> = node_variants.iter().map(|n| n.weight()).collect();

    for (slot_idx, domain) in domains.iter().enumerate() {
        if count >= max {
            break;
        }
        if domain.len() <= 1 {
            continue;
        }
        let ac3_node = node_of_slot[slot_idx];
        let Some(&entity) = ac3_to_entity.get(&ac3_node) else {
            continue;
        };

        if let Some(candidate) =
            solver
                .heuristic
                .select_candidate(domain, &node_weights, &mut rng)
        {
            let label = node_variants
                .get(candidate.node_idx)
                .map(|n| n.symbol().to_string())
                .unwrap_or_default();
            let edge_label = edge_variants
                .get(candidate.edge_idx)
                .map(|e| format!("{:?}", e))
                .unwrap_or_default();
            solver.suggestions.push(SuggestionRecord {
                anchor_entity: entity,
                node_variant_idx: candidate.node_idx,
                edge_variant_idx: candidate.edge_idx,
                label,
                edge_label,
            });
            count += 1;
        }
    }

    solver.clear_dirty();
}

/// Scatter: write suggestions into the ECS as `WfcSuggestion` components on
/// ghost entities.
pub fn wfc_scatter<C: Catalog>(
    mut commands: Commands,
    solver: Res<WfcSolverState<C>>,
    existing: Query<Entity, With<WfcSuggestion>>,
) {
    for e in existing.iter() {
        commands.entity(e).despawn();
    }

    for rec in &solver.suggestions {
        commands.spawn(WfcSuggestion {
            node_variant_idx: rec.node_variant_idx,
            edge_variant_idx: rec.edge_variant_idx,
            anchor: rec.anchor_entity,
            label: rec.label.clone(),
            edge_label: rec.edge_label.clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Adds the WFC solver systems to the graph pipeline.
///
/// You must insert a [`WfcSolverState<C>`] resource yourself (since it
/// carries your domain catalog and rules).
pub struct WfcPlugin<C, Node, Edge>
where
    C: Catalog,
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    _phantom: std::marker::PhantomData<(C, Node, Edge)>,
}

impl<C, Node, Edge> Default for WfcPlugin<C, Node, Edge>
where
    C: Catalog,
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<C, Node, Edge> Plugin for WfcPlugin<C, Node, Edge>
where
    C: Catalog,
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                wfc_gather::<C>,
                wfc_solve::<C, Node, Edge>,
                wfc_scatter::<C>,
            )
                .chain()
                .in_set(SolverSet),
        );
    }
}
