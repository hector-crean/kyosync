//! Tree primitive layered on top of the directed-graph core.
//!
//! A *tree* is a constrained subgraph of the main graph: every child has at
//! most one incoming edge marked with [`TreeEdge`], and the relation must
//! be acyclic. Children are ordered by [`OrderKey`] — a fractional index
//! string sorted lexicographically — so the parent's children form an
//! ordered list without anyone holding a `Vec<Entity>` that needs
//! rewriting on every insert.
//!
//! This shape is what Figma-style scene graphs and most workflow editors
//! want, and it maps cleanly onto tree CRDTs (Loro, Yjs, Kleppmann) when
//! a CRDT-backed [`crate::backend::GraphBackend`] is plugged in: a single
//! [`OrderKey`] per child is the only piece of mutable position state
//! that needs to merge.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{EdgeFrom, EdgeTo, IncomingEdges, OutgoingEdges};
use crate::{GraphCommand, GraphSystemSet};

/// Marker on an edge entity declaring it the parent→child link of a tree.
///
/// `EdgeFrom` carries the parent, `EdgeTo` the child. The same generic
/// graph traversal in [`crate::queries::GraphQuery`] applies; the marker
/// just lets tree-aware systems filter to tree edges.
#[derive(Component, Debug, Default, Clone, Copy, Reflect, Serialize, Deserialize)]
#[reflect(Component, Default)]
pub struct TreeEdge;

/// Per-child cache of the current tree parent. Set by
/// [`apply_tree_commands`] alongside the `TreeEdge` entity it spawns,
/// so consumers (and the sync layer) can answer "who is your parent?"
/// without walking incoming-edge queries each frame. `None` means the
/// node is currently a root.
#[derive(Component, Debug, Default, Clone, Copy, Reflect)]
#[reflect(Component, Default)]
pub struct TreeParent(pub Option<Entity>);

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
        // `r` is all 'a's (rare). Append 'a' to dive deeper; this is
        // strictly greater than `r`, which is wrong, so push a sentinel
        // mid char one level deeper. With `r = "aa..."`, returning
        // `"a..a a"` (l prefix + "a" + "a") wouldn't be less than r either.
        // First-cut fallback: panic loudly so the issue is visible.
        // Production code should pick a richer alphabet that includes
        // characters below 'a'.
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

/// Plugin that consumes tree-shaped [`GraphCommand`] variants
/// (`InsertChild`, `Reparent`, `MoveSibling`) and applies them as ECS
/// mutations.
///
/// Add alongside [`crate::GraphManagerPlugin`].
pub struct TreePlugin;

impl Plugin for TreePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<TreeEdge>()
            .register_type::<TreeParent>()
            .register_type::<OrderKey>()
            .add_systems(
                Update,
                apply_tree_commands.in_set(GraphSystemSet::CommandApplication),
            );
    }
}

fn apply_tree_commands(
    mut commands: Commands,
    mut reader: MessageReader<GraphCommand>,
    incoming: Query<&IncomingEdges>,
    tree_edges: Query<&TreeEdge>,
    edge_endpoints: Query<(&EdgeFrom, &EdgeTo)>,
) {
    for cmd in reader.read() {
        match cmd {
            GraphCommand::InsertChild {
                parent,
                child,
                position,
            } => {
                spawn_tree_edge(&mut commands, *parent, *child, position.clone());
            }
            GraphCommand::Reparent {
                child,
                new_parent,
                position,
            } => {
                if let Some(existing) =
                    find_parent_edge(*child, &incoming, &tree_edges, &edge_endpoints)
                {
                    commands.entity(existing).despawn();
                }
                spawn_tree_edge(&mut commands, *new_parent, *child, position.clone());
            }
            GraphCommand::MoveSibling { child, position } => {
                commands.entity(*child).insert(position.clone());
            }
            _ => {}
        }
    }
}

fn spawn_tree_edge(commands: &mut Commands, parent: Entity, child: Entity, position: OrderKey) {
    commands
        .entity(child)
        .insert((position, TreeParent(Some(parent))));
    commands
        .entity(parent)
        .with_related_entities::<EdgeFrom>(|rel| {
            rel.spawn((EdgeTo(child), TreeEdge));
        });
}

fn find_parent_edge(
    child: Entity,
    incoming: &Query<&IncomingEdges>,
    tree_edges: &Query<&TreeEdge>,
    edge_endpoints: &Query<(&EdgeFrom, &EdgeTo)>,
) -> Option<Entity> {
    let inc = incoming.get(child).ok()?;
    inc.iter().find(|&edge| {
        tree_edges.get(edge).is_ok() && edge_endpoints.get(edge).is_ok()
    })
}

/// Return `parent`'s direct children in ascending [`OrderKey`] order.
///
/// Filters the parent's outgoing edges to those marked [`TreeEdge`] and
/// reads each child's `OrderKey` to sort siblings.
pub fn ordered_children(
    parent: Entity,
    outgoing_index: &Query<&OutgoingEdges>,
    tree_edges: &Query<&TreeEdge>,
    edge_endpoints: &Query<(&EdgeFrom, &EdgeTo)>,
    order_keys: &Query<&OrderKey>,
) -> Vec<(Entity, OrderKey)> {
    let Ok(out) = outgoing_index.get(parent) else {
        return Vec::new();
    };
    let mut children: Vec<(Entity, OrderKey)> = out
        .iter()
        .filter_map(|edge| {
            tree_edges.get(edge).ok()?;
            let (_, edge_to) = edge_endpoints.get(edge).ok()?;
            let key = order_keys.get(edge_to.0).ok()?;
            Some((edge_to.0, key.clone()))
        })
        .collect();
    children.sort_by(|(_, a), (_, b)| a.cmp(b));
    children
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
