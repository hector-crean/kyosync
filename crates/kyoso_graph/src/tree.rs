//! Tree primitive layered on top of Bevy's native `Children` / `ChildOf`
//! hierarchy.
//!
//! ## Why Bevy's hierarchy, not entity-edges
//!
//! A *tree* is one parent per child plus an ordered sibling list. Bevy's
//! `#[relationship]`-derived [`Children`] / [`ChildOf`] pair gives us
//! that natively: `ChildOf` is one component on the child pointing to
//! the parent; `Children` is an auto-maintained `SmallVec<Entity>` on
//! the parent. Re-parenting is a single [`EntityCommands::add_child`]
//! call which removes the old `ChildOf` and inserts the new one. No
//! edge entities, no parallel parent caches.
//!
//! The sibling order Bevy maintains by insertion isn't what we want
//! though — we need *deterministic* ordering across peers that have
//! seen different concurrent inserts. That's what [`OrderKey`] is for:
//! a fractional-index string stored per child, sorted lexicographically
//! when reading children. Strings let you insert between any two
//! existing siblings without renumbering anyone, which maps cleanly
//! onto tree CRDTs (Loro, Yjs, Kleppmann) — a single `OrderKey` per
//! child is the only piece of mutable position state that needs to
//! merge.
//!
//! ## The pair: `ChildOf` + `OrderKey`
//!
//! - `ChildOf(parent)` — structural; absent on root nodes.
//! - `OrderKey("...")` — sibling ordering; consulted by [`TreeQuery`]
//!   when enumerating children. Roots can carry one too if they need
//!   forest ordering, otherwise it's ignored.
//!
//! ## Reading the tree
//!
//! Use [`TreeQuery`] inside a Bevy system. It's the canonical agent-
//! facing one-hop view: `parent`, `children` (OrderKey-sorted), `roots`,
//! `depth`, `walk_dfs_with_depth`, and `GraphTraverse` for composing
//! with the closure-driven walks in [`crate::traverse`].

use bevy::ecs::query::QueryFilter;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::traverse::GraphTraverse;
use crate::{GraphCommand, GraphSystemSet};

// ============================================================================
// OrderKey — fractional-index sibling ordering
// ============================================================================

/// Per-child fractional ordering key.
///
/// Strings sort lexicographically over a printable-ASCII alphabet, so
/// inserting between two existing siblings is always possible without
/// renumbering anyone else. See [`OrderKey::between`] for the midpoint
/// generator.
#[derive(
    Component,
    Clone,
    Debug,
    Reflect,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[reflect(Component, Serialize, Deserialize)]
pub struct OrderKey(pub String);

impl OrderKey {
    /// Default key for the very first child placed under a parent.
    /// Sits in the middle of the alphabet so subsequent inserts have
    /// equal room on either side.
    pub fn first() -> Self {
        Self("n".to_string())
    }

    /// Generate an ordering key strictly between `left` and `right`.
    ///
    /// `None` on either side means "no neighbour on that side" (i.e.
    /// inserting before the first / after the last sibling). The result
    /// uses lowercase ASCII letters 'a'..='z' and is at most one byte
    /// longer than the longer of the inputs.
    pub fn between(left: Option<&Self>, right: Option<&Self>) -> Self {
        match (left, right) {
            (None, None) => Self::first(),
            (Some(l), None) => Self(format!("{}n", l.0)),
            (None, Some(r)) => Self::before(r),
            (Some(l), Some(r)) => Self::midpoint(l, r),
        }
    }

    fn before(r: &Self) -> Self {
        let bytes = r.0.as_bytes();
        let mut prefix = Vec::with_capacity(bytes.len() + 1);
        for &b in bytes {
            if b > b'a' {
                prefix.push(u8::midpoint(b'a', b));
                return Self(String::from_utf8(prefix).expect("ASCII"));
            }
            prefix.push(b);
        }
        // `r` is all 'a's (rare); the alphabet is exhausted. Production
        // code should pick a richer alphabet that includes characters
        // below 'a'.
        panic!("OrderKey::before: cannot produce a key smaller than {r:?}; alphabet is exhausted");
    }

    fn midpoint(l: &Self, r: &Self) -> Self {
        debug_assert!(l < r, "midpoint requires l < r, got {l:?} >= {r:?}");
        let lb = l.0.as_bytes();
        let rb = r.0.as_bytes();
        let mut prefix = Vec::with_capacity(lb.len().max(rb.len()) + 1);
        let max = lb.len().max(rb.len());
        for i in 0..max {
            let lc = lb.get(i).copied().unwrap_or(b'a' - 1);
            let rc = rb.get(i).copied().unwrap_or(b'z' + 1);
            if rc > lc + 1 {
                prefix.push(u8::midpoint(lc, rc));
                return Self(String::from_utf8(prefix).expect("ASCII"));
            }
            prefix.push(lc);
        }
        // l and r are equal as compared so far — extend l with mid char.
        prefix.push(b'n');
        Self(String::from_utf8(prefix).expect("ASCII"))
    }
}

// ============================================================================
// TreeQuery — agent-facing one-hop view over Bevy's hierarchy + OrderKey
// ============================================================================

/// Read-only `SystemParam` over Bevy's native [`Children`] / [`ChildOf`]
/// pair plus our [`OrderKey`] for deterministic sibling ordering.
///
/// Parallel-safe in-system traversal: declares its component access so
/// Bevy can schedule alongside other systems. Use this for any
/// hierarchical read (parent lookup, ordered child enumeration, depth
/// computation, walks).
///
/// ## Optional `F` filter
///
/// The `F: QueryFilter` parameter narrows every internal query so the
/// tree view only sees entities that satisfy `F`. Default `F = ()`
/// (no filter) walks every tree-participating entity in the world —
/// the right shape for agent traversal that should surface bare
/// overlays too.
///
/// Couple this with the node-side filter on
/// [`crate::queries::GraphQuery`] via the [`crate::scene::Scene`]
/// combined SystemParam, e.g.
/// `Scene<&SceneNode, &SceneEdge, With<SceneNode>>` — both layers
/// then operate on the same SceneNode-marked entity set.
///
/// For *graph* reads (entity-edges with marker filters, typed
/// edge variants) use [`crate::queries::GraphQuery`] — that's a
/// different access pattern, not a wrapping of this one.
#[derive(SystemParam)]
pub struct TreeQuery<'w, 's, F = ()>
where
    F: QueryFilter + 'static,
{
    children_q: Query<'w, 's, &'static Children, F>,
    child_of_q: Query<'w, 's, &'static ChildOf, F>,
    order_key_q: Query<'w, 's, &'static OrderKey, F>,
    /// Read every entity that carries `ChildOf` *and satisfies `F`*.
    /// Used by enumeration helpers (`node_count`, `max_depth`).
    parents_q: Query<'w, 's, Entity, (With<ChildOf>, F)>,
    /// Read every entity that has `Children` *and satisfies `F`*.
    /// Used by [`roots`](Self::roots) and enumeration.
    has_children_q: Query<'w, 's, Entity, (With<Children>, F)>,
}

impl<F> TreeQuery<'_, '_, F>
where
    F: QueryFilter + 'static,
{
    /// Get the parent of `node`, or `None` if it's a root (has no
    /// [`ChildOf`] component).
    pub fn parent(&self, node: Entity) -> Option<Entity> {
        self.child_of_q.get(node).ok().map(|c| c.parent())
    }

    /// Get all children of `parent`, sorted by [`OrderKey`].
    /// Children without an `OrderKey` are filtered out — every tree
    /// node should carry one. Returns an empty vector if `parent`
    /// has no children.
    pub fn children(&self, parent: Entity) -> Vec<Entity> {
        self.children_with_keys(parent)
            .into_iter()
            .map(|(e, _)| e)
            .collect()
    }

    /// Get all children with their order keys, sorted ascending.
    pub fn children_with_keys(&self, parent: Entity) -> Vec<(Entity, OrderKey)> {
        let Ok(children) = self.children_q.get(parent) else {
            return Vec::new();
        };
        let mut out: Vec<(Entity, OrderKey)> = children
            .iter()
            .filter_map(|child| {
                self.order_key_q
                    .get(child)
                    .ok()
                    .map(|k| (child, k.clone()))
            })
            .collect();
        out.sort_by(|(_, a), (_, b)| a.cmp(b));
        out
    }

    /// Find all roots (tree-participating entities with no parent).
    ///
    /// A *tree-participating entity* is one that either has children
    /// or has a parent. Pure standalone entities (no `Children`, no
    /// `ChildOf`) aren't considered part of any tree and aren't roots.
    pub fn roots(&self) -> Vec<Entity> {
        // Anything with `Children` but not `ChildOf` is a root.
        self.has_children_q
            .iter()
            .filter(|e| self.child_of_q.get(*e).is_err())
            .collect()
    }

    /// Depth of `node` (distance from nearest root). 0 for roots.
    pub fn depth(&self, mut node: Entity) -> usize {
        let mut depth = 0;
        while let Some(parent) = self.parent(node) {
            depth += 1;
            node = parent;
        }
        depth
    }

    /// Path from `node` to its root: `[node, parent, grandparent, ..., root]`.
    pub fn path_to_root(&self, mut node: Entity) -> Vec<Entity> {
        let mut path = vec![node];
        while let Some(parent) = self.parent(node) {
            path.push(parent);
            node = parent;
        }
        path
    }

    pub fn is_root(&self, node: Entity) -> bool {
        self.parent(node).is_none()
    }

    pub fn is_leaf(&self, node: Entity) -> bool {
        self.children_q
            .get(node)
            .map(|c| c.is_empty())
            .unwrap_or(true)
    }

    /// Total count of entities participating in any tree.
    pub fn node_count(&self) -> usize {
        // Parents ∪ children. Parents = has_children; children = has ChildOf.
        // Root-only counts via has_children (roots have children to be
        // tree-participating). Leaf counts via parents_q.
        let mut seen = std::collections::HashSet::new();
        for e in self.parents_q.iter() {
            seen.insert(e);
        }
        for e in self.has_children_q.iter() {
            seen.insert(e);
        }
        seen.len()
    }

    /// Maximum depth across all tree-participating entities.
    pub fn max_depth(&self) -> usize {
        self.parents_q
            .iter()
            .chain(self.has_children_q.iter())
            .map(|e| self.depth(e))
            .max()
            .unwrap_or(0)
    }

    /// Pre-order DFS over the subtree rooted at `root`, yielding
    /// `(entity, depth)` for each visited entity. `depth` is relative
    /// to `root` (i.e. `root` itself yields depth `0`). Children are
    /// visited in `OrderKey` order, so the traversal is deterministic
    /// across runs given the same tree.
    pub fn walk_dfs_with_depth(&self, root: Entity) -> impl Iterator<Item = (Entity, usize)> {
        let mut out: Vec<(Entity, usize)> = Vec::new();
        let mut stack: Vec<(Entity, usize)> = vec![(root, 0)];
        while let Some((entity, depth)) = stack.pop() {
            out.push((entity, depth));
            // Push children in reverse so the next pop yields the first child.
            for child in self.children(entity).into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        out.into_iter()
    }
}

// ============================================================================
// GraphTraverse impl — successors = ordered children, predecessors = parent
// ============================================================================
//
// Lets `TreeQuery` act as the substrate for `crate::traverse` closure-
// driven walks (`bfs_walk` / `dfs_walk` with `Step`) — what the agent-
// facing `TraversalQuery` runner needs.

impl<F> GraphTraverse for TreeQuery<'_, '_, F>
where
    F: QueryFilter + 'static,
{
    fn successors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.children(node).into_iter()
    }

    fn predecessors(&self, node: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.parent(node).into_iter()
    }
}

// ============================================================================
// TreePlugin — consumes tree-shaped GraphCommand variants
// ============================================================================

/// Plugin that consumes tree-shaped [`GraphCommand`] variants
/// (`InsertChild`, `Reparent`, `MoveSibling`) and applies them as ECS
/// mutations.
///
/// Add alongside [`crate::GraphManagerPlugin`].
pub struct TreePlugin;

impl Plugin for TreePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<OrderKey>().add_systems(
            Update,
            apply_tree_commands.in_set(GraphSystemSet::CommandApplication),
        );
    }
}

fn apply_tree_commands(mut commands: Commands, mut reader: MessageReader<GraphCommand>) {
    for cmd in reader.read() {
        match cmd {
            GraphCommand::InsertChild {
                parent,
                child,
                position,
            } => {
                commands.entity(*child).insert(position.clone());
                commands.entity(*parent).add_child(*child);
            }
            GraphCommand::Reparent {
                child,
                new_parent,
                position,
            } => {
                // `add_child` re-parents: removes the old `ChildOf`
                // from the child (and the entry in the old parent's
                // `Children`) and inserts the new one.
                commands.entity(*child).insert(position.clone());
                commands.entity(*new_parent).add_child(*child);
            }
            GraphCommand::MoveSibling { child, position } => {
                commands.entity(*child).insert(position.clone());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn between_none_none_is_first() {
        assert_eq!(OrderKey::between(None, None), OrderKey::first());
    }

    #[test]
    fn between_some_some_lies_in_range() {
        let l = OrderKey("a".into());
        let r = OrderKey("c".into());
        let mid = OrderKey::between(Some(&l), Some(&r));
        assert!(l < mid && mid < r, "got {mid:?} not between {l:?} and {r:?}");
    }

    #[test]
    fn between_adjacent_extends_depth() {
        let l = OrderKey("a".into());
        let r = OrderKey("b".into());
        let mid = OrderKey::between(Some(&l), Some(&r));
        assert!(l < mid && mid < r, "got {mid:?}");
        assert!(mid.0.starts_with('a'));
    }

    #[test]
    fn between_after_end_grows() {
        let l = OrderKey("z".into());
        let mid = OrderKey::between(Some(&l), None);
        assert!(l < mid);
    }
}
