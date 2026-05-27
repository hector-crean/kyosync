//! `walk` — subtree traversal, depth/budget-bounded, returning
//! [`NodeRef`]s ready for the next call.
//!
//! Thin wrapper over [`SceneWorld::traverse`] / `traverse_typed`. The
//! agent-facing shape: pass a root + [`WalkOpts`], get back a `Walk`
//! with per-row depth + kind + the cost actually consumed.

use bevy::prelude::*;
use kyoso_core::{Node, NodeKind, SceneNode, SceneWorld};
use kyoso_graph::traversal::{Order, TraversalQuery};
use serde::{Deserialize, Serialize};

use crate::handle::{node_ref_for, NodeRef, ScenePath, SessionId};
use crate::scan::SubtreeCost;
use crate::NodeProjection;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub enum WalkStrategy {
    #[default]
    Dfs,
    Bfs,
}

impl WalkStrategy {
    fn to_order(self) -> Order {
        match self {
            WalkStrategy::Dfs => Order::Dfs,
            WalkStrategy::Bfs => Order::Bfs,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct WalkOpts {
    pub strategy: WalkStrategy,
    /// Cap depth (in tree edges from root). `None` = unbounded.
    pub depth_limit: Option<u32>,
    /// Filter result rows by kind. Empty = all kinds.
    /// (Filter is post-yield — see `TraversalQuery` semantics.)
    pub kinds: Vec<NodeKind>,
    /// Cap on row count. 0 = unlimited.
    pub max_items: u32,
    /// How much per-row detail to embed. Defaults to
    /// [`NodeProjection::Ref`] (today's shape — `NodeRef` + `kind`).
    /// Bump to [`NodeProjection::Variant`] / [`NodeProjection::Full`]
    /// to save an `inspect` round-trip per row when the agent already
    /// knows it wants the data.
    pub include: NodeProjection,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Walk {
    pub root: NodeRef,
    pub rows: Vec<WalkRow>,
    /// True when `max_items` truncated the result before all matching
    /// rows were collected. The agent can issue another `walk` rooted
    /// at the last yielded row to continue.
    pub truncated: bool,
    pub cost: SubtreeCost,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WalkRow {
    pub node: NodeRef,
    pub kind: Option<NodeKind>,
    pub depth: u32,
    /// Typed `Node` sum-type materialisation. Populated when
    /// [`WalkOpts::include`] is [`NodeProjection::Variant`] or
    /// [`NodeProjection::Full`]; `None` for [`NodeProjection::Ref`] or
    /// for entities that match no known variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<Node>,
    /// Schemaless component-name dump for the row. Populated only
    /// when [`WalkOpts::include`] is [`NodeProjection::Full`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_names: Option<Vec<String>>,
}

pub fn run_walk(
    sw: &mut SceneWorld,
    session: SessionId,
    root: Entity,
    opts: &WalkOpts,
) -> Walk {
    let mut q = TraversalQuery::new()
        .start_at(root)
        .order(opts.strategy.to_order());
    if let Some(d) = opts.depth_limit {
        q = q.max_depth(d as usize);
    }

    let walked = sw.traverse(&q);

    let max = if opts.max_items == 0 {
        usize::MAX
    } else {
        opts.max_items as usize
    };

    let kind_filter = &opts.kinds;

    // Pass 1: structural filter (kind + budget). We collect kept
    // entities first so the variant/component projection passes below
    // can batch their state caches over exactly the surviving set.
    let mut kept: Vec<(Entity, u32, Option<NodeKind>)> = Vec::with_capacity(walked.len().min(max));
    let mut work = 0u64;
    let mut truncated = false;

    for w in walked.into_iter() {
        work = work.saturating_add(1);
        let kind = sw.world().get::<NodeKind>(w.entity).copied();
        if !kind_filter.is_empty() {
            match kind {
                Some(k) if kind_filter.contains(&k) => {}
                _ => continue,
            }
        }
        if kept.len() >= max {
            truncated = true;
            break;
        }
        kept.push((w.entity, w.depth as u32, kind));
    }

    // Pass 2: optional batched variant projection — one QueryState
    // tuple build amortised across all kept rows.
    let variants: Vec<Option<Node>> = if opts.include.wants_variant() {
        sw.materialize_many::<SceneNode>(kept.iter().map(|(e, _, _)| *e))
    } else {
        Vec::new()
    };

    // Pass 3: per-row finalisation — NodeRef + optional component-name
    // dump. Component names is per-entity (no batch API today); keeping
    // it inline here means we only pay for rows we're actually emitting.
    let want_components = opts.include.wants_component_names();
    let mut rows = Vec::with_capacity(kept.len());
    let mut variant_iter = variants.into_iter();
    for (entity, depth, kind) in kept {
        let variant = variant_iter.next().unwrap_or(None);
        let component_names = if want_components {
            Some(sw.component_names(entity))
        } else {
            None
        };
        let node = node_ref_for(sw, entity, session)
            .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
        rows.push(WalkRow {
            node,
            kind,
            depth,
            variant,
            component_names,
        });
    }

    let root_ref = node_ref_for(sw, root, session)
        .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));

    let items = rows.len() as u64;
    Walk {
        root: root_ref,
        rows,
        truncated,
        cost: SubtreeCost {
            estimated_items: items,
            estimated_work: work,
        },
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_demo_scene;

    #[test]
    fn unbounded_walk_covers_demo_scene() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(&mut sw, SessionId::new(), ents.root, &WalkOpts::default());
        assert_eq!(walk.rows.len(), 5);
    }

    #[test]
    fn depth_limit_caps_descent() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(
            &mut sw,
            SessionId::new(),
            ents.root,
            &WalkOpts {
                depth_limit: Some(1),
                ..Default::default()
            },
        );
        // Root + 2 children at depth 1, nothing deeper.
        let depths: Vec<u32> = walk.rows.iter().map(|r| r.depth).collect();
        assert!(depths.iter().all(|d| *d <= 1));
    }

    #[test]
    fn kind_filter_restricts_rows() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(
            &mut sw,
            SessionId::new(),
            ents.root,
            &WalkOpts {
                kinds: vec![NodeKind::Text],
                ..Default::default()
            },
        );
        for row in &walk.rows {
            assert_eq!(row.kind, Some(NodeKind::Text));
        }
        assert_eq!(walk.rows.len(), 2);
    }

    #[test]
    fn max_items_truncates_and_flags() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(
            &mut sw,
            SessionId::new(),
            ents.root,
            &WalkOpts {
                max_items: 2,
                ..Default::default()
            },
        );
        assert_eq!(walk.rows.len(), 2);
        assert!(walk.truncated);
    }

    #[test]
    fn projection_ref_omits_variant_and_components() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(&mut sw, SessionId::new(), ents.root, &WalkOpts::default());
        assert!(!walk.rows.is_empty());
        for row in &walk.rows {
            assert!(
                row.variant.is_none(),
                "default projection (Ref) must not embed Node variants"
            );
            assert!(
                row.component_names.is_none(),
                "default projection (Ref) must not embed component names"
            );
        }
    }

    #[test]
    fn projection_variant_embeds_typed_node() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(
            &mut sw,
            SessionId::new(),
            ents.root,
            &WalkOpts {
                include: NodeProjection::Variant,
                ..Default::default()
            },
        );
        for row in &walk.rows {
            assert!(
                row.variant.is_some(),
                "Variant projection should materialise every demo-scene row"
            );
            assert!(
                row.component_names.is_none(),
                "Variant projection should not include component names"
            );
        }
        // Spot-check: root is the Frame "Root".
        let root_row = walk
            .rows
            .iter()
            .find(|r| r.depth == 0)
            .expect("walk must yield the root row");
        match root_row.variant.as_ref().unwrap() {
            Node::Frame(f) => assert_eq!(f.frame.name, "Root"),
            other => panic!("expected Frame variant at root, got {:?}", other),
        }
    }

    #[test]
    fn projection_full_includes_component_names() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let walk = run_walk(
            &mut sw,
            SessionId::new(),
            ents.root,
            &WalkOpts {
                include: NodeProjection::Full,
                ..Default::default()
            },
        );
        for row in &walk.rows {
            assert!(row.variant.is_some());
            let names = row
                .component_names
                .as_ref()
                .expect("Full projection should populate component_names");
            assert!(!names.is_empty(), "every demo-scene entity carries components");
        }
    }
}
