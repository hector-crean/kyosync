//! `scan` — the catalog + outline an agent reads first to plan its
//! drill-down. Three-tier model:
//!
//! - [`Catalog`]: counts per `NodeKind`, total nodes, max depth.
//!   Tiny — fits in ~200 bytes regardless of scene size.
//! - [`OutlineNode`] forest: depth-bounded skeleton with per-node
//!   cost hints. The agent picks branches by cost, doesn't need to
//!   materialise the whole scene.
//! - [`SceneIndex::generation`]: the current [`WorldGeneration`] so
//!   the agent can pair `scan()` with `watch(since)` cleanly.
//!
//! `OutlineNode::elided_children` is the most important field —
//! truncation is a signal, not silent loss. When the agent sees
//! `elided_children: 18`, it knows to call `walk()` or another `scan()`
//! with `under: this_ref` to drill in.

use std::collections::HashMap;

use bevy::prelude::*;
use kyoso_core::{NodeKind, SceneNode, SceneWorld};
use kyoso_graph::cost::Cost;
use serde::{Deserialize, Serialize};

use crate::handle::{node_ref_for, NodeRef, ScenePath, SessionId};
use crate::watch::WorldGeneration;

// =============================================================================
// Public types
// =============================================================================

/// Knobs for [`SceneAgent::scan`](crate::SceneAgent::scan). All optional;
/// sane defaults keep the result small enough for a first call from an
/// agent that knows nothing about the scene yet.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ScanOpts {
    /// Confine the outline to the subtree rooted at this node. `None` =
    /// scene roots.
    pub under: Option<NodeRef>,
    /// How deep to descend the outline. Default 2 — enough to see "what
    /// pages exist" and "what's at the top level of each page" without
    /// flooding the agent. `0` = catalog-only.
    pub depth: u32,
    /// Cap on total outline rows. When exceeded, deeper / later
    /// children are dropped and `OutlineNode::elided_children` is
    /// incremented on the parent.
    pub max_outline_rows: u32,
    /// Filter outline rows by kind. Empty = all kinds.
    pub kinds: Vec<NodeKind>,
}

impl Default for ScanOpts {
    fn default() -> Self {
        Self {
            under: None,
            depth: 2,
            max_outline_rows: 256,
            kinds: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SceneIndex {
    pub session: SessionId,
    pub generation: u64,
    pub catalog: Catalog,
    pub roots: Vec<OutlineNode>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Catalog {
    pub total_nodes: u64,
    pub kind_counts: HashMap<NodeKind, u64>,
    pub max_depth: u32,
}

/// One row in the outline forest. `children` is the recursive structure;
/// `elided_children` tells the agent how many descendants the outline
/// hid (either because of `depth` or `max_outline_rows`).
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutlineNode {
    pub node: NodeRef,
    pub kind: Option<NodeKind>,
    pub name: Option<String>,
    pub depth: u32,
    pub child_count: u32,
    pub subtree_cost: SubtreeCost,
    pub elided_children: u32,
    pub children: Vec<OutlineNode>,
}

/// Serializable mirror of [`kyoso_graph::cost::Cost`] so the SDK doesn't
/// require pulling the graph crate just to read an index. Same fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubtreeCost {
    pub estimated_items: u64,
    pub estimated_work: u64,
}

impl From<Cost> for SubtreeCost {
    fn from(c: Cost) -> Self {
        Self {
            estimated_items: c.estimated_items,
            estimated_work: c.estimated_work,
        }
    }
}

// =============================================================================
// Implementation
// =============================================================================

pub fn build_scene_index(
    sw: &mut SceneWorld,
    session: SessionId,
    opts: &ScanOpts,
) -> SceneIndex {
    let catalog = build_catalog(sw);
    let generation = sw
        .world()
        .get_resource::<WorldGeneration>()
        .copied()
        .unwrap_or_default()
        .0;

    let roots = build_outline(sw, session, opts);

    SceneIndex {
        session,
        generation,
        catalog,
        roots,
    }
}

fn build_catalog(sw: &mut SceneWorld) -> Catalog {
    let world = sw.world_mut();
    let mut q = world.query_filtered::<(Entity, &NodeKind), With<SceneNode>>();
    let mut total = 0u64;
    let mut by_kind: HashMap<NodeKind, u64> = HashMap::new();
    for (_, kind) in q.iter(world) {
        total += 1;
        *by_kind.entry(*kind).or_insert(0) += 1;
    }
    Catalog {
        total_nodes: total,
        kind_counts: by_kind,
        max_depth: compute_max_depth(sw),
    }
}

fn compute_max_depth(sw: &mut SceneWorld) -> u32 {
    let roots = scene_roots(sw);
    roots
        .into_iter()
        .map(|r| max_depth_under(sw, r, 0))
        .max()
        .unwrap_or(0)
}

fn max_depth_under(sw: &mut SceneWorld, entity: Entity, depth: u32) -> u32 {
    let children: Vec<Entity> = sw
        .world()
        .get::<Children>(entity)
        .map(|c| {
            c.iter()
                .filter(|e| sw.world().get::<SceneNode>(*e).is_some())
                .collect()
        })
        .unwrap_or_default();
    if children.is_empty() {
        return depth;
    }
    children
        .into_iter()
        .map(|c| max_depth_under(sw, c, depth + 1))
        .max()
        .unwrap_or(depth)
}

fn build_outline(sw: &mut SceneWorld, session: SessionId, opts: &ScanOpts) -> Vec<OutlineNode> {
    let mut emitted: u32 = 0;
    let max_rows = opts.max_outline_rows.max(1);

    let starts: Vec<Entity> = match opts.under.as_ref() {
        Some(r) => r
            .resolve(sw, session)
            .map(|e| vec![e])
            .unwrap_or_default(),
        None => scene_roots(sw),
    };

    starts
        .into_iter()
        .filter_map(|root| {
            if emitted >= max_rows {
                return None;
            }
            Some(build_outline_node(
                sw,
                session,
                root,
                0,
                opts,
                &mut emitted,
                max_rows,
            ))
        })
        .collect()
}

fn build_outline_node(
    sw: &mut SceneWorld,
    session: SessionId,
    entity: Entity,
    depth: u32,
    opts: &ScanOpts,
    emitted: &mut u32,
    max_rows: u32,
) -> OutlineNode {
    *emitted = emitted.saturating_add(1);

    let kind = sw.world().get::<NodeKind>(entity).copied();
    let name = display_name(sw, entity);
    let node = node_ref_for(sw, entity, session)
        .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));

    let children: Vec<Entity> = sw
        .world()
        .get::<Children>(entity)
        .map(|c| {
            c.iter()
                .filter(|e| sw.world().get::<SceneNode>(*e).is_some())
                .collect()
        })
        .unwrap_or_default();

    let child_count = children.len() as u32;
    let (subtree_items, subtree_work) = subtree_size(sw, entity);

    let mut included_children = Vec::new();
    let mut elided = 0u32;

    let kind_filter_active = !opts.kinds.is_empty();
    let kind_passes = |k: Option<NodeKind>| -> bool {
        if !kind_filter_active {
            true
        } else {
            k.map(|k| opts.kinds.contains(&k)).unwrap_or(false)
        }
    };

    if depth < opts.depth {
        for child in children {
            if *emitted >= max_rows {
                elided = elided.saturating_add(1);
                continue;
            }
            let child_kind = sw.world().get::<NodeKind>(child).copied();
            if !kind_passes(child_kind) {
                elided = elided.saturating_add(1);
                continue;
            }
            let sub = build_outline_node(sw, session, child, depth + 1, opts, emitted, max_rows);
            included_children.push(sub);
        }
    } else {
        elided = child_count;
    }

    OutlineNode {
        node,
        kind,
        name,
        depth,
        child_count,
        subtree_cost: SubtreeCost {
            estimated_items: subtree_items,
            estimated_work: subtree_work,
        },
        elided_children: elided,
        children: included_children,
    }
}

/// Returns `(item_count, work_count)` for the subtree rooted at
/// `entity`. Walks once via DFS; both numbers approximate the
/// [`Cost`] an iterator over this subtree would carry.
fn subtree_size(sw: &mut SceneWorld, entity: Entity) -> (u64, u64) {
    let mut stack = vec![entity];
    let mut items = 0u64;
    let mut work = 0u64;
    while let Some(e) = stack.pop() {
        if sw.world().get::<SceneNode>(e).is_some() {
            items += 1;
        }
        if let Some(children) = sw.world().get::<Children>(e) {
            work = work.saturating_add(children.len() as u64);
            for c in children.iter() {
                stack.push(c);
            }
        }
    }
    (items, work)
}

fn display_name(sw: &mut SceneWorld, entity: Entity) -> Option<String> {
    use kyoso_core::{Frame, Text};
    if let Some(data) = sw.read_as::<Frame>(entity) {
        if !data.frame.name.is_empty() {
            return Some(data.frame.name);
        }
    }
    if let Some(data) = sw.read_as::<Text>(entity) {
        if !data.text.content.is_empty() {
            return Some(data.text.content);
        }
    }
    None
}

fn scene_roots(sw: &mut SceneWorld) -> Vec<Entity> {
    let world = sw.world_mut();
    let mut q = world.query_filtered::<Entity, (With<SceneNode>, Without<ChildOf>)>();
    q.iter(world).collect()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_demo_scene;

    #[test]
    fn catalog_counts_match_demo_scene() {
        let mut sw = SceneWorld::new();
        let _ents = spawn_demo_scene(&mut sw);
        let catalog = build_catalog(&mut sw);
        assert_eq!(catalog.total_nodes, 5);
        assert_eq!(catalog.kind_counts.get(&NodeKind::Frame), Some(&2));
        assert_eq!(catalog.kind_counts.get(&NodeKind::Rectangle), Some(&1));
        assert_eq!(catalog.kind_counts.get(&NodeKind::Text), Some(&2));
        assert_eq!(catalog.max_depth, 2);
    }

    #[test]
    fn outline_respects_depth_cap() {
        let mut sw = SceneWorld::new();
        let _ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();
        let index = build_scene_index(
            &mut sw,
            session,
            &ScanOpts {
                depth: 1,
                ..Default::default()
            },
        );
        // Single root (Frame "Root"). Its children should not have grandchildren listed.
        assert_eq!(index.roots.len(), 1);
        for child in &index.roots[0].children {
            assert!(child.children.is_empty(), "depth=1 must not include grandchildren");
            // Children beyond the depth boundary should be flagged as elided
            // if they exist.
            if child.child_count > 0 {
                assert_eq!(child.elided_children, child.child_count);
            }
        }
    }

    #[test]
    fn outline_includes_cost_hints() {
        let mut sw = SceneWorld::new();
        let _ents = spawn_demo_scene(&mut sw);
        let index = build_scene_index(&mut sw, SessionId::new(), &ScanOpts::default());
        // Root subtree should contain 5 nodes (whole scene).
        assert_eq!(index.roots[0].subtree_cost.estimated_items, 5);
    }
}
