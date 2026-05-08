//! Solver framework for graph constraint systems.
//!
//! This module provides infrastructure for plugging constraint solvers into
//! the graph pipeline. It maintains a lightweight [`SolverGraph`] that mirrors
//! the ECS graph topology, supports synchronous and asynchronous execution
//! via [`SolveBackend`], and manages in-flight computation through [`SolveJob`].
//!
//! Solvers run in [`GraphSystemSet::Solving`], after event propagation and
//! before graph sync.
//!
//! # Architecture
//!
//! ```text
//! ECS World ──(sync systems)──▶ SolverGraph ──(snapshot)──▶ async task
//!                                                               │
//!     ECS World ◀──(scatter)── SolveResult ◀────────────────────┘
//! ```
//!
//! The solver graph is a topology-only [`StableGraph`] that tracks which ECS
//! entities are nodes and edges. It uses stable indices so removals never
//! invalidate other entries. For async work, call [`SolverGraph::snapshot`]
//! to get a cheap [`GraphSnapshot`] that is `Send + Sync`.
//!
//! # Usage
//!
//! 1. Add [`GraphSolverPlugin`] to your app (alongside
//!    [`GraphManagerPlugin`](super::GraphManagerPlugin))
//! 2. Implement your solver as Bevy systems following the gather / solve /
//!    scatter pattern
//! 3. Schedule them in [`SolverSet`]
//!
//! ```ignore
//! fn gather(mut solver: ResMut<MySolver>, messages: MessageReader<GraphMessage>) {
//!     for msg in messages.read() {
//!         if matches!(msg, GraphMessage::NodeChanged { .. }) {
//!             solver.mark_dirty();
//!         }
//!     }
//! }
//!
//! fn solve(
//!     mut solver: ResMut<MySolver>,
//!     config: Res<SolverConfig>,
//!     graph: Res<SolverGraph>,
//!     index: Res<SolverEntityIndex>,
//!     mut job: ResMut<SolveJob>,
//! ) {
//!     if !config.enabled || !solver.needs_solve() { return; }
//!     let snapshot = graph.snapshot(&index);
//!     job.start(snapshot, |snap| {
//!         // ... heavy computation on the snapshot ...
//!         Ok(SolveResult { changed: true, iterations: 42 })
//!     });
//!     solver.clear_dirty();
//! }
//!
//! fn scatter(mut job: ResMut<SolveJob>, mut commands: Commands) {
//!     if let Some(Ok(result)) = job.poll() {
//!         if result.changed {
//!             // write results back to ECS
//!         }
//!     }
//! }
//!
//! app.add_systems(Update, (gather, solve, scatter).chain().in_set(SolverSet));
//! ```
//!
//! # Feedback Loop Prevention
//!
//! Solvers should set
//! [`CurrentChangeSource`](super::transaction::CurrentChangeSource) to
//! [`ChangeSource::Solver`](super::transaction::ChangeSource::Solver) before
//! writing ECS mutations. Change detection stamps the active source into
//! [`NodeChangeSet`](super::NodeChangeSet) /
//! [`EdgeChangeSet`](super::EdgeChangeSet), so downstream propagation
//! systems can filter solver-originated changes and avoid re-triggering
//! the same solver.

use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::stable_graph::StableGraph;
use petgraph::visit::EdgeRef;
use petgraph::Directed;
use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::PhantomData;

use super::components::{EdgeFrom, EdgeTo};
use super::queries::GraphComponent;
use super::GraphSystemSet;

// ============================================================================
// Error Types
// ============================================================================

/// Errors that may occur during graph solving.
#[derive(Debug)]
pub enum GraphSolverError {
    NoValidStates(NodeIndex),
    PropagationFailed(String),
    IncompleteSolve(NodeIndex),
    InvalidState(String),
    NodeNotFound(NodeIndex),
    NoSolution,
    Timeout(usize),
}

impl std::fmt::Display for GraphSolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoValidStates(ni) => write!(f, "no valid states for node {ni:?}"),
            Self::PropagationFailed(msg) => write!(f, "propagation failed: {msg}"),
            Self::IncompleteSolve(ni) => write!(f, "incomplete solve for node {ni:?}"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::NodeNotFound(ni) => write!(f, "node not found: {ni:?}"),
            Self::NoSolution => write!(f, "no solution found"),
            Self::Timeout(iters) => write!(f, "solver timeout after {iters} iterations"),
        }
    }
}

impl std::error::Error for GraphSolverError {}

// ============================================================================
// Solve Backend
// ============================================================================

/// Execution strategy for the solver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
pub enum SolveBackend {
    /// Run the solver synchronously on the calling thread.
    CpuSync,
    /// Spawn the solver on the [`AsyncComputeTaskPool`].
    #[default]
    CpuAsync,
}

// ============================================================================
// Graph Snapshot
// ============================================================================

/// A lightweight, `Send + Sync` snapshot of the solver graph topology.
///
/// Created via [`SolverGraph::snapshot`] and passed into async solve tasks.
/// Contains only connectivity plus the entity mapping so results can be
/// written back to the ECS.
#[derive(Clone, Debug, Default)]
pub struct GraphSnapshot {
    pub graph: StableGraph<(), (), Directed>,
    pub node_entities: HashMap<NodeIndex, Entity>,
    pub edge_entities: HashMap<EdgeIndex, Entity>,
}

// ============================================================================
// Solve Result
// ============================================================================

/// Output of a solver computation.
#[derive(Clone, Debug, Default)]
pub struct SolveResult {
    pub changed: bool,
    pub iterations: usize,
}

// ============================================================================
// Solver Trait
// ============================================================================

/// Trait for solver resources that track their own dirty state.
///
/// Implement this on your solver resource to integrate with the graph
/// pipeline. The canonical pattern is:
///
/// ```ignore
/// #[derive(Resource)]
/// struct MyLayoutSolver { dirty: bool }
///
/// impl GraphSolver for MyLayoutSolver {
///     fn needs_solve(&self) -> bool { self.dirty }
///     fn mark_dirty(&mut self) { self.dirty = true; }
///     fn clear_dirty(&mut self) { self.dirty = false; }
/// }
/// ```
pub trait GraphSolver: Resource {
    /// Whether the solver has pending work.
    fn needs_solve(&self) -> bool;

    /// Mark the solver as needing re-evaluation.
    fn mark_dirty(&mut self);

    /// Clear the dirty flag (call after solving).
    fn clear_dirty(&mut self);
}

// ============================================================================
// Solver Graph
// ============================================================================

/// Topology-only mirror of the ECS graph, maintained for solver use.
///
/// Uses [`StableGraph`] so that node/edge removal never invalidates indices
/// held elsewhere. Stores unit weights — solvers that need richer state
/// should maintain their own side tables keyed by [`NodeIndex`] /
/// [`EdgeIndex`].
#[derive(Clone, Resource, Default)]
pub struct SolverGraph(pub StableGraph<(), (), Directed>);

impl std::ops::Deref for SolverGraph {
    type Target = StableGraph<(), (), Directed>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SolverGraph {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl SolverGraph {
    pub fn new() -> Self {
        Self(StableGraph::new())
    }

    /// Create a cloneable snapshot suitable for off-thread computation.
    pub fn snapshot(&self, index: &SolverEntityIndex) -> GraphSnapshot {
        GraphSnapshot {
            graph: self.0.clone(),
            node_entities: index.entity_of_node.clone(),
            edge_entities: index.entity_of_edge.clone(),
        }
    }
}

// ============================================================================
// Solver Entity Index
// ============================================================================

/// Bidirectional mapping between ECS entities and solver graph indices.
#[derive(Resource, Default)]
pub struct SolverEntityIndex {
    pub node_of_entity: HashMap<Entity, NodeIndex>,
    pub entity_of_node: HashMap<NodeIndex, Entity>,
    pub edge_of_entity: HashMap<Entity, EdgeIndex>,
    pub entity_of_edge: HashMap<EdgeIndex, Entity>,
}

impl SolverEntityIndex {
    pub fn node_index(&self, entity: Entity) -> Option<NodeIndex> {
        self.node_of_entity.get(&entity).copied()
    }

    pub fn entity_for_node(&self, index: NodeIndex) -> Option<Entity> {
        self.entity_of_node.get(&index).copied()
    }

    pub fn edge_index(&self, entity: Entity) -> Option<EdgeIndex> {
        self.edge_of_entity.get(&entity).copied()
    }

    pub fn entity_for_edge(&self, index: EdgeIndex) -> Option<Entity> {
        self.entity_of_edge.get(&index).copied()
    }
}

// ============================================================================
// Solve Job
// ============================================================================

/// Manages in-flight solver computation (synchronous or asynchronous).
///
/// Use [`start`](SolveJob::start) to kick off a computation and
/// [`poll`](SolveJob::poll) each frame to check for results.
#[derive(Resource)]
pub struct SolveJob {
    task: Option<Task<Result<SolveResult, GraphSolverError>>>,
    last_result: Option<Result<SolveResult, GraphSolverError>>,
    backend: SolveBackend,
}

impl Default for SolveJob {
    fn default() -> Self {
        Self {
            task: None,
            last_result: None,
            backend: SolveBackend::default(),
        }
    }
}

impl SolveJob {
    /// Kick off a solve computation.
    ///
    /// For [`SolveBackend::CpuSync`] the closure runs immediately on the
    /// calling thread. For [`SolveBackend::CpuAsync`] it is spawned on the
    /// [`AsyncComputeTaskPool`].
    ///
    /// Returns `false` if an async job is already in flight.
    pub fn start(
        &mut self,
        snapshot: GraphSnapshot,
        solve_fn: impl FnOnce(GraphSnapshot) -> Result<SolveResult, GraphSolverError>
            + Send
            + Sync
            + 'static,
    ) -> bool {
        match self.backend {
            SolveBackend::CpuSync => {
                self.last_result = Some(solve_fn(snapshot));
                true
            }
            SolveBackend::CpuAsync => {
                if self.task.is_some() {
                    return false;
                }
                let task =
                    AsyncComputeTaskPool::get().spawn(async move { solve_fn(snapshot) });
                self.task = Some(task);
                true
            }
        }
    }

    /// Non-blocking poll for a completed result.
    ///
    /// Returns `Some` when a result is available (either from a sync run or
    /// a finished async task), `None` if no result is ready yet.
    pub fn poll(&mut self) -> Option<Result<SolveResult, GraphSolverError>> {
        if let Some(result) = self.last_result.take() {
            return Some(result);
        }
        if let Some(task) = self.task.as_mut() {
            if let Some(result) = block_on(poll_once(task)) {
                self.task = None;
                return Some(result);
            }
        }
        None
    }

    /// Whether an async task is currently running.
    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }

    pub fn set_backend(&mut self, backend: SolveBackend) {
        self.backend = backend;
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for solver behavior.
#[derive(Resource, Clone, Debug, Reflect)]
pub struct SolverConfig {
    /// Global enable/disable for all solvers.
    pub enabled: bool,
    /// Maximum solver iterations per frame (for iterative solvers).
    pub max_iterations_per_frame: usize,
    /// Convergence tolerance for iterative solvers.
    pub tolerance: f64,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_iterations_per_frame: 100,
            tolerance: 1e-6,
        }
    }
}

// ============================================================================
// System Sets
// ============================================================================

/// System set for user-defined solver systems.
///
/// Schedule your solver systems in this set to have them run in
/// [`GraphSystemSet::Solving`], between event propagation and graph sync.
/// The solver graph is guaranteed to be up-to-date when systems in this
/// set execute.
///
/// ```ignore
/// app.add_systems(Update, (gather, solve, scatter).chain().in_set(SolverSet));
/// ```
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub struct SolverSet;

/// Internal set for syncing ECS → [`SolverGraph`] before user solvers run.
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
struct SolverSyncSet;

// ============================================================================
// Graph Sync Helpers
// ============================================================================

fn register_solver_node(
    graph: &mut SolverGraph,
    index: &mut SolverEntityIndex,
    entity: Entity,
) -> Option<NodeIndex> {
    if index.node_of_entity.contains_key(&entity) {
        return None;
    }
    let ni = graph.add_node(());
    index.node_of_entity.insert(entity, ni);
    index.entity_of_node.insert(ni, entity);
    Some(ni)
}

fn unregister_solver_node(graph: &mut SolverGraph, index: &mut SolverEntityIndex, entity: Entity) {
    use petgraph::Direction;
    if let Some(ni) = index.node_of_entity.remove(&entity) {
        index.entity_of_node.remove(&ni);

        for edge_ref in graph
            .edges_directed(ni, Direction::Outgoing)
            .collect::<Vec<_>>()
        {
            let eid = edge_ref.id();
            if let Some(ent) = index.entity_of_edge.remove(&eid) {
                index.edge_of_entity.remove(&ent);
            }
        }
        for edge_ref in graph
            .edges_directed(ni, Direction::Incoming)
            .collect::<Vec<_>>()
        {
            let eid = edge_ref.id();
            if let Some(ent) = index.entity_of_edge.remove(&eid) {
                index.edge_of_entity.remove(&ent);
            }
        }
        let _ = graph.remove_node(ni);
    }
}

fn sync_solver_edge(
    graph: &mut SolverGraph,
    index: &mut SolverEntityIndex,
    edge_entity: Entity,
    from_entity: Entity,
    to_entity: Entity,
) {
    if let Some(old_ei) = index.edge_of_entity.remove(&edge_entity) {
        index.entity_of_edge.remove(&old_ei);
        let _ = graph.remove_edge(old_ei);
    }

    let Some(a) = index.node_of_entity.get(&from_entity).copied() else {
        return;
    };
    let Some(b) = index.node_of_entity.get(&to_entity).copied() else {
        return;
    };

    let ei = graph.add_edge(a, b, ());
    index.edge_of_entity.insert(edge_entity, ei);
    index.entity_of_edge.insert(ei, edge_entity);
}

fn unregister_solver_edge(
    graph: &mut SolverGraph,
    index: &mut SolverEntityIndex,
    edge_entity: Entity,
) {
    if let Some(ei) = index.edge_of_entity.remove(&edge_entity) {
        index.entity_of_edge.remove(&ei);
        let _ = graph.remove_edge(ei);
    }
}

// ============================================================================
// ECS Sync Systems
// ============================================================================

fn sync_solver_nodes_added<Node: GraphComponent>(
    mut graph: ResMut<SolverGraph>,
    mut index: ResMut<SolverEntityIndex>,
    added: Query<Entity, Added<Node>>,
) {
    for entity in added.iter() {
        register_solver_node(&mut graph, &mut index, entity);
    }
}

fn sync_solver_nodes_removed<Node: GraphComponent>(
    mut graph: ResMut<SolverGraph>,
    mut index: ResMut<SolverEntityIndex>,
    mut removed: RemovedComponents<Node>,
) {
    for entity in removed.read() {
        unregister_solver_node(&mut graph, &mut index, entity);
    }
}

fn sync_solver_edges_added<Edge: GraphComponent>(
    mut graph: ResMut<SolverGraph>,
    mut index: ResMut<SolverEntityIndex>,
    added: Query<(Entity, &EdgeFrom, &EdgeTo), Added<Edge>>,
) {
    for (entity, from, to) in added.iter() {
        sync_solver_edge(&mut graph, &mut index, entity, from.0, to.0);
    }
}

fn sync_solver_edges_removed<Edge: GraphComponent>(
    mut graph: ResMut<SolverGraph>,
    mut index: ResMut<SolverEntityIndex>,
    mut removed: RemovedComponents<Edge>,
) {
    for entity in removed.read() {
        unregister_solver_edge(&mut graph, &mut index, entity);
    }
}

fn sync_solver_edges_changed<Edge: GraphComponent>(
    mut graph: ResMut<SolverGraph>,
    mut index: ResMut<SolverEntityIndex>,
    changed: Query<
        (Entity, &EdgeFrom, &EdgeTo),
        (Or<(Changed<EdgeFrom>, Changed<EdgeTo>)>, With<Edge>),
    >,
) {
    for (entity, from, to) in changed.iter() {
        sync_solver_edge(&mut graph, &mut index, entity, from.0, to.0);
    }
}

// ============================================================================
// Plugin
// ============================================================================

/// Plugin that wires solver infrastructure into the graph pipeline.
///
/// Maintains a [`SolverGraph`] mirror of the ECS topology, initialises
/// [`SolveJob`] for async computation, and exposes [`SolverSet`] for
/// user-defined solver systems.
///
/// Add alongside [`GraphManagerPlugin`](super::GraphManagerPlugin):
///
/// ```ignore
/// app.add_plugins(GraphSolverPlugin::<MyNode, MyEdge>::new());
/// ```
pub struct GraphSolverPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    _phantom: PhantomData<(Node, Edge)>,
}

impl<Node, Edge> GraphSolverPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<Node, Edge> Default for GraphSolverPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Node, Edge> Plugin for GraphSolverPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    fn build(&self, app: &mut App) {
        app.init_resource::<SolverConfig>()
            .init_resource::<SolverGraph>()
            .init_resource::<SolverEntityIndex>()
            .init_resource::<SolveJob>()
            .configure_sets(
                Update,
                (SolverSyncSet, SolverSet)
                    .chain()
                    .in_set(GraphSystemSet::Solving),
            )
            .add_systems(
                Update,
                (
                    sync_solver_nodes_added::<Node>,
                    sync_solver_nodes_removed::<Node>,
                    sync_solver_edges_added::<Edge>,
                    sync_solver_edges_removed::<Edge>,
                    sync_solver_edges_changed::<Edge>,
                )
                    .chain()
                    .in_set(SolverSyncSet),
            );
    }
}
