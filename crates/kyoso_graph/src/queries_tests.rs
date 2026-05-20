//! Tests for graph query traversal functionality.

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use crate::components::{IncomingEdges, OutgoingEdges};
    use crate::queries::{GraphQuery, TraversalNode};
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
}
