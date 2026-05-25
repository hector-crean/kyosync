//! Pattern + subgraph-isomorphism tests against the typed
//! [`SceneView`] = `Scene<&SceneNode, &SceneEdge>`.
//!
//! Confirms that the generic [`kyoso_graph::pattern::PatternBuilder`]
//! + [`kyoso_graph::subgraph::SubgraphMatches`] machinery composes
//! cleanly with our typed combined view: we build patterns over
//! `Entity` predicates, run them through `scene.graph.subgraph_matches`,
//! and read out [`Match`] rows.
//!
//! ## Fixture
//!
//! ```text
//!         r (Frame)
//!         │
//!       e1↓
//!         m (Rectangle)
//!        ╱ ╲
//!     e2╱   ╲e3
//!      ↓     ↓
//!      t     x      (both Text)
//! ```
//!
//! 4 scene nodes (`SceneNode` markers), 3 entity-edges (`SceneEdge`).
//! Hierarchy isn't relevant to subgraph matching — the matcher only
//! sees the entity-edge layer.

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use kyoso_core::{
    Frame, FrameData, Rectangle, RectangleData, SceneEdge, SceneNode, SceneView, TextData,
};
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph::pattern::PatternBuilder;
use kyoso_graph::subgraph::Match;
// `subgraph_matches` is a default method on `GraphTraverseEdges` — trait
// must be in scope at call sites.
use kyoso_graph::traverse::GraphTraverseEdges;

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app
}

struct Fixture {
    r: Entity,
    m: Entity,
    t: Entity,
    x: Entity,
}

fn spawn_fixture(world: &mut World) -> Fixture {
    world
        .run_system_once(|mut commands: Commands| {
            let r = commands
                .spawn((FrameData::default(), Transform::IDENTITY, SceneNode))
                .id();
            let m = commands
                .spawn((RectangleData::default(), Transform::IDENTITY, SceneNode))
                .id();
            let t = commands
                .spawn((TextData::default(), Transform::IDENTITY, SceneNode))
                .id();
            let x = commands
                .spawn((TextData::default(), Transform::IDENTITY, SceneNode))
                .id();
            // r → m, m → t, m → x
            commands.spawn((EdgeFrom(r), EdgeTo(m), SceneEdge));
            commands.spawn((EdgeFrom(m), EdgeTo(t), SceneEdge));
            commands.spawn((EdgeFrom(m), EdgeTo(x), SceneEdge));
            Fixture { r, m, t, x }
        })
        .expect("spawn fixture")
}

// ---------------------------------------------------------------------------
// One-edge pattern (any A → any B)
// ---------------------------------------------------------------------------

#[test]
fn one_edge_pattern_finds_every_directed_edge() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());

    let pairs = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let mut builder = PatternBuilder::new();
            let a = builder.node(|_| true);
            let b = builder.node(|_| true);
            let _e = builder.edge(a, b);
            let pattern = builder.build();
            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| (m.node(a), m.node(b)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    let mut sorted = pairs.clone();
    sorted.sort();
    let mut expected = vec![(fx.r, fx.m), (fx.m, fx.t), (fx.m, fx.x)];
    expected.sort();
    assert_eq!(sorted, expected, "got {pairs:?}");
}

// ---------------------------------------------------------------------------
// Two-edge path (A → B → C)
// ---------------------------------------------------------------------------

#[test]
fn two_edge_path_pattern_finds_length_two_paths() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());

    let triples = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let mut builder = PatternBuilder::new();
            let a = builder.node(|_| true);
            let b = builder.node(|_| true);
            let c = builder.node(|_| true);
            let _ab = builder.edge(a, b);
            let _bc = builder.edge(b, c);
            let pattern = builder.build();
            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| (m.node(a), m.node(b), m.node(c)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    // r → m → t and r → m → x.
    let mut sorted = triples.clone();
    sorted.sort();
    let mut expected = vec![(fx.r, fx.m, fx.t), (fx.r, fx.m, fx.x)];
    expected.sort();
    assert_eq!(sorted, expected, "got {triples:?}");
}

// ---------------------------------------------------------------------------
// Anchored pattern
// ---------------------------------------------------------------------------

#[test]
fn anchored_pattern_restricts_to_a_specific_starting_entity() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());
    let r = fx.r;
    let m = fx.m;

    let pairs = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let mut builder = PatternBuilder::new();
            let a = builder.node(|_| true);
            let b = builder.node(|_| true);
            builder.anchor(a, r);
            let _e = builder.edge(a, b);
            let pattern = builder.build();
            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| (m.node(a), m.node(b)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    // Anchored at r: only r → m matches.
    assert_eq!(pairs, vec![(r, m)]);
}

// ---------------------------------------------------------------------------
// Node predicate (filter to Rectangle as the source)
// ---------------------------------------------------------------------------

#[test]
fn node_predicate_filters_pattern_matches_by_component_type() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());

    let pairs = app
        .world_mut()
        .run_system_once(move |scene: SceneView, rects: Query<&Rectangle>| {
            let mut builder = PatternBuilder::new();
            // A must carry the `Rectangle` component.
            let a = builder.node(|e| rects.get(e).is_ok());
            let b = builder.node(|_| true);
            let _e = builder.edge(a, b);
            let pattern = builder.build();
            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| (m.node(a), m.node(b)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    // Only `m` is a Rectangle, and it has two outgoing edges (m→t, m→x).
    let mut sorted = pairs.clone();
    sorted.sort();
    let mut expected = vec![(fx.m, fx.t), (fx.m, fx.x)];
    expected.sort();
    assert_eq!(sorted, expected, "got {pairs:?}");
}

// ---------------------------------------------------------------------------
// Fork (A → B AND A → C) — two outgoing edges from one node
// ---------------------------------------------------------------------------

#[test]
fn fork_pattern_finds_nodes_with_two_distinct_successors() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());

    let triples = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let mut builder = PatternBuilder::new();
            let a = builder.node(|_| true);
            let b = builder.node(|_| true);
            let c = builder.node(|_| true);
            let _ab = builder.edge(a, b);
            let _ac = builder.edge(a, c);
            let pattern = builder.build();
            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| (m.node(a), m.node(b), m.node(c)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    // Only `m` has ≥2 distinct outgoing successors. Injective on nodes
    // means (B, C) ∈ {(t, x), (x, t)} — two matches.
    let mut sorted = triples.clone();
    sorted.sort();
    let mut expected = vec![(fx.m, fx.t, fx.x), (fx.m, fx.x, fx.t)];
    expected.sort();
    assert_eq!(sorted, expected, "got {triples:?}");
}

// ---------------------------------------------------------------------------
// No-match pattern (Frame → Frame)
// ---------------------------------------------------------------------------

#[test]
fn no_match_when_pattern_predicates_are_unsatisfiable() {
    let mut app = test_app();
    let _ = spawn_fixture(app.world_mut());

    let count = app
        .world_mut()
        .run_system_once(|scene: SceneView, frames: Query<&Frame>| {
            let mut builder = PatternBuilder::new();
            // Both endpoints must be Frame components. Our fixture
            // has only one Frame (r) — no Frame→Frame edge possible.
            let a = builder.node(|e| frames.get(e).is_ok());
            let b = builder.node(|e| frames.get(e).is_ok());
            let _ab = builder.edge(a, b);
            let pattern = builder.build();
            scene.graph.subgraph_matches(&pattern).count()
        })
        .expect("query runs");

    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// Edge predicate
// ---------------------------------------------------------------------------

#[test]
fn edge_predicate_restricts_to_specific_edge_entities() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());

    // Capture the specific edge entity we want to allow (r → m).
    let target_edge = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            scene.graph.find_edge(fx.r, fx.m).expect("r→m edge exists")
        })
        .expect("query runs");

    let pairs = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let mut builder = PatternBuilder::new();
            let a = builder.node(|_| true);
            let b = builder.node(|_| true);
            // Only the r→m edge entity satisfies the predicate.
            let _e = builder.edge_where(a, b, move |e| e == target_edge);
            let pattern = builder.build();
            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| (m.node(a), m.node(b)))
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    assert_eq!(pairs, vec![(fx.r, fx.m)]);
}

// ---------------------------------------------------------------------------
// Iterator semantics: a match binds edge entities too
// ---------------------------------------------------------------------------

#[test]
fn match_exposes_bound_edge_entities() {
    let mut app = test_app();
    let fx = spawn_fixture(app.world_mut());

    let edges_in_path = app
        .world_mut()
        .run_system_once(move |scene: SceneView| {
            let mut builder = PatternBuilder::new();
            let a = builder.node(|_| true);
            let b = builder.node(|_| true);
            let c = builder.node(|_| true);
            // Anchor at r so the only viable path is r → m → {t,x};
            // we end up with two matches, one per terminal.
            builder.anchor(a, fx.r);
            let ab = builder.edge(a, b);
            let bc = builder.edge(b, c);
            let pattern = builder.build();

            scene
                .graph
                .subgraph_matches(&pattern)
                .map(|m: Match| {
                    // Each match binds *concrete* edge entities, not
                    // just the (from, to) node pairs.
                    let e_ab = m.edge(ab);
                    let e_bc = m.edge(bc);
                    (m.node(c), e_ab, e_bc)
                })
                .collect::<Vec<_>>()
        })
        .expect("query runs");

    // Two matches: ending at t or x. The first edge (a→b) is always
    // the r→m edge entity; the second varies.
    assert_eq!(edges_in_path.len(), 2);
    // Same first edge entity bound in both matches.
    assert_eq!(edges_in_path[0].1, edges_in_path[1].1);
    // The terminal node distinguishes the two matches.
    let terminals: std::collections::HashSet<Entity> =
        edges_in_path.iter().map(|(c, _, _)| *c).collect();
    assert!(terminals.contains(&fx.t));
    assert!(terminals.contains(&fx.x));
}
