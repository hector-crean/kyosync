//! Scene graph descriptor system for AI agents and ML workflows.
//!
//! Provides serializable representations of scene graphs optimized for:
//! - Direct LLM consumption (hierarchical JSON)
//! - RAG/vector search (embeddings)
//! - Graph ML (GNN training)

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::tree::TreeQuery;

// ============================================================================
// Core Descriptor Types (for LLM consumption)
// ============================================================================

/// Serializable scene graph description for AI agents.
///
/// This format is optimized for direct consumption by LLMs, providing a
/// hierarchical view of the scene with nested children.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SceneGraphDescriptor {
    /// Graph metadata
    pub metadata: GraphMetadata,
    /// Root nodes of the scene
    pub roots: Vec<NodeDescriptor>,
}

/// Metadata about the graph structure.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphMetadata {
    /// Total number of nodes
    pub node_count: usize,
    /// Number of root nodes
    pub root_count: usize,
    /// Maximum depth of the tree
    pub max_depth: usize,
    /// Whether this is an acyclic graph
    pub is_acyclic: bool,
    /// Whether this is a tree (single parent per node)
    pub is_tree: bool,
}

/// Descriptor for a single node in the scene graph.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NodeDescriptor {
    /// Entity ID (as a string for serialization)
    pub id: String,
    /// Node type (from component type name)
    pub node_type: String,
    /// Depth from nearest root
    pub depth: usize,
    /// Ordered children (nested structure)
    pub children: Vec<NodeDescriptor>,
    /// Optional component data as JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// ============================================================================
// Builder Implementation
// ============================================================================

impl SceneGraphDescriptor {
    /// Build a descriptor from a scene graph.
    ///
    /// This performs a depth-first traversal of the scene, starting from roots,
    /// and builds a hierarchical JSON-ready structure.
    ///
    /// **Parameters:**
    /// - `scene`: The scene graph to describe
    /// - `include_data`: Whether to include component data (via reflection)
    ///
    /// **Note:** Component data extraction via reflection is not yet implemented.
    /// For now, `include_data` is accepted but ignored. This will be added in a
    /// future iteration.
    pub fn from_scene_graph(
        tree: &TreeQuery,
        _include_data: bool, // TODO: implement reflection-based data extraction
    ) -> Self {
        let roots = tree.roots();
        let node_count = tree.node_count();
        let root_count = roots.len();
        let max_depth = tree.max_depth();

        let root_descriptors: Vec<NodeDescriptor> = roots
            .into_iter()
            .map(|root| Self::build_node_descriptor(tree, root))
            .collect();

        SceneGraphDescriptor {
            metadata: GraphMetadata {
                node_count,
                root_count,
                max_depth,
                // Scene graphs are trees by construction
                is_acyclic: true,
                is_tree: true,
            },
            roots: root_descriptors,
        }
    }

    /// Recursively build a node descriptor with all its children.
    fn build_node_descriptor(tree: &TreeQuery, entity: Entity) -> NodeDescriptor {
        let depth = tree.depth(entity);
        let children_entities = tree.children(entity);

        let children: Vec<NodeDescriptor> = children_entities
            .into_iter()
            .map(|child| Self::build_node_descriptor(tree, child))
            .collect();

        NodeDescriptor {
            id: format!("{:?}", entity),
            // TODO: Get actual component type name via reflection
            node_type: "Node".to_string(),
            depth,
            children,
            // TODO: Extract component data via reflection
            data: None,
        }
    }

    /// Export as compact JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Export as formatted JSON (pretty-printed).
    ///
    /// This format is easier for humans to read and better for LLM consumption.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

// ============================================================================
// Statistics & Utilities
// ============================================================================

impl SceneGraphDescriptor {
    /// Count total nodes in the descriptor (including nested children).
    pub fn count_nodes(&self) -> usize {
        self.roots.iter().map(|root| Self::count_nodes_recursive(root)).sum()
    }

    fn count_nodes_recursive(node: &NodeDescriptor) -> usize {
        1 + node.children.iter().map(Self::count_nodes_recursive).sum::<usize>()
    }

    /// Get all node IDs as a flat list.
    pub fn collect_node_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for root in &self.roots {
            Self::collect_ids_recursive(root, &mut ids);
        }
        ids
    }

    fn collect_ids_recursive(node: &NodeDescriptor, ids: &mut Vec<String>) {
        ids.push(node.id.clone());
        for child in &node.children {
            Self::collect_ids_recursive(child, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptor_serialization() {
        let descriptor = SceneGraphDescriptor {
            metadata: GraphMetadata {
                node_count: 3,
                root_count: 1,
                max_depth: 2,
                is_acyclic: true,
                is_tree: true,
            },
            roots: vec![NodeDescriptor {
                id: "1".to_string(),
                node_type: "Root".to_string(),
                depth: 0,
                children: vec![NodeDescriptor {
                    id: "2".to_string(),
                    node_type: "Child".to_string(),
                    depth: 1,
                    children: vec![],
                    data: None,
                }],
                data: None,
            }],
        };

        let json = descriptor.to_json().unwrap();
        assert!(json.contains("\"node_count\":3"));

        let json_pretty = descriptor.to_json_pretty().unwrap();
        assert!(json_pretty.contains("\"node_type\": \"Root\""));
    }

    #[test]
    fn test_count_nodes() {
        let descriptor = SceneGraphDescriptor {
            metadata: GraphMetadata {
                node_count: 3,
                root_count: 1,
                max_depth: 2,
                is_acyclic: true,
                is_tree: true,
            },
            roots: vec![NodeDescriptor {
                id: "1".to_string(),
                node_type: "Root".to_string(),
                depth: 0,
                children: vec![
                    NodeDescriptor {
                        id: "2".to_string(),
                        node_type: "Child".to_string(),
                        depth: 1,
                        children: vec![],
                        data: None,
                    },
                    NodeDescriptor {
                        id: "3".to_string(),
                        node_type: "Child".to_string(),
                        depth: 1,
                        children: vec![],
                        data: None,
                    },
                ],
                data: None,
            }],
        };

        assert_eq!(descriptor.count_nodes(), 3);
        let ids = descriptor.collect_node_ids();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"1".to_string()));
        assert!(ids.contains(&"2".to_string()));
        assert!(ids.contains(&"3".to_string()));
    }
}
