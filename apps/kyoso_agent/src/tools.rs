//! The two traits that pin the canonical agent-facing API surface.
//!
//! [`SceneRead`] = six read verbs, no side effects. [`SceneMutate`] =
//! four write verbs (create / update / delete / move). They take
//! **concrete** types — no `impl Into<NodeTarget>` — so the traits are
//! object-safe (`Box<dyn SceneRead>` etc.) and walkable by the wire
//! codegen layer that emits MCP tool schemas, Python stubs, TS
//! bindings.
//!
//! # Why two traits instead of one
//!
//! - **Capability separation**: an agent identity can declare what it
//!   can do statically. Read-only agents get `impl SceneRead`; mutating
//!   agents get `impl SceneRead + SceneMutate`. The compile-time check
//!   beats a runtime "is this tool allowed?" guard.
//! - **Versioning**: the read surface stabilises faster than the write
//!   surface (mutate verbs gain options, patch shapes, transaction
//!   recording, …). Separating them means readers don't churn every
//!   time we extend writes.
//!
//! # Relationship to the inherent methods
//!
//! The inherent `impl SceneAgent` block is the **source of truth** for
//! verb behaviour. Inherent methods take `impl Into<NodeTarget>` for
//! Rust-side ergonomics; the trait methods take plain `NodeTarget`
//! and exist *only* to expose the same surface to the wire codegen.
//! Each trait method's body is a one-line forwarder to the inherent
//! method.
//!
//! This means: adding a field to an opts struct updates one place
//! (the helper or inherent method). Adding a *new verb* edits the
//! trait first, then the inherent forwarder. There's no place where
//! trait and inherent can disagree on behaviour.
//!
//! # Raw-identifier verb names
//!
//! `r#match` and `r#move` use raw-identifier syntax because both
//! collide with Rust keywords. Callers can rename via let-binding
//! when that's annoying, but the trait method names stay symmetric
//! with the inherent methods.

use crate::handle::{Cursor, NodeTarget};
use crate::mutate::{CreateSpec, MoveSpec, MutateError, MutateResult, UpdatePatch};
use crate::navigate::NavOpts;
use crate::query::{QueryResult, QuerySpec};
use crate::scan::{SceneIndex, ScanOpts};
use crate::walk::{Walk, WalkOpts};
use crate::watch::{WatchOpts, WatchPage};
use crate::{EntityReport, MatchRefs, NodeRef, PatternSpec};

/// Side-effect-free agent operations. Implementors return owned data;
/// no references escape.
pub trait SceneRead {
    /// Catalog + depth-bounded outline for plan-then-drill workflows.
    fn scan(&mut self, opts: ScanOpts) -> SceneIndex;

    /// Schemaless component dump + typed variant materialisation for
    /// one target.
    fn inspect(&mut self, target: NodeTarget) -> EntityReport;

    /// Subtree walk under `root` with depth / kind / budget caps.
    fn walk(&mut self, root: NodeTarget, opts: WalkOpts) -> Walk;

    /// One-hop (or transitive) neighbourhood query. Returns
    /// `NodeRef`s the agent can feed back into the next call.
    fn navigate(&mut self, from: NodeTarget, opts: NavOpts) -> Vec<NodeRef>;

    /// Pattern (subgraph-isomorphism) matching. The pattern is a
    /// [`PatternSpec`] — owned, serde-able, lifetime-free — so MCP /
    /// FFI callers can describe a shape from outside the Rust process.
    /// Results are in `NodeRef`/`EdgeRef` space; no raw `Entity` exposed.
    fn r#match(&mut self, spec: &PatternSpec) -> Vec<MatchRefs>;

    /// Coalesced change page since `since`. Returns
    /// `buffer_overflow: true` when the agent's incremental view is
    /// gone and must re-`scan`.
    fn watch(&mut self, since: Option<Cursor>, opts: WatchOpts) -> WatchPage;

    /// Generic ECS-component filter — the escape hatch for cases the
    /// semantic verbs above don't cover. Component types must be
    /// registered in the [`AppTypeRegistry`]; kyoso's [`WatchPlugin`]
    /// registers the scene variants automatically.
    ///
    /// Returns `NodeRef`s only — for component-data projection, pair
    /// with [`Self::inspect`] (or reach for the
    /// [Bevy Remote Protocol](https://docs.rs/bevy/latest/bevy/remote/index.html)
    /// when the data isn't a kyoso scene variant).
    ///
    /// [`AppTypeRegistry`]: bevy::reflect::AppTypeRegistry
    /// [`WatchPlugin`]: crate::WatchPlugin
    fn query(&mut self, spec: QuerySpec) -> QueryResult;
}

/// Mutation verbs. All four return a [`MutateResult`] with the
/// post-mutation [`crate::handle::NodeRef`] and the new
/// [`crate::watch::WorldGeneration`] cursor — the agent can stitch
/// mutations and observations together without losing position.
pub trait SceneMutate {
    /// Spawn a new node under `spec.parent` (or as a scene root if
    /// `None`). Returns the new [`NodeRef`].
    fn create(&mut self, spec: CreateSpec) -> Result<MutateResult, MutateError>;

    /// Apply a partial update to `target`. Patch fields that don't
    /// match the variant are silently ignored today; see
    /// [`MutateError::VariantMismatch`].
    fn update(
        &mut self,
        target: NodeTarget,
        patch: UpdatePatch,
    ) -> Result<MutateResult, MutateError>;

    /// Despawn `target` and any tree descendants (Bevy's `ChildOf`
    /// relationship cascades).
    fn delete(&mut self, target: NodeTarget) -> Result<MutateResult, MutateError>;

    /// Reparent / reorder. `new_parent: None` promotes to scene root.
    fn r#move(&mut self, spec: MoveSpec) -> Result<MutateResult, MutateError>;
}
