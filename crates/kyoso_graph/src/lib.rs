pub mod algo;
pub mod commands;
pub mod components;
pub mod cost;
pub mod descriptor;
pub mod ecs_view;
pub mod pattern;
pub mod queries;
pub mod scene;
pub mod subgraph;
pub mod traversal;
pub mod traverse;
pub mod tree;
pub mod variant;

pub use commands::*;
pub use components::*;
pub use ecs_view::EcsGraphView;
pub use queries::*;
pub use scene::Scene;
pub use cost::{Cost, CostHint};
pub use pattern::{Direction, PEdge, PNode, Pattern, PatternBuilder};
pub use subgraph::{Match, SubgraphMatches};
pub use traverse::{
    BfsIter, BfsIterWithDepth, BfsWalk, DfsIter, DfsIterWithDepth, DfsWalk, EdgeBfsIter,
    EdgeDfsIter, GraphNodes, GraphTraverse, GraphTraverseEdges, OrderedBfsIter, OrderedDfsIter,
    OrderedTraverse, Reverse, Step, TraversalNode,
};
pub use tree::{OrderKey, TreePlugin, TreeQuery};
pub use variant::{EdgeVariant, NodeVariant, NodeVariants};

use bevy::prelude::*;
use std::marker::PhantomData;

// ============================================================================
// Typed-graph traits
// ============================================================================

/// A typed graph: names the marker components, variant-agnostic owned
/// forms (typically sum-type enums), borrowed query projections, and
/// per-variant discriminators — symmetrically for nodes **and** edges.
///
/// Implementors are usually the node marker itself, e.g.
/// `impl Graph for FigmaNode`. One trait impl per typed graph. For
/// single-variant edges (or graphs without typed edges), use `()` for
/// the edge slots.
///
/// ```ignore
/// impl Graph for SceneNode {
///     // Nodes
///     type NodeMarker        = SceneNode;
///     type Node              = kyoso_core::Node;
///     type NodeData          = kyoso_core::AnyNodeQueryData;
///     type NodeDiscriminator = kyoso_core::NodeKind;
///     // Edges (no variants yet)
///     type EdgeMarker        = SceneEdge;
///     type Edge              = ();
///     type EdgeData          = ();
///     type EdgeDiscriminator = ();
/// }
/// ```
///
/// Per-variant typing lives in [`NodeVariant`] / [`EdgeVariant`];
/// per-graph traversal lives in [`Materialize`](crate::scene::Materialize).
/// This trait is the binding point between them.
///
/// Implementors also pin their **variant set** as a tuple in
/// [`Variants`](Self::Variants), so [`NodeVariants::try_materialize`]
/// can dispatch over the closed family without per-call match arms.
/// `()` works as the empty tuple when no typed variants exist yet.
pub trait Graph: 'static + Send + Sync {
    // ---- Nodes ----
    /// Marker `Component` identifying "this entity is a node of this graph".
    /// Used in `Added<…>` / `Changed<…>` / `With<…>` filters.
    type NodeMarker: Component;
    /// Variant-agnostic owned node form. Typically the sum-type enum
    /// returned by [`NodeVariant::wrap`].
    type Node: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static;
    /// Variant-agnostic borrowed node projection. Plugs into
    /// `GraphQuery<G::NodeData, _>` for typed graph-wide traversal.
    type NodeData: bevy::ecs::query::QueryData;
    /// Per-node-variant discriminator (e.g. an enum tag). `()` for
    /// single-variant graphs.
    type NodeDiscriminator: Copy + Eq + Send + Sync + 'static;
    /// Tuple of node variants belonging to this graph, in dispatch
    /// order. E.g. `type Variants = (Frame, Rectangle, Text)`. The
    /// matching [`NodeVariants`] tuple impl (arity 1..=8) drives the
    /// closed-sum dispatch used by agent-facing typed traversal.
    type Variants: NodeVariants<Graph = Self>;

    // ---- Edges ----
    /// Marker `Component` for "this entity is an edge of this graph".
    type EdgeMarker: Component;
    /// Variant-agnostic owned edge form. `()` for graphs without typed
    /// edges (the common case today — see `kyoso_graph_crdt::EdgeCategory`
    /// for an example of where typed edges become useful).
    type Edge: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static;
    /// Variant-agnostic borrowed edge projection. `()` if not typed.
    type EdgeData: bevy::ecs::query::QueryData;
    /// Per-edge-variant discriminator. `()` if not typed.
    type EdgeDiscriminator: Copy + Eq + Send + Sync + 'static;
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
    },
    EdgeChanged {
        entity: Entity,
        from: Entity,
        to: Entity,
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
/// CommandApplication → ChangeDetection → EventPropagation
/// ```
///
/// [`GraphManagerPlugin`] configures this chain and populates all
/// three sets. [`crate::tree::TreePlugin`] adds to `CommandApplication`.
/// Apps schedule their own systems after this chain by ordering against
/// `EventPropagation` as needed.
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub enum GraphSystemSet {
    /// Intent-based commands are applied as ECS mutations.
    CommandApplication,
    /// Automatic detection of Added/Changed/Removed components.
    ChangeDetection,
    /// Graph-aware propagation of changes through topology.
    EventPropagation,
}

/// Graph manager plugin parameterised over the **node marker** and
/// **edge marker** components. These are the `Component`s that
/// change-detection systems filter on (`Added<NM>` / `Changed<NM>` /
/// `With<NM>`).
///
/// The plugin only needs markers, not the full [`Graph`] trait — that
/// kicks in for typed traversal in the consumer crate. Pass any
/// `Component` directly:
/// `GraphManagerPlugin::<SceneNode, SceneEdge>::new()`.
///
/// This plugin wires up:
/// - Change detection for nodes and edges
/// - Topology-change tracking on the underlying edge components
/// - Tree-position-change tracking ([`tree::TreeParent`] / [`tree::OrderKey`])
/// - Event propagation through the graph
pub struct GraphManagerPlugin<NM, EM>
where
    NM: Component,
    EM: Component,
{
    _phantom: PhantomData<(NM, EM)>,
}

impl<NM, EM> GraphManagerPlugin<NM, EM>
where
    NM: Component,
    EM: Component,
{
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<NM, EM> Default for GraphManagerPlugin<NM, EM>
where
    NM: Component,
    EM: Component,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<NM, EM> Plugin for GraphManagerPlugin<NM, EM>
where
    NM: Component,
    EM: Component,
{
    fn build(&self, app: &mut App) {
        app.register_type::<GraphMessage>()
            .register_type::<PropagationType>()
            .add_message::<GraphMessage>()
            .register_type::<GraphCommand>()
            .add_message::<GraphCommand>()
            .init_resource::<GraphEventPropagationConfig>()
            .init_resource::<PropagationBuffer>()
            .configure_sets(
                Update,
                (
                    GraphSystemSet::CommandApplication,
                    GraphSystemSet::ChangeDetection,
                    GraphSystemSet::EventPropagation,
                )
                    .chain(),
            )
            // Command application: consume GraphCommand messages
            .add_systems(
                Update,
                consume_graph_commands::<NM, EM>.in_set(GraphSystemSet::CommandApplication),
            )
            // Change detection
            .add_systems(
                Update,
                (
                    detect_node_changes::<NM, EM>,
                    detect_edge_changes::<EM>,
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
                    read_propagation_messages::<NM, EM>,
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

fn consume_graph_commands<NM: Component, EM: Component>(
    mut commands: Commands,
    mut reader: MessageReader<GraphCommand>,
    graph_query: GraphQuery<'_, '_, &NM, &EM>,
) {
    for cmd in reader.read() {
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

fn detect_node_changes<NM: Component, EM: Component>(
    graph_query: GraphQuery<'_, '_, &NM, &EM>,
    mut graph_messages: MessageWriter<GraphMessage>,
    added_nodes: Query<Entity, Added<NM>>,
    mut removed_nodes: RemovedComponents<NM>,
) {
    for entity in added_nodes.iter() {
        let neighbors = graph_query.affected_neighbors(entity);
        graph_messages.write(GraphMessage::NodeAdded {
            entity,
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

fn detect_edge_changes<EM: Component>(
    mut graph_messages: MessageWriter<GraphMessage>,
    added_edges: Query<(Entity, &EdgeFrom, &EdgeTo), Added<EM>>,
    mut removed_edges: RemovedComponents<EM>,
    edges: Query<(&EdgeFrom, &EdgeTo), With<EM>>,
) {
    for (entity, edge_from, edge_to) in added_edges.iter() {
        graph_messages.write(GraphMessage::EdgeAdded {
            entity,
            from: edge_from.0,
            to: edge_to.0,
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

/// Emit [`GraphMessage::TreePositionChanged`] when a node's `ChildOf`
/// or [`tree::OrderKey`] changes — i.e. when the node has been
/// reparented or reordered among its siblings.
///
/// Fires uniformly for:
/// - Local edits via [`GraphCommand::Reparent`] /
///   [`GraphCommand::MoveSibling`] / [`GraphCommand::InsertChild`].
/// - Remote-applied CRDT `Move` ops, applied via `apply_tree_commands`.
///
/// `new_parent: None` indicates the node is a root (no `ChildOf`).
///
/// The propagation layer ([`read_propagation_messages`]) treats this
/// like [`GraphMessage::NodeChanged`], so a tree move triggers the
/// same downstream propagation as any other node update.
fn detect_tree_position_changes(
    mut graph_messages: MessageWriter<GraphMessage>,
    moved: Query<
        (Entity, Option<&ChildOf>, &tree::OrderKey),
        Or<(Changed<ChildOf>, Changed<tree::OrderKey>)>,
    >,
) {
    for (entity, child_of, key) in moved.iter() {
        graph_messages.write(GraphMessage::TreePositionChanged {
            entity,
            new_parent: child_of.map(|c| c.0),
            position: key.clone(),
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

fn read_propagation_messages<NM: Component, EM: Component>(
    config: Res<GraphEventPropagationConfig>,
    graph_query: GraphQuery<'_, '_, &NM, &EM>,
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
            }
            | GraphMessage::NodeAdded {
                entity,
                initial_neighbors: _,
            }
            | GraphMessage::TreePositionChanged {
                entity,
                new_parent: _,
                position: _,
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
