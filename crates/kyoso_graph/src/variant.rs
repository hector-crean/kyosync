//! Typed projection between marker `Component`s and the bundle each
//! composes, plus the **closed-per-domain sum** that lets a typed
//! traversal yield `match`-able rows.
//!
//! Three layered traits work together:
//!
//! - [`NodeVariant`] — a single variant. Marker `Component` + paired
//!   `Bundle` ([`Data`](NodeVariant::Data)) + borrowed query projection
//!   ([`Query`](NodeVariant::Query)) + `wrap` lift into the sum.
//! - [`crate::Graph`] — a *domain* (e.g. the kyoso scene). Pins the
//!   variant set as a tuple in `Variants` and names the owned sum type
//!   `Node` plus its discriminator `NodeDiscriminator`.
//! - [`NodeVariants`] — implemented for tuples of [`NodeVariant`]s with
//!   a shared `Graph`. Holds the per-variant `QueryState` cache and
//!   does the try-each-variant dispatch. **You don't impl this** — the
//!   tuple impls below cover it for arity 1..=8.
//!
//! Use this when you want to materialize a node entity into the owned
//! sum without already knowing its discriminator — e.g. agent-facing
//! traversal walks that need to resolve `Entity → Node` from `&World`
//! directly. The in-system path with a held `SystemParam` (see e.g.
//! `kyoso_core::SceneNodeQuery::get`) is still cheaper when you can
//! afford to declare the per-variant `Query`s up front.

use bevy::ecs::bundle::Bundle;
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::query::{QueryData, QueryState, ROQueryItem, With};
use bevy::ecs::world::World;

use crate::Graph;

// ============================================================================
// NodeVariant — a single variant within some graph
// ============================================================================

/// A typed *node* variant within a [`Graph`]. The implementing type
/// **is** the variant's marker `Component` (`Self: Component`),
/// eliminating a separate `type Marker` slot.
///
/// Adding a new variant doesn't touch any central enum (besides the
/// per-graph [`Graph::Node`] sum that lists which variants this graph
/// permits) — the variant declares its own bundle, query projection,
/// and discriminator constant.
///
/// ```ignore
/// impl NodeVariant for Frame {
///     type Graph = SceneNode;
///     type Data  = FrameData;
///     type Query = FrameQueryData;
///     const KIND: NodeKind = NodeKind::Frame;
///     fn wrap(data: FrameData) -> Node { Node::Frame(data) }
///     fn materialize(item: ROQueryItem<'_, '_, FrameQueryData>) -> FrameData { /* clones */ }
/// }
/// ```
pub trait NodeVariant: Component + 'static {
    type Graph: Graph;
    type Data: Bundle
        + serde::Serialize
        + serde::de::DeserializeOwned
        + Default
        + Clone;
    type Query: QueryData;
    const KIND: <Self::Graph as Graph>::NodeDiscriminator;
    fn wrap(data: Self::Data) -> <Self::Graph as Graph>::Node;
    fn materialize(item: ROQueryItem<'_, '_, Self::Query>) -> Self::Data;
}

// ============================================================================
// EdgeVariant — mirror of NodeVariant for typed edge variants
// ============================================================================

/// A typed *edge* variant within a [`Graph`]. Mirror of [`NodeVariant`].
/// The implementing type is the variant's edge marker `Component`.
///
/// No domain impls exist today — `SceneEdge` is a structural marker
/// with no variants yet. The trait shape is here so future typed-edge
/// work (e.g. reference edges → `EdgeCategory::{InstanceOf,
/// PrototypeLink, …}`) can land without trait redesign.
pub trait EdgeVariant: Component + 'static {
    type Graph: Graph;
    type Data: Bundle
        + serde::Serialize
        + serde::de::DeserializeOwned
        + Default
        + Clone;
    type Query: QueryData;
    const KIND: <Self::Graph as Graph>::EdgeDiscriminator;
    fn wrap(data: Self::Data) -> <Self::Graph as Graph>::Edge;
    fn materialize(item: ROQueryItem<'_, '_, Self::Query>) -> Self::Data;
}

// ============================================================================
// NodeVariants — implemented for tuples of NodeVariant
// ============================================================================

/// Implemented for tuples of [`NodeVariant`]s that all share the same
/// [`Graph`]. Owns the per-variant [`QueryState`] cache and does the
/// try-each-variant dispatch.
///
/// You don't implement this — the tuple impls below cover arity 1..=8.
/// Set `Graph::Variants = (Frame, Rectangle, Text)` to wire your graph up;
/// the matching tuple impl is selected automatically.
pub trait NodeVariants: 'static {
    /// The graph all variants in this tuple belong to.
    type Graph: Graph;

    /// Tuple of `QueryState`s, one per variant. Built once via
    /// [`build_states`](Self::build_states) and reused for every
    /// per-entity probe.
    type States;

    /// Build the cache of per-variant `QueryState`s. Pays the
    /// component-id-resolution cost once per call site.
    fn build_states(world: &mut World) -> Self::States;

    /// Try each variant in tuple order, return the first match wrapped
    /// into the graph's [`Node`](Graph::Node) sum.
    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node>;
}

// ----------------------------------------------------------------------------
// Tuple impls (arity 1..=8). All hand-written; mechanical but explicit.
// If we ever outgrow 8 variants in one graph, extend here.
// ----------------------------------------------------------------------------

impl<A> NodeVariants for (A,)
where
    A: NodeVariant,
{
    type Graph = A::Graph;
    type States = QueryState<A::Query, With<A>>;

    fn build_states(world: &mut World) -> Self::States {
        QueryState::new(world)
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.get(world, entity).ok().map(A::materialize).map(A::wrap)
    }
}

impl<A, B> NodeVariants for (A, B)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (QueryState::new(world), QueryState::new(world))
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
    }
}

impl<A, B, C> NodeVariants for (A, B, C)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
    C: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
        QueryState<C::Query, With<C>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
        )
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
            .or_else(|| states.2.get(world, entity).ok().map(C::materialize).map(C::wrap))
    }
}

impl<A, B, C, D> NodeVariants for (A, B, C, D)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
    C: NodeVariant<Graph = A::Graph>,
    D: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
        QueryState<C::Query, With<C>>,
        QueryState<D::Query, With<D>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
        )
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
            .or_else(|| states.2.get(world, entity).ok().map(C::materialize).map(C::wrap))
            .or_else(|| states.3.get(world, entity).ok().map(D::materialize).map(D::wrap))
    }
}

impl<A, B, C, D, E> NodeVariants for (A, B, C, D, E)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
    C: NodeVariant<Graph = A::Graph>,
    D: NodeVariant<Graph = A::Graph>,
    E: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
        QueryState<C::Query, With<C>>,
        QueryState<D::Query, With<D>>,
        QueryState<E::Query, With<E>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
        )
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
            .or_else(|| states.2.get(world, entity).ok().map(C::materialize).map(C::wrap))
            .or_else(|| states.3.get(world, entity).ok().map(D::materialize).map(D::wrap))
            .or_else(|| states.4.get(world, entity).ok().map(E::materialize).map(E::wrap))
    }
}

impl<A, B, C, D, E, F> NodeVariants for (A, B, C, D, E, F)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
    C: NodeVariant<Graph = A::Graph>,
    D: NodeVariant<Graph = A::Graph>,
    E: NodeVariant<Graph = A::Graph>,
    F: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
        QueryState<C::Query, With<C>>,
        QueryState<D::Query, With<D>>,
        QueryState<E::Query, With<E>>,
        QueryState<F::Query, With<F>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
        )
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
            .or_else(|| states.2.get(world, entity).ok().map(C::materialize).map(C::wrap))
            .or_else(|| states.3.get(world, entity).ok().map(D::materialize).map(D::wrap))
            .or_else(|| states.4.get(world, entity).ok().map(E::materialize).map(E::wrap))
            .or_else(|| states.5.get(world, entity).ok().map(F::materialize).map(F::wrap))
    }
}

impl<A, B, C, D, E, F, G> NodeVariants for (A, B, C, D, E, F, G)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
    C: NodeVariant<Graph = A::Graph>,
    D: NodeVariant<Graph = A::Graph>,
    E: NodeVariant<Graph = A::Graph>,
    F: NodeVariant<Graph = A::Graph>,
    G: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
        QueryState<C::Query, With<C>>,
        QueryState<D::Query, With<D>>,
        QueryState<E::Query, With<E>>,
        QueryState<F::Query, With<F>>,
        QueryState<G::Query, With<G>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
        )
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
            .or_else(|| states.2.get(world, entity).ok().map(C::materialize).map(C::wrap))
            .or_else(|| states.3.get(world, entity).ok().map(D::materialize).map(D::wrap))
            .or_else(|| states.4.get(world, entity).ok().map(E::materialize).map(E::wrap))
            .or_else(|| states.5.get(world, entity).ok().map(F::materialize).map(F::wrap))
            .or_else(|| states.6.get(world, entity).ok().map(G::materialize).map(G::wrap))
    }
}

impl<A, B, C, D, E, F, G, H> NodeVariants for (A, B, C, D, E, F, G, H)
where
    A: NodeVariant,
    B: NodeVariant<Graph = A::Graph>,
    C: NodeVariant<Graph = A::Graph>,
    D: NodeVariant<Graph = A::Graph>,
    E: NodeVariant<Graph = A::Graph>,
    F: NodeVariant<Graph = A::Graph>,
    G: NodeVariant<Graph = A::Graph>,
    H: NodeVariant<Graph = A::Graph>,
{
    type Graph = A::Graph;
    type States = (
        QueryState<A::Query, With<A>>,
        QueryState<B::Query, With<B>>,
        QueryState<C::Query, With<C>>,
        QueryState<D::Query, With<D>>,
        QueryState<E::Query, With<E>>,
        QueryState<F::Query, With<F>>,
        QueryState<G::Query, With<G>>,
        QueryState<H::Query, With<H>>,
    );

    fn build_states(world: &mut World) -> Self::States {
        (
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
            QueryState::new(world),
        )
    }

    fn try_materialize(
        states: &mut Self::States,
        world: &World,
        entity: Entity,
    ) -> Option<<Self::Graph as Graph>::Node> {
        states.0.get(world, entity).ok().map(A::materialize).map(A::wrap)
            .or_else(|| states.1.get(world, entity).ok().map(B::materialize).map(B::wrap))
            .or_else(|| states.2.get(world, entity).ok().map(C::materialize).map(C::wrap))
            .or_else(|| states.3.get(world, entity).ok().map(D::materialize).map(D::wrap))
            .or_else(|| states.4.get(world, entity).ok().map(E::materialize).map(E::wrap))
            .or_else(|| states.5.get(world, entity).ok().map(F::materialize).map(F::wrap))
            .or_else(|| states.6.get(world, entity).ok().map(G::materialize).map(G::wrap))
            .or_else(|| states.7.get(world, entity).ok().map(H::materialize).map(H::wrap))
    }
}
