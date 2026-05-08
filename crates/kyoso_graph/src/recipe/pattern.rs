//! Pattern definitions for subgraph matching.
//!
//! A [`Pattern`] is a small directed graph whose nodes and edges carry
//! *predicates* rather than concrete values.  The matcher tests every
//! subgraph of the main graph against these predicates.

use petgraph::graph::{DiGraph, NodeIndex};

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// Predicate applied to a node during subgraph matching.
pub type NodePredicate = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Predicate applied to an edge during subgraph matching.
/// Receives (edge_order, from_symbol, to_symbol).
pub type EdgePredicate = Box<dyn Fn(u8, &str, &str) -> bool + Send + Sync>;

// ---------------------------------------------------------------------------
// PatternNode / PatternEdge
// ---------------------------------------------------------------------------

pub struct PatternNode {
    /// An opaque label for human readability.
    pub label: String,
    /// If `None`, the node matches anything.
    pub predicate: Option<NodePredicate>,
}

impl PatternNode {
    pub fn any(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            predicate: None,
        }
    }

    pub fn with_symbol(label: impl Into<String>, expected: impl Into<String>) -> Self {
        let expected: String = expected.into();
        Self {
            label: label.into(),
            predicate: Some(Box::new(move |sym| sym == expected)),
        }
    }

    pub fn with_predicate(
        label: impl Into<String>,
        pred: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            predicate: Some(Box::new(pred)),
        }
    }

    pub fn matches(&self, symbol: &str) -> bool {
        self.predicate.as_ref().map_or(true, |p| p(symbol))
    }
}

pub struct PatternEdge {
    pub label: String,
    pub predicate: Option<EdgePredicate>,
}

impl PatternEdge {
    pub fn any(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            predicate: None,
        }
    }

    pub fn with_order(label: impl Into<String>, expected_order: u8) -> Self {
        Self {
            label: label.into(),
            predicate: Some(Box::new(move |order, _, _| order == expected_order)),
        }
    }

    pub fn with_predicate(
        label: impl Into<String>,
        pred: impl Fn(u8, &str, &str) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            predicate: Some(Box::new(pred)),
        }
    }

    pub fn matches(&self, order: u8, from_sym: &str, to_sym: &str) -> bool {
        self.predicate
            .as_ref()
            .map_or(true, |p| p(order, from_sym, to_sym))
    }
}

// ---------------------------------------------------------------------------
// Pattern
// ---------------------------------------------------------------------------

/// A small template graph used for subgraph matching.
pub struct Pattern {
    pub name: String,
    pub graph: DiGraph<PatternNode, PatternEdge>,
}

impl Pattern {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            graph: DiGraph::new(),
        }
    }

    pub fn add_node(&mut self, node: PatternNode) -> NodeIndex {
        self.graph.add_node(node)
    }

    pub fn add_edge(&mut self, from: NodeIndex, to: NodeIndex, edge: PatternEdge) {
        self.graph.add_edge(from, to, edge);
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

