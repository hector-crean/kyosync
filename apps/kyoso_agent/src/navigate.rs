//! `navigate` — local-neighborhood query for iterative graph crawls.
//!
//! Sits between `inspect` (single node) and `walk` (subtree). Lets the
//! agent move one step at a time across either the tree (ChildOf) or
//! the entity-edge graph (SceneEdge) without materialising whole
//! subtrees. The result is always `Vec<NodeRef>` ready to feed back
//! into the next call — the actual crawl shape.

use bevy::prelude::*;
use kyoso_core::{NodeKind, SceneNode, SceneWorld};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

use crate::handle::{node_ref_for, NodeRef, ScenePath, SessionId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum NavDir {
    /// Outgoing edges only — successors I point to.
    #[default]
    Out,
    /// Incoming edges only — predecessors pointing to me.
    In,
    /// Union of Out and In at each hop. Undirected neighborhood.
    Both,
    /// Transitive closure of `Out`. Ignores `depth_limit` (uses
    /// `GraphQuery::downstream_nodes`).
    Downstream,
    /// Transitive closure of `In`. Ignores `depth_limit`.
    Upstream,
    /// Whole connected component (undirected). Ignores `depth_limit`.
    Component,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum NavEdgeFilter {
    /// Tree (ChildOf) + entity-edges (SceneEdge). Default.
    #[default]
    All,
    /// Only the tree hierarchy. For walking a subtree manually one
    /// hop at a time. (`walk` is usually better for whole subtrees.)
    TreeOnly,
    /// Only the entity-edge graph — the "weave" relations that don't
    /// live in the parent-child hierarchy. This is where most
    /// agent-relevant graph crawls happen.
    EntityEdgesOnly,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct NavOpts {
    pub direction: NavDir,
    pub edges: NavEdgeFilter,
    /// Filter result rows by kind. Empty = all kinds.
    pub kinds: Vec<NodeKind>,
    /// Hop depth. `None` defaults to 1 (immediate neighbors only).
    /// Ignored for `Downstream` / `Upstream` / `Component`.
    pub depth_limit: Option<u32>,
    /// Cap on row count. 0 = unlimited.
    pub max_items: u32,
}

pub fn run_navigate(
    sw: &mut SceneWorld,
    session: SessionId,
    from: Entity,
    opts: &NavOpts,
) -> Vec<NodeRef> {
    let entities = match opts.direction {
        NavDir::Out | NavDir::In | NavDir::Both => {
            bounded_bfs(sw, from, opts)
        }
        NavDir::Downstream => transitive(sw, from, NavDir::Out, opts.edges),
        NavDir::Upstream => transitive(sw, from, NavDir::In, opts.edges),
        NavDir::Component => connected_component(sw, from, opts.edges),
    };

    let mut rows = Vec::with_capacity(entities.len());
    let max = if opts.max_items == 0 {
        usize::MAX
    } else {
        opts.max_items as usize
    };
    let kind_filter = &opts.kinds;

    for e in entities {
        if !kind_filter.is_empty() {
            let kind = sw.world().get::<NodeKind>(e).copied();
            match kind {
                Some(k) if kind_filter.contains(&k) => {}
                _ => continue,
            }
        }
        if rows.len() >= max {
            break;
        }
        let r = node_ref_for(sw, e, session)
            .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
        rows.push(r);
    }

    rows
}

// =============================================================================
// Internals — neighbour enumeration
// =============================================================================

/// One hop in the given direction(s) under the given edge filter.
/// Returns *distinct* neighbours (no duplicates if a node is reachable
/// via two edges).
fn neighbours_once(
    sw: &mut SceneWorld,
    node: Entity,
    dir: NavDir,
    edges: NavEdgeFilter,
) -> Vec<Entity> {
    let mut out: Vec<Entity> = Vec::new();
    let mut seen: HashSet<Entity> = HashSet::new();

    let want_out = matches!(dir, NavDir::Out | NavDir::Both);
    let want_in = matches!(dir, NavDir::In | NavDir::Both);

    // Tree edges (ChildOf hierarchy).
    if matches!(edges, NavEdgeFilter::All | NavEdgeFilter::TreeOnly) {
        if want_out {
            if let Some(children) = sw.world().get::<Children>(node) {
                for c in children.iter() {
                    if sw.world().get::<SceneNode>(c).is_some() && seen.insert(c) {
                        out.push(c);
                    }
                }
            }
        }
        if want_in {
            if let Some(parent) = sw.world().get::<ChildOf>(node) {
                if sw.world().get::<SceneNode>(parent.0).is_some() && seen.insert(parent.0) {
                    out.push(parent.0);
                }
            }
        }
    }

    // Entity-edge graph (SceneEdge).
    if matches!(edges, NavEdgeFilter::All | NavEdgeFilter::EntityEdgesOnly) {
        let view = sw.scene_view();
        let q = &view.scene().graph;
        if want_out {
            for n in q.neighbors(node) {
                if seen.insert(n) {
                    out.push(n);
                }
            }
        }
        if want_in {
            for n in q.predecessors(node) {
                if seen.insert(n) {
                    out.push(n);
                }
            }
        }
    }

    out
}

fn bounded_bfs(sw: &mut SceneWorld, from: Entity, opts: &NavOpts) -> Vec<Entity> {
    let depth = opts.depth_limit.unwrap_or(1);
    if depth == 0 {
        return Vec::new();
    }

    let mut visited: HashSet<Entity> = HashSet::new();
    visited.insert(from);
    let mut order: Vec<Entity> = Vec::new();
    let mut queue: VecDeque<(Entity, u32)> = VecDeque::new();
    queue.push_back((from, 0));

    while let Some((node, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        let next = neighbours_once(sw, node, opts.direction, opts.edges);
        for n in next {
            if visited.insert(n) {
                order.push(n);
                queue.push_back((n, d + 1));
            }
        }
    }

    order
}

fn transitive(
    sw: &mut SceneWorld,
    from: Entity,
    dir: NavDir,
    edges: NavEdgeFilter,
) -> Vec<Entity> {
    let opts = NavOpts {
        direction: dir,
        edges,
        kinds: Vec::new(),
        depth_limit: Some(u32::MAX),
        max_items: 0,
    };
    bounded_bfs(sw, from, &opts)
}

fn connected_component(
    sw: &mut SceneWorld,
    from: Entity,
    edges: NavEdgeFilter,
) -> Vec<Entity> {
    transitive(sw, from, NavDir::Both, edges)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_demo_scene;

    #[test]
    fn out_navigation_finds_children() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();
        let rows = run_navigate(
            &mut sw,
            session,
            ents.root,
            &NavOpts {
                direction: NavDir::Out,
                edges: NavEdgeFilter::TreeOnly,
                ..Default::default()
            },
        );
        assert_eq!(rows.len(), 2, "Root has 2 children (header, body)");
    }

    #[test]
    fn in_navigation_finds_parent() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();
        let rows = run_navigate(
            &mut sw,
            session,
            ents.label,
            &NavOpts {
                direction: NavDir::In,
                edges: NavEdgeFilter::TreeOnly,
                ..Default::default()
            },
        );
        assert_eq!(rows.len(), 1, "label has one parent (header)");
    }

    #[test]
    fn entity_edge_out_finds_target() {
        // Demo scene has a label→body_caption SceneEdge.
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();
        let rows = run_navigate(
            &mut sw,
            session,
            ents.label,
            &NavOpts {
                direction: NavDir::Out,
                edges: NavEdgeFilter::EntityEdgesOnly,
                ..Default::default()
            },
        );
        assert_eq!(rows.len(), 1, "label has 1 entity-edge → body_caption");
    }

    #[test]
    fn upstream_walks_all_ancestors() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();
        let rows = run_navigate(
            &mut sw,
            session,
            ents.label,
            &NavOpts {
                direction: NavDir::Upstream,
                edges: NavEdgeFilter::TreeOnly,
                ..Default::default()
            },
        );
        // label → header → root
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn kind_filter_restricts_results() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();
        let rows = run_navigate(
            &mut sw,
            session,
            ents.root,
            &NavOpts {
                direction: NavDir::Downstream,
                edges: NavEdgeFilter::TreeOnly,
                kinds: vec![NodeKind::Text],
                ..Default::default()
            },
        );
        assert_eq!(rows.len(), 2, "two Text nodes under root");
    }
}
