//! Reference-edge categories.
//!
//! kyoso documents have two structurally distinct kinds of edges:
//!
//! - **Tree edges** — the parent-child scaffold. Identified by a node's
//!   `tree_parent` + `OrderKey`, not by a separate edge entity, and
//!   replicated through the [`Move`](crate::op::OpKind::Move) op.
//! - **Reference edges** — many-to-many cross-references between nodes
//!   (component instance → main, prototype links, comment anchors, etc.).
//!   Replicated through [`AddRefEdge`](crate::op::OpKind::AddRefEdge) /
//!   [`RemoveRefEdge`](crate::op::OpKind::RemoveRefEdge), each carrying
//!   one of these categories.
//!
//! The category is fixed at edge creation. Different categories may
//! eventually pick different conflict-resolution policies (Phase F) and
//! different [`DanglePolicy`] when an endpoint is tombstoned. For Phase E
//! the category is just metadata; backend behavior is uniform.

use serde::{Deserialize, Serialize};

/// Hardcoded reference-edge category, with an open `Custom` escape hatch.
///
/// New first-class categories should be added as variants here rather
/// than via `Custom`; the trade-off is intentional — first-class names
/// get compile-time exhaustiveness checks across the codebase, while
/// `Custom` is for app-level extensions that aren't worth a kernel
/// change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeCategory {
    /// Untyped reference. Default for callers that don't pick a more
    /// specific category (notably the [`GraphBackend::add_edge`]
    /// fallback path).
    ///
    /// [`GraphBackend::add_edge`]: kyoso_graph::backend::GraphBackend::add_edge
    #[default]
    Reference,
    /// Component instance → main component (Figma-shaped).
    InstanceOf,
    /// Prototype interaction transition.
    PrototypeLink,
    /// Constraint anchor relating two nodes.
    ConstraintPin,
    /// Reference to a shared style or variable definition.
    StyleRef,
    /// Comment thread anchored to this node.
    CommentAnchor,
    /// User mention.
    Mention,
    /// Mask layer relationship (this node masks another).
    MaskOf,
    /// Application-defined category.
    Custom(String),
}

/// Policy for what to do with a reference edge when one of its endpoints
/// is tombstoned. Phase E keeps a single document-wide default
/// ([`DanglePolicy::Cascade`] for compatibility with kyoso's existing
/// `RemoveNode` cascade); each [`RefEdgeCrdt`] impl can override.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DanglePolicy {
    /// Tombstone the edge when an endpoint is tombstoned. Current kyoso
    /// behavior; matches filesystem semantics (deleting a file
    /// invalidates symlinks).
    Cascade,
    /// Keep the edge; runtime treats it as broken. Matches Figma's
    /// behavior with deleted main components.
    Tolerate,
    /// Keep the edge; if the endpoint is restored (undo), re-bind.
    /// Requires tombstones to live until compaction.
    ReanchorOnUndo,
}

/// Conflict-resolution policy for the *existence* of a reference edge
/// under concurrent add/remove of the same `(from, to)` pair.
///
/// Each [`EdgeCategory`] can pick a different policy via its
/// [`RefEdgeCrdt`] impl — `instance_of` may want `OrSet` (re-add after
/// remove), while a permanent `mask_of` link may want `TwoPSet` (once
/// removed, never re-added without explicit re-creation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefEdgePolicy {
    /// Add-wins observed-remove set. Default; matches modern collaborative
    /// editor expectations.
    OrSet,
    /// Once removed, never re-added. Used when deletion implies
    /// permanence.
    TwoPSet,
    /// Concurrent remove wins over concurrent add.
    RemoveWins,
    /// Last-writer-wins keyed by `(from, to)` pair. Useful when you
    /// genuinely have at most one edge per endpoint pair.
    LwwByEndpoints,
}

/// Per-category CRDT semantics for reference edges.
///
/// One unit struct per [`EdgeCategory`] variant implements this trait,
/// declaring the existence policy ([`RefEdgePolicy`]), the dangling-
/// target policy ([`DanglePolicy`]), and the per-edge property schema
/// ([`Properties`](Self::Properties)).
///
/// ## Example
///
/// ```ignore
/// use kyoso_crdt::types::LwwRegister;
/// use kyoso_crdt::Crdt;
/// use kyoso_crdt_derive::Crdt as DeriveCrdt;
/// use kyoso_graph_crdt::{RefEdgeCrdt, RefEdgePolicy, DanglePolicy};
///
/// #[derive(DeriveCrdt)]
/// pub struct PrototypeTransition {
///     pub kind: LwwRegister<String>,
///     pub easing: LwwRegister<String>,
/// }
///
/// pub struct PrototypeLinkEdge;
/// impl RefEdgeCrdt for PrototypeLinkEdge {
///     const POLICY: RefEdgePolicy = RefEdgePolicy::OrSet;
///     const DANGLE: DanglePolicy = DanglePolicy::Tolerate;
///     type Properties = PrototypeTransition;
/// }
/// ```
pub trait RefEdgeCrdt: Default + Send + Sync + 'static {
    /// Conflict resolution under concurrent add/remove.
    const POLICY: RefEdgePolicy;
    /// What to do when an endpoint is tombstoned.
    const DANGLE: DanglePolicy;
    /// Per-edge property schema. Use `()` for edges without properties.
    type Properties;
}
