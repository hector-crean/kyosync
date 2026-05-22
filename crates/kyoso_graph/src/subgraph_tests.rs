//! Tests for streaming subgraph matching.

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use std::collections::HashSet;

    use crate::commands::spawn_edge;
    use crate::components::{IncomingEdges, OutgoingEdges};
    use crate::cost::CostHint;
    use crate::pattern::{Direction, PatternBuilder};
    use crate::queries::GraphQuery;
    use crate::traverse::GraphTraverseEdges;

    #[derive(Component, Default, Clone, Debug)]
    struct TestNode;

    #[derive(Component, Default, Clone, Debug)]
    struct TestEdge;

    #[derive(Resource, Clone, Copy)]
    struct GraphIds {
        a: Entity,
        b: Entity,
        c: Entity,
        d: Entity,
    }

    fn setup_diamond(world: &mut World) {
        // A → B, A → C, B → D, C → D
        let a = world
            .spawn((TestNode, OutgoingEdges::default(), IncomingEdges::default()))
            .id();
        let b = world
            .spawn((TestNode, OutgoingEdges::default(), IncomingEdges::default()))
            .id();
        let c = world
            .spawn((TestNode, OutgoingEdges::default(), IncomingEdges::default()))
            .id();
        let d = world
            .spawn((TestNode, OutgoingEdges::default(), IncomingEdges::default()))
            .id();

        spawn_edge(&mut world.commands(), a, b);
        spawn_edge(&mut world.commands(), a, c);
        spawn_edge(&mut world.commands(), b, d);
        spawn_edge(&mut world.commands(), c, d);
        world.flush();

        world.insert_resource(GraphIds { a, b, c, d });
    }

    // -----------------------------------------------------------------
    // single-node, unanchored: 4 matches
    // -----------------------------------------------------------------
    fn sys_single_node_unanchored(query: GraphQuery<&TestNode, &TestEdge>) {
        let mut b = PatternBuilder::new();
        let _n = b.node(|_| true);
        let pattern = b.build();

        let matches: Vec<_> = query.subgraph_matches(&pattern).collect();
        assert_eq!(matches.len(), 4, "should match each of the 4 nodes once");
    }

    #[test]
    fn single_node_unanchored_matches_all_nodes() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_single_node_unanchored);
        schedule.run(&mut world);
    }

    // -----------------------------------------------------------------
    // 2-node A → ?: 2 matches (A→B, A→C)
    // -----------------------------------------------------------------
    fn sys_anchored_two_node(query: GraphQuery<&TestNode, &TestEdge>, ids: Res<GraphIds>) {
        let mut b = PatternBuilder::new();
        let p_a = b.node(|_| true);
        let p_x = b.node(|_| true);
        b.edge(p_a, p_x);
        b.anchor(p_a, ids.a);
        let pattern = b.build();

        let matches: Vec<_> = query.subgraph_matches(&pattern).collect();
        assert_eq!(matches.len(), 2);
        let targets: HashSet<Entity> = matches.iter().map(|m| m.node(p_x)).collect();
        assert!(targets.contains(&ids.b));
        assert!(targets.contains(&ids.c));
    }

    #[test]
    fn anchored_two_node_matches_outgoing() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_anchored_two_node);
        schedule.run(&mut world);
    }

    // -----------------------------------------------------------------
    // 3-chain unanchored: a → b → c. From A: A→B→D and A→C→D.
    // -----------------------------------------------------------------
    fn sys_three_chain(query: GraphQuery<&TestNode, &TestEdge>, ids: Res<GraphIds>) {
        let mut b = PatternBuilder::new();
        let p_a = b.node(|_| true);
        let p_b = b.node(|_| true);
        let p_c = b.node(|_| true);
        b.edge(p_a, p_b);
        b.edge(p_b, p_c);
        let pattern = b.build();

        let matches: Vec<_> = query.subgraph_matches(&pattern).collect();

        // Triples reachable: A→B→D, A→C→D. (D has no outgoing.)
        let triples: HashSet<(Entity, Entity, Entity)> = matches
            .iter()
            .map(|m| (m.node(p_a), m.node(p_b), m.node(p_c)))
            .collect();
        assert!(triples.contains(&(ids.a, ids.b, ids.d)));
        assert!(triples.contains(&(ids.a, ids.c, ids.d)));
        assert_eq!(triples.len(), 2);
    }

    #[test]
    fn three_chain_matches_two_triples() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_three_chain);
        schedule.run(&mut world);
    }

    // -----------------------------------------------------------------
    // Full diamond pattern: a→b, a→c, b→d, c→d, with b ≠ c (injectivity).
    // Should match exactly once: (A, B, C, D) or (A, C, B, D).
    // Each unordered match shows up twice because the pattern is
    // symmetric in (b, c). So we expect 2 matches.
    // -----------------------------------------------------------------
    fn sys_diamond_pattern(query: GraphQuery<&TestNode, &TestEdge>, ids: Res<GraphIds>) {
        let mut b = PatternBuilder::new();
        let p_a = b.node(|_| true);
        let p_b = b.node(|_| true);
        let p_c = b.node(|_| true);
        let p_d = b.node(|_| true);
        b.edge(p_a, p_b);
        b.edge(p_a, p_c);
        b.edge(p_b, p_d);
        b.edge(p_c, p_d);
        let pattern = b.build();

        let matches: Vec<_> = query.subgraph_matches(&pattern).collect();

        // 2 because b and c are interchangeable in the pattern.
        assert_eq!(matches.len(), 2);
        for m in &matches {
            assert_eq!(m.node(p_a), ids.a);
            assert_eq!(m.node(p_d), ids.d);
            assert!(m.node(p_b) == ids.b || m.node(p_b) == ids.c);
            assert!(m.node(p_c) == ids.b || m.node(p_c) == ids.c);
            assert_ne!(m.node(p_b), m.node(p_c), "injective");
        }
    }

    #[test]
    fn diamond_pattern_matches_once_per_symmetry() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_diamond_pattern);
        schedule.run(&mut world);
    }

    // -----------------------------------------------------------------
    // Direction::Backward: anchor at D, walk one backward edge.
    // D has 2 incoming (from B and from C). With pattern
    // `target -[backward]-> source` anchored at target=D, candidates
    // should be B and C.
    // -----------------------------------------------------------------
    fn sys_backward_edge(query: GraphQuery<&TestNode, &TestEdge>, ids: Res<GraphIds>) {
        let mut b = PatternBuilder::new();
        let p_d = b.node(|_| true);
        let p_x = b.node(|_| true);
        // Pattern arrow p_d -> p_x, but Backward → graph edge points p_x -> p_d.
        b.edge_dir(p_d, p_x, Direction::Backward);
        b.anchor(p_d, ids.d);
        let pattern = b.build();

        let matches: Vec<_> = query.subgraph_matches(&pattern).collect();
        let sources: HashSet<Entity> = matches.iter().map(|m| m.node(p_x)).collect();
        assert_eq!(sources.len(), 2);
        assert!(sources.contains(&ids.b));
        assert!(sources.contains(&ids.c));
    }

    #[test]
    fn backward_edge_matches_predecessors() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_backward_edge);
        schedule.run(&mut world);
    }

    // -----------------------------------------------------------------
    // Predicate that filters out a specific entity.
    // -----------------------------------------------------------------
    fn sys_node_pred_filter(query: GraphQuery<&TestNode, &TestEdge>, ids: Res<GraphIds>) {
        let exclude = ids.b;
        let mut b = PatternBuilder::new();
        let p_a = b.node(|_| true);
        let p_x = b.node(move |e| e != exclude);
        b.edge(p_a, p_x);
        b.anchor(p_a, ids.a);
        let pattern = b.build();

        let matches: Vec<_> = query.subgraph_matches(&pattern).collect();
        let targets: HashSet<Entity> = matches.iter().map(|m| m.node(p_x)).collect();
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&ids.c));
    }

    #[test]
    fn node_predicate_excludes_match() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_node_pred_filter);
        schedule.run(&mut world);
    }

    // -----------------------------------------------------------------
    // CostHint smoke test: unanchored 2-node pattern over 4 nodes
    // gives an estimate > 0 for both items and work.
    // -----------------------------------------------------------------
    fn sys_cost_hint(query: GraphQuery<&TestNode, &TestEdge>) {
        let mut b = PatternBuilder::new();
        let p_a = b.node(|_| true);
        let p_b = b.node(|_| true);
        b.edge(p_a, p_b);
        let pattern = b.build();

        let it = query.subgraph_matches(&pattern);
        let cost = it.cost();
        assert!(cost.estimated_items > 0);
        assert!(cost.estimated_work > 0);
    }

    #[test]
    fn cost_hint_nonzero() {
        let mut world = World::new();
        setup_diamond(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(sys_cost_hint);
        schedule.run(&mut world);
    }
}
