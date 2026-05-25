//! `Scene` — the combined SystemParam: hierarchy + edge-graph.
//!
//! Bundles a [`TreeQuery`] (Bevy native `Children` / `ChildOf` +
//! [`crate::tree::OrderKey`] for deterministic sibling ordering) with
//! a [`GraphQuery<N, E>`] (entity-edges with marker filters, typed
//! edge variants). Use this when a system genuinely needs both — for
//! example, "walk down the scene tree and report which descendants
//! have outgoing typed edges".
//!
//! For one-sided work, reach for the inner SystemParams directly:
//! [`TreeQuery`] for pure hierarchy, [`GraphQuery<N, E>`] for entity-
//! edge ops.
//!
//! ## No `GraphTraverse` impl on `Scene`
//!
//! Both inner queries impl `GraphTraverse` with different semantics
//! (successors = ordered children, successors = `OutgoingEdges`
//! neighbours). Picking one as the default for `Scene` would surprise
//! the other half of callers, so we don't — go through `scene.tree`
//! or `scene.graph` explicitly.

use bevy::ecs::query::{QueryData, QueryFilter};
use bevy::ecs::system::SystemParam;

use crate::queries::GraphQuery;
use crate::tree::TreeQuery;

/// Combined read view: hierarchy (`tree`) + entity-edge graph (`graph`).
///
/// Generic parameters mirror [`GraphQuery`]:
/// - `N`: node query projection (e.g. `&MyNode`, `(&Transform, &Size)`)
/// - `E`: edge query projection (e.g. `&MyEdge`)
/// - `NF`: node filter (defaults to `()`)
/// - `EF`: edge filter (defaults to `()`)
///
/// `NF` is **shared with the inner [`TreeQuery`]** so both layers
/// observe the same entity set. For example, in
/// `Scene<&SceneNode, &SceneEdge, With<SceneNode>>`, the tree walks
/// only entities carrying `SceneNode` — any bare overlay child of a
/// scene node is invisible to `scene.tree` (and to `scene.graph`).
/// Default `NF = ()` keeps the tree unfiltered.
///
/// ```ignore
/// fn cross_frame_refs(scene: Scene<&SceneNode, &SceneEdge, With<SceneNode>>) {
///     for root in scene.tree.roots() {
///         for (child, _depth) in scene.tree.walk_dfs_with_depth(root) {
///             for edge in scene.graph.outgoing_edges(child) {
///                 // …
///             }
///         }
///     }
/// }
/// ```
#[derive(SystemParam)]
pub struct Scene<'w, 's, N, E, NF = (), EF = ()>
where
    N: QueryData + 'static,
    E: QueryData + 'static,
    NF: QueryFilter + 'static,
    EF: QueryFilter + 'static,
{
    /// Hierarchy view — `Children` / `ChildOf` / `OrderKey`, filtered
    /// by the same `NF` used by [`GraphQuery`] for nodes.
    pub tree: TreeQuery<'w, 's, NF>,
    /// Entity-edge graph view — `EdgeFrom` / `EdgeTo` /
    /// `OutgoingEdges` / `IncomingEdges` with marker filters.
    pub graph: GraphQuery<'w, 's, N, E, NF, EF>,
}
