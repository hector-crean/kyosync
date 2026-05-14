pub mod commands;
pub mod components;
pub mod ecs_view;
pub mod queries;
pub mod solver;
pub mod transaction;
pub mod tree;

pub use commands::*;
pub use components::*;
pub use ecs_view::EcsGraphView;
pub use queries::*;
pub use solver::*;
pub use transaction::*;
pub use tree::{OrderKey, TreeEdge, TreeParent, TreePlugin};

use bevy::prelude::*;
use std::fmt::Debug;
use std::marker::PhantomData;

/// Change set tracking the origin of a node-level change.
///
/// The [`source`](NodeChangeSet::source) field records the origin of the
/// change (user, solver, propagation, remote peer), enabling downstream
/// systems to filter and avoid feedback loops. Domain-specific change
/// flags are the consumer's responsibility.
#[derive(Clone, Debug, Reflect, Default, serde::Serialize, serde::Deserialize)]
pub struct NodeChangeSet {
    pub source: ChangeSource,
}

/// Change set tracking the origin of an edge-level change.
///
/// See [`NodeChangeSet`] for details on the [`source`](EdgeChangeSet::source)
/// field.
#[derive(Clone, Debug, Reflect, Default, serde::Serialize, serde::Deserialize)]
pub struct EdgeChangeSet {
    pub source: ChangeSource,
}

/// Type of graph propagation event
#[derive(Clone, Debug, Reflect, PartialEq, Eq)]
pub enum PropagationType {
    ImmediateNeighbors,
    ConnectedComponent,
    Downstream,
    Upstream,
    LimitedDepth { depth: usize },
}

#[derive(Message, Event, Clone, Debug, Reflect)]
pub enum GraphMessage {
    NodeAdded {
        entity: Entity,
        initial_neighbors: Vec<Entity>,
    },
    NodeRemoved {
        entity: Entity,
        affected_edges: Vec<Entity>,
    },
    EdgeAdded {
        entity: Entity,
        from: Entity,
        to: Entity,
    },
    EdgeRemoved {
        entity: Entity,
        from: Entity,
        to: Entity,
    },

    NodeConnected {
        node: Entity,
        edge: Entity,
        neighbor: Entity,
    },
    NodeDisconnected {
        node: Entity,
        edge: Entity,
        neighbor: Entity,
    },

    NodeChanged {
        entity: Entity,
        affected_neighbors: Vec<Entity>,
        changes: NodeChangeSet,
    },
    EdgeChanged {
        entity: Entity,
        from: Entity,
        to: Entity,
        changes: EdgeChangeSet,
    },

    /// A node moved within the tree — either reparented (changed
    /// [`tree::TreeParent`]) or reordered among its siblings (changed
    /// [`tree::OrderKey`]).
    ///
    /// Fired by [`detect_tree_position_changes`] for both local and
    /// remote-applied moves, so downstream propagation handles them
    /// uniformly without caring which side originated the move.
    /// `new_parent` is `None` for a root.
    TreePositionChanged {
        entity: Entity,
        new_parent: Option<Entity>,
        position: tree::OrderKey,
        changes: NodeChangeSet,
    },

    PropagationTriggered {
        source: Entity,
        affected_nodes: Vec<Entity>,
        propagation_type: PropagationType,
    },
}

/// Intent-based graph commands.
///
/// Write these via [`MessageWriter<GraphCommand>`] to request structural
/// graph mutations. The [`GraphManagerPlugin`] includes a system in
/// [`GraphSystemSet::CommandApplication`] that consumes these and
/// translates them into ECS mutations + optional transaction records.
///
/// For topology-only commands, inverses are automatically computed and
/// stored in the [`PendingTransaction`] when [`TransactionPlugin`] is
/// active.
#[derive(Message, Event, Clone, Debug, Reflect)]
pub enum GraphCommand {
    /// Connect two existing node entities with a new directed edge.
    Connect { from: Entity, to: Entity },
    /// Remove the directed edge from `from` → `to`, if it exists.
    Disconnect { from: Entity, to: Entity },
    /// Remove a node entity and despawn all its connected edges.
    RemoveNode { entity: Entity },
    /// Remove a specific edge entity.
    RemoveEdge { entity: Entity },
    /// Insert `child` as a child of `parent` at `position`. Spawns a
    /// [`tree::TreeEdge`]-marked edge and stamps `position` onto `child`.
    InsertChild {
        parent: Entity,
        child: Entity,
        position: tree::OrderKey,
    },
    /// Move `child` to a new parent at `position`. Despawns the previous
    /// tree-parent edge if any, then spawns a new one.
    Reparent {
        child: Entity,
        new_parent: Entity,
        position: tree::OrderKey,
    },
    /// Update `child`'s sibling ordering without changing its parent.
    MoveSibling {
        child: Entity,
        position: tree::OrderKey,
    },
}


// `Graph<N, E, B>`, `GraphEntityIndex<N, E, B>`, `NodeState<N>`,
// `EdgeState<E>`, and the `GraphBackend` trait were deleted in
// Part IV §IV.2 Step 3. They were the swap-in seam between an
// in-memory petgraph mirror and a CRDT-replicated store; with the
// petgraph mirror gone (queries always go through `GraphQuery`
// against ECS) and the CRDT bookkeeping moved to
// `kyoso_sync::ClientSyncEngine`, none of these types had any
// callers left.
//
// Consumers that previously held `Graph<N, E, CrdtBackend<N, E>>`
// resources should switch to `kyoso_sync::ClientSyncEngine`, and
// `GraphEntityIndex<N, E, B>` becomes `kyoso_sync::EntityCrdtIndex`
// (collapsed to a non-generic `Entity ↔ CrdtId` map).

/// Configuration for graph event propagation behavior
#[derive(Resource, Clone, Debug, Reflect)]
pub struct GraphEventPropagationConfig {
    pub propagate_node_changes: bool,
    pub propagate_edge_changes: bool,
    pub default_node_propagation: PropagationType,
    pub default_edge_propagation: PropagationType,
}

impl Default for GraphEventPropagationConfig {
    fn default() -> Self {
        Self {
            propagate_node_changes: true,
            propagate_edge_changes: false,
            default_node_propagation: PropagationType::ImmediateNeighbors,
            default_edge_propagation: PropagationType::ImmediateNeighbors,
        }
    }
}

/// System sets defining the graph pipeline execution order.
///
/// Every set is ordered linearly — each runs strictly after the previous:
///
/// ```text
/// TransactionRecording → CommandApplication → ChangeDetection
///   → EventGeneration → EventPropagation → Solving
///   → SnapshotCreation → Consumption
/// ```
///
/// [`GraphManagerPlugin`] configures this chain and populates
/// `CommandApplication`, `ChangeDetection`, and `EventPropagation`.
/// The remaining sets are available for downstream plugins and user
/// systems.
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub enum GraphSystemSet {
    /// External commands arrive (from MCP, network, UI, etc.).
    TransactionRecording,
    /// Intent-based commands are applied as ECS mutations.
    CommandApplication,
    /// Automatic detection of Added/Changed/Removed components.
    ChangeDetection,
    /// Reserved for user-defined event generation systems.
    EventGeneration,
    /// Graph-aware propagation of changes through topology.
    EventPropagation,
    /// Solver/constraint evaluation (see [`SolverSet`]).
    Solving,
    /// State captured for undo/redo and network replication.
    SnapshotCreation,
    /// Downstream consumers: rendering, network broadcast, etc.
    Consumption,
}

/// Generic graph manager plugin that can work with any node and edge types.
///
/// This plugin sets up the ECS infrastructure for managing graphs in Bevy:
/// - Change detection for nodes and edges
/// - Event propagation through the graph
/// - Synchronization with a petgraph representation
///
/// # Type Parameters
/// - `Node`: The component type for graph nodes (must implement `GraphComponent`)
/// - `Edge`: The component type for graph edges (must implement `GraphComponent`)
pub struct GraphManagerPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    _phantom: PhantomData<(Node, Edge)>,
}

impl<Node, Edge> GraphManagerPlugin<Node, Edge>
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

impl<Node, Edge> Default for GraphManagerPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Node, Edge> Plugin for GraphManagerPlugin<Node, Edge>
where
    Node: GraphComponent + Debug,
    Edge: GraphComponent + Debug,
{
    fn build(&self, app: &mut App) {
        app.register_type::<GraphMessage>()
            .register_type::<NodeChangeSet>()
            .register_type::<EdgeChangeSet>()
            .register_type::<PropagationType>()
            .register_type::<ChangeSource>()
            .add_message::<GraphMessage>()
            .register_type::<GraphCommand>()
            .add_message::<GraphCommand>()
            .init_resource::<GraphEventPropagationConfig>()
            .init_resource::<PropagationBuffer>()
            .configure_sets(
                Update,
                (
                    GraphSystemSet::TransactionRecording,
                    GraphSystemSet::CommandApplication,
                    GraphSystemSet::ChangeDetection,
                    GraphSystemSet::EventGeneration,
                    GraphSystemSet::EventPropagation,
                    GraphSystemSet::Solving,
                    GraphSystemSet::SnapshotCreation,
                    GraphSystemSet::Consumption,
                )
                    .chain(),
            )
            // Command application: consume GraphCommand messages
            .add_systems(
                Update,
                consume_graph_commands::<Node, Edge>
                    .in_set(GraphSystemSet::CommandApplication),
            )
            // Change detection
            .add_systems(
                Update,
                (
                    detect_node_changes::<Node, Edge>,
                    detect_edge_changes::<Edge>,
                    detect_topology_changes,
                    detect_tree_position_changes,
                )
                    .chain()
                    .in_set(GraphSystemSet::ChangeDetection),
            )
            // Event propagation
            .add_systems(
                Update,
                (
                    read_propagation_messages::<Node, Edge>,
                    write_propagation_events,
                )
                    .chain()
                    .in_set(GraphSystemSet::EventPropagation),
            );
    }
}

// ============================================================================
// Command Application System
// ============================================================================

fn consume_graph_commands<Node: GraphComponent + Debug, Edge: GraphComponent + Debug>(
    mut commands: Commands,
    mut reader: MessageReader<GraphCommand>,
    graph_query: GraphQuery<'_, '_, Node, Edge>,
    pending: Option<ResMut<PendingTransaction>>,
) {
    let mut pending = pending;

    for cmd in reader.read() {
        let inverse = match cmd {
            GraphCommand::Connect { from, to } => Some(GraphCommand::Disconnect {
                from: *from,
                to: *to,
            }),
            GraphCommand::Disconnect { from, to } => Some(GraphCommand::Connect {
                from: *from,
                to: *to,
            }),
            GraphCommand::RemoveNode { .. }
            | GraphCommand::RemoveEdge { .. }
            | GraphCommand::InsertChild { .. }
            | GraphCommand::Reparent { .. }
            | GraphCommand::MoveSibling { .. } => None,
        };

        if let Some(ref mut pending) = pending {
            pending.record(cmd.clone(), inverse);
        }

        match cmd {
            GraphCommand::Connect { from, to } => {
                spawn_edge(&mut commands, *from, *to);
            }
            GraphCommand::Disconnect { from, to } => {
                if let Some(edge) = graph_query.find_edge(*from, *to) {
                    commands.entity(edge).despawn();
                }
            }
            GraphCommand::RemoveNode { entity } => {
                for edge in graph_query.connected_edges(*entity) {
                    commands.entity(edge).despawn();
                }
                commands.entity(*entity).despawn();
            }
            GraphCommand::RemoveEdge { entity } => {
                commands.entity(*entity).despawn();
            }
            // Tree-shaped commands are consumed by `tree::TreePlugin`.
            GraphCommand::InsertChild { .. }
            | GraphCommand::Reparent { .. }
            | GraphCommand::MoveSibling { .. } => {}
        }
    }
}

// ============================================================================
// Change Detection Systems
// ============================================================================

fn detect_node_changes<Node: GraphComponent, Edge: GraphComponent>(
    graph_query: GraphQuery<'_, '_, Node, Edge>,
    mut graph_messages: MessageWriter<GraphMessage>,
    added_nodes: Query<NodeQueryData<Node>, Added<Node>>,
    mut removed_nodes: RemovedComponents<Node>,
) {
    for node_data in added_nodes.iter() {
        let neighbors = graph_query.affected_neighbors(node_data.entity);
        graph_messages.write(GraphMessage::NodeAdded {
            entity: node_data.entity,
            initial_neighbors: neighbors,
        });
    }

    for entity in removed_nodes.read() {
        let affected_edges = graph_query.connected_edges(entity);
        graph_messages.write(GraphMessage::NodeRemoved {
            entity,
            affected_edges,
        });
    }
}

fn detect_edge_changes<Edge: GraphComponent>(
    mut graph_messages: MessageWriter<GraphMessage>,
    added_edges: Query<EdgeQueryData<Edge>, Added<Edge>>,
    mut removed_edges: RemovedComponents<Edge>,
    edges: Query<(&EdgeFrom, &EdgeTo), With<Edge>>,
) {
    for edge_data in added_edges.iter() {
        graph_messages.write(GraphMessage::EdgeAdded {
            entity: edge_data.entity,
            from: edge_data.edge_from.0,
            to: edge_data.edge_to.0,
        });
    }

    for entity in removed_edges.read() {
        if let Ok((from, to)) = edges.get(entity) {
            graph_messages.write(GraphMessage::EdgeRemoved {
                entity,
                from: from.0,
                to: to.0,
            });
        }
    }
}

fn detect_topology_changes(
    mut graph_messages: MessageWriter<GraphMessage>,
    changed_edges: Query<(Entity, &EdgeFrom, &EdgeTo), Or<(Changed<EdgeFrom>, Changed<EdgeTo>)>>,
) {
    for (edge_entity, from, to) in changed_edges.iter() {
        graph_messages.write(GraphMessage::NodeConnected {
            node: from.0,
            edge: edge_entity,
            neighbor: to.0,
        });
        graph_messages.write(GraphMessage::NodeConnected {
            node: to.0,
            edge: edge_entity,
            neighbor: from.0,
        });
    }
}

/// Emit [`GraphMessage::TreePositionChanged`] when a node's
/// [`tree::TreeParent`] or [`tree::OrderKey`] changes — i.e. when the
/// node has been reparented or reordered among its siblings.
///
/// Fires uniformly for:
/// - Local edits via [`GraphCommand::Reparent`] /
///   [`GraphCommand::MoveSibling`] / [`GraphCommand::InsertChild`].
/// - Remote-applied CRDT `Move` ops, where
///   `kyoso_graph_sync::plugin::project_move` writes a new
///   `TreeParent` / `OrderKey` onto the affected entity.
///
/// The propagation layer ([`read_propagation_messages`]) treats this
/// like [`GraphMessage::NodeChanged`], so a tree move triggers the
/// same downstream propagation as any other node update — meaning
/// solvers and consumers don't have to special-case where the move
/// originated.
fn detect_tree_position_changes(
    mut graph_messages: MessageWriter<GraphMessage>,
    moved: Query<
        (Entity, &tree::TreeParent, &tree::OrderKey),
        Or<(Changed<tree::TreeParent>, Changed<tree::OrderKey>)>,
    >,
) {
    for (entity, parent, key) in moved.iter() {
        graph_messages.write(GraphMessage::TreePositionChanged {
            entity,
            new_parent: parent.0,
            position: key.clone(),
            changes: NodeChangeSet::default(),
        });
    }
}

// ============================================================================
// Event Propagation System
// ============================================================================

#[derive(Resource, Default)]
struct PropagationBuffer {
    events: Vec<GraphMessage>,
}

fn read_propagation_messages<Node: GraphComponent, Edge: GraphComponent>(
    config: Res<GraphEventPropagationConfig>,
    graph_query: GraphQuery<'_, '_, Node, Edge>,
    mut reader: MessageReader<GraphMessage>,
    mut buffer: ResMut<PropagationBuffer>,
) {
    if !config.propagate_node_changes && !config.propagate_edge_changes {
        return;
    }

    buffer.events.clear();

    for message in reader.read() {
        match message {
            GraphMessage::NodeChanged {
                entity,
                affected_neighbors: _,
                changes: _,
            }
            | GraphMessage::NodeAdded {
                entity,
                initial_neighbors: _,
            }
            | GraphMessage::TreePositionChanged {
                entity,
                new_parent: _,
                position: _,
                changes: _,
            } => {
                if config.propagate_node_changes {
                    let affected_nodes = match config.default_node_propagation {
                        PropagationType::ImmediateNeighbors => {
                            graph_query.affected_neighbors(*entity)
                        }
                        PropagationType::ConnectedComponent => {
                            graph_query.connected_component_undirected(*entity)
                        }
                        PropagationType::Downstream => graph_query.downstream_nodes(*entity),
                        PropagationType::Upstream => graph_query.upstream_nodes(*entity),
                        PropagationType::LimitedDepth { depth } => {
                            graph_query.affected_subgraph(*entity, Some(depth))
                        }
                    };

                    buffer.events.push(GraphMessage::PropagationTriggered {
                        source: *entity,
                        affected_nodes,
                        propagation_type: config.default_node_propagation.clone(),
                    });
                }
            }
            GraphMessage::EdgeChanged {
                entity,
                from,
                to,
                changes: _,
            } => {
                if config.propagate_edge_changes {
                    let affected_nodes = match config.default_edge_propagation {
                        PropagationType::ImmediateNeighbors => {
                            let mut nodes = graph_query.affected_neighbors(*from);
                            nodes.extend(graph_query.affected_neighbors(*to));
                            nodes.sort();
                            nodes.dedup();
                            nodes
                        }
                        PropagationType::ConnectedComponent => {
                            let mut from_comp = graph_query.connected_component_undirected(*from);
                            let to_comp = graph_query.connected_component_undirected(*to);
                            from_comp.extend(to_comp);
                            from_comp.sort();
                            from_comp.dedup();
                            from_comp
                        }
                        PropagationType::Downstream => {
                            let mut nodes = graph_query.downstream_nodes(*from);
                            nodes.extend(graph_query.downstream_nodes(*to));
                            nodes.sort();
                            nodes.dedup();
                            nodes
                        }
                        PropagationType::Upstream => {
                            let mut nodes = graph_query.upstream_nodes(*from);
                            nodes.extend(graph_query.upstream_nodes(*to));
                            nodes.sort();
                            nodes.dedup();
                            nodes
                        }
                        PropagationType::LimitedDepth { depth } => {
                            let mut nodes = graph_query.affected_subgraph(*from, Some(depth));
                            nodes.extend(graph_query.affected_subgraph(*to, Some(depth)));
                            nodes.sort();
                            nodes.dedup();
                            nodes
                        }
                    };

                    buffer.events.push(GraphMessage::PropagationTriggered {
                        source: *entity,
                        affected_nodes,
                        propagation_type: config.default_edge_propagation.clone(),
                    });
                }
            }
            _ => {}
        }
    }
}

fn write_propagation_events(
    mut graph_messages: MessageWriter<GraphMessage>,
    mut buffer: ResMut<PropagationBuffer>,
) {
    for event in buffer.events.drain(..) {
        graph_messages.write(event);
    }
}

