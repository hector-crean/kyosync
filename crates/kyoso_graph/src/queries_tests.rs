//! Tests for graph query traversal functionality.

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use crate::components::{IncomingEdges, OutgoingEdges};
    use crate::queries::GraphQuery;
    use crate::traverse::{GraphTraverse, Step, TraversalNode};
    use crate::commands::spawn_edge;

    #[derive(Component, Default, Clone, Debug)]
    struct TestNode {
        id: u32,
    }

    #[derive(Component, Default, Clone, Debug)]
    struct TestEdge;

    #[derive(Resource)]
    struct TestGraphData {
        a: Entity,
        b: Entity,
        c: Entity,
        d: Entity,
    }

    fn setup_test_graph(world: &mut World) {
        // Create a simple diamond graph:
        //     A
        //    / \
        //   B   C
        //    \ /
        //     D

        let a = world.spawn((TestNode { id: 0 }, OutgoingEdges::default(), IncomingEdges::default())).id();
        let b = world.spawn((TestNode { id: 1 }, OutgoingEdges::default(), IncomingEdges::default())).id();
        let c = world.spawn((TestNode { id: 2 }, OutgoingEdges::default(), IncomingEdges::default())).id();
        let d = world.spawn((TestNode { id: 3 }, OutgoingEdges::default(), IncomingEdges::default())).id();

        spawn_edge(&mut world.commands(), a, b);
        spawn_edge(&mut world.commands(), a, c);
        spawn_edge(&mut world.commands(), b, d);
        spawn_edge(&mut world.commands(), c, d);

        world.flush();

        world.insert_resource(TestGraphData { a, b, c, d });
    }

    fn test_bfs_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        let nodes: Vec<TraversalNode> = query.bfs_iter_with_depth(graph_data.a).collect();

        assert_eq!(nodes.len(), 4);

        // First node should be A at depth 0
        assert_eq!(nodes[0].entity, graph_data.a);
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[0].parent, None);

        // B and C should be at depth 1
        let depth_1_nodes: Vec<_> = nodes.iter().filter(|n| n.depth == 1).collect();
        assert_eq!(depth_1_nodes.len(), 2);

        // D should be at depth 2
        let depth_2_node = nodes.iter().find(|n| n.depth == 2).unwrap();
        assert_eq!(depth_2_node.entity, graph_data.d);
        assert!(depth_2_node.parent == Some(graph_data.b) || depth_2_node.parent == Some(graph_data.c));
    }

    fn test_dfs_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        let nodes: Vec<TraversalNode> = query.dfs_iter_with_depth(graph_data.a).collect();

        assert_eq!(nodes.len(), 4);

        // First node should be A
        assert_eq!(nodes[0].entity, graph_data.a);
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[0].parent, None);

        // All nodes should have valid depths
        for node in &nodes {
            assert!(node.depth <= 2);
        }
    }

    fn test_component_extraction_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        // Test get_node
        assert!(query.get_node(graph_data.a).is_some());
        assert!(query.get_node(graph_data.b).is_some());

        // Test neighbors_with_data
        let neighbors: Vec<_> = query.neighbors_with_data(graph_data.a).collect();
        assert_eq!(neighbors.len(), 2); // B and C
    }

    #[test]
    fn test_bfs_iter_with_depth() {
        let mut world = World::new();
        setup_test_graph(&mut world);

        let mut schedule = Schedule::default();
        schedule.add_systems(test_bfs_system);
        schedule.run(&mut world);
    }

    #[test]
    fn test_dfs_iter_with_depth() {
        let mut world = World::new();
        setup_test_graph(&mut world);

        let mut schedule = Schedule::default();
        schedule.add_systems(test_dfs_system);
        schedule.run(&mut world);
    }

    #[test]
    fn test_component_extraction() {
        let mut world = World::new();
        setup_test_graph(&mut world);

        let mut schedule = Schedule::default();
        schedule.add_systems(test_component_extraction_system);
        schedule.run(&mut world);
    }

    // ====================================================================
    // bfs_walk semantics
    // ====================================================================

    fn test_bfs_walk_stop_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        // Stop on B: should yield A then B and halt before exploring C or D.
        let collected: Vec<TraversalNode> = query
            .bfs_walk(graph_data.a, |node| {
                if node.entity == graph_data.b {
                    Step::Stop
                } else {
                    Step::Visit
                }
            })
            .collect();

        // A is yielded; B is yielded (Stop includes the node) then walk halts.
        // Because BFS visits siblings together, C may or may not be queued
        // before B is popped — but Stop on B prevents any further iteration.
        assert!(collected.iter().any(|n| n.entity == graph_data.a));
        assert!(collected.iter().any(|n| n.entity == graph_data.b));
        // D was never reached.
        assert!(collected.iter().all(|n| n.entity != graph_data.d));
    }

    fn test_bfs_walk_prune_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        // Prune at B: B is hidden AND its subtree under B isn't expanded.
        // D is still reachable via C, so D appears in the result.
        let collected: Vec<Entity> = query
            .bfs_walk(graph_data.a, |node| {
                if node.entity == graph_data.b {
                    Step::Prune
                } else {
                    Step::Visit
                }
            })
            .map(|n| n.entity)
            .collect();

        assert!(collected.contains(&graph_data.a));
        assert!(!collected.contains(&graph_data.b), "B should be pruned");
        assert!(collected.contains(&graph_data.c));
        assert!(collected.contains(&graph_data.d), "D still reachable via C");
    }

    fn test_bfs_walk_skip_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        // Skip at C: C is yielded but its successor (D) is not expanded
        // through C. D is still reached via B → D.
        let collected: Vec<Entity> = query
            .bfs_walk(graph_data.a, |node| {
                if node.entity == graph_data.c {
                    Step::Skip
                } else {
                    Step::Visit
                }
            })
            .map(|n| n.entity)
            .collect();

        assert!(collected.contains(&graph_data.a));
        assert!(collected.contains(&graph_data.b));
        assert!(collected.contains(&graph_data.c), "C is yielded under Skip");
        assert!(collected.contains(&graph_data.d), "D reachable via B");
    }

    #[test]
    fn test_bfs_walk_stop() {
        let mut world = World::new();
        setup_test_graph(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(test_bfs_walk_stop_system);
        schedule.run(&mut world);
    }

    #[test]
    fn test_bfs_walk_prune() {
        let mut world = World::new();
        setup_test_graph(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(test_bfs_walk_prune_system);
        schedule.run(&mut world);
    }

    #[test]
    fn test_bfs_walk_skip() {
        let mut world = World::new();
        setup_test_graph(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(test_bfs_walk_skip_system);
        schedule.run(&mut world);
    }

    // ====================================================================
    // find_paths_matching
    // ====================================================================

    fn test_find_paths_matching_system(
        query: GraphQuery<&TestNode, &TestEdge>,
        graph_data: Res<TestGraphData>,
    ) {
        // Paths of length 3 from A: must end at D. Two paths exist:
        //   A → B → D
        //   A → C → D
        let paths = query.find_paths_matching(graph_data.a, 3, |i, e| match i {
            0 => e == graph_data.a,
            1 => e == graph_data.b || e == graph_data.c,
            2 => e == graph_data.d,
            _ => false,
        });
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p == &vec![graph_data.a, graph_data.b, graph_data.d]));
        assert!(paths.iter().any(|p| p == &vec![graph_data.a, graph_data.c, graph_data.d]));

        // Paths of length 2: A → B and A → C.
        let two_step = query.find_paths_matching(graph_data.a, 2, |_, _| true);
        assert_eq!(two_step.len(), 2);

        // Constraint that fails at position 0 yields nothing.
        let empty = query.find_paths_matching(graph_data.a, 2, |i, _| i != 0);
        assert!(empty.is_empty());

        // path_len == 0 yields nothing.
        let zero = query.find_paths_matching(graph_data.a, 0, |_, _| true);
        assert!(zero.is_empty());
    }

    #[test]
    fn test_find_paths_matching() {
        let mut world = World::new();
        setup_test_graph(&mut world);
        let mut schedule = Schedule::default();
        schedule.add_systems(test_find_paths_matching_system);
        schedule.run(&mut world);
    }
}
