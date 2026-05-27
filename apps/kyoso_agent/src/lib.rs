//! Agent-facing read + write surface for kyoso scenes.
//!
//! # What this crate is for
//!
//! `kyoso_agent` is the **semantic SDK surface** that lets an AI agent
//! (or MCP server, JS-FFI handler, Python notebook, …) reason about
//! and edit a running kyoso scene. It's designed for the loop a real
//! coding/design assistant runs:
//!
//! 1. Get a cheap **catalog** of the scene to plan a search.
//! 2. **Drill down** into one branch at a time, paying token cost
//!    proportional to the answer.
//! 3. **Navigate** between related entities (cross-frame edges,
//!    parents, downstream effects) one step at a time.
//! 4. **Edit** with explicit intent (create / update / delete / move).
//! 5. **Watch** the change stream — own edits, human edits, sync from
//!    other peers — all in one coherent cursor.
//!
//! This is deliberately a small, opinionated verb set. The contract is
//! that an agent never needs to know what a Bevy `Entity` is, doesn't
//! reach into ECS internals, and can carry state across process
//! boundaries (it gets durable [`NodeRef`]s back from every call).
//!
//! ## Compared to BRP (Bevy Remote Protocol)
//!
//! Bevy ships [`bevy::remote`] — a JSON-RPC layer that exposes the
//! ECS generically (`world.query`, `world.get_components`,
//! `world.spawn_entity`, …). The two layers are complementary, not
//! competing:
//!
//! | Layer | Surface | Best for |
//! |-------|---------|----------|
//! | **`kyoso_agent`** (this crate) | Scene-aware verbs in `NodeRef` space | Plan-and-drill agent loops, MCP tool calls, FFI |
//! | **BRP** | Generic ECS over reflection | Debugging, inspector tools, arbitrary component data projection |
//!
//! Rule of thumb: if the question is "show me what this *scene* looks
//! like, what changed, what's connected to what" → use the kyoso
//! verbs. If the question is "give me the raw `Transform` JSON for
//! entity 42 with full reflection metadata" → use BRP. The escape
//! hatch [`SceneAgent::query`] sits between them — generic filter,
//! `NodeRef` results, no reflection serialization.
//!
//! # The verb set
//!
//! Six **read** verbs + one escape hatch on [`SceneRead`]:
//!
//! | Verb | Use when |
//! |------|----------|
//! | [`scan`](SceneAgent::scan) | Starting fresh / re-planning after `buffer_overflow`. Catalog + depth-bounded outline. |
//! | [`inspect`](SceneAgent::inspect) | You have a [`NodeRef`] and want its typed variant + component dump. |
//! | [`walk`](SceneAgent::walk) | You want a *subtree*, depth/kind/budget bounded. |
//! | [`navigate`](SceneAgent::navigate) | One hop (or transitive) along a relationship. The iterative crawl primitive. |
//! | [`r#match`](SceneAgent::r#match) | Structural search ("find every `A → B` where A is Frame, B is Text"). Takes a [`PatternSpec`] (wire-friendly). |
//! | [`watch`](SceneAgent::watch) | Coalesced change stream since a [`Cursor`]. The live loop hook. |
//! | [`query`](SceneAgent::query) | Generic ECS component-presence filter. Use when the semantic verbs above don't have a knob for what you need. |
//!
//! Four **mutate** verbs on [`SceneMutate`]:
//!
//! | Verb | Use when |
//! |------|----------|
//! | [`create`](SceneAgent::create) | Spawn a new Frame/Rectangle/Text. |
//! | [`update`](SceneAgent::update) | Apply a partial patch (rename, content edit, transform). |
//! | [`delete`](SceneAgent::delete) | Despawn (cascades via `ChildOf`). |
//! | [`r#move`](SceneAgent::r#move) | Reparent / reorder. |
//!
//! Every mutate verb returns a [`MutateResult`] carrying both the
//! post-mutation [`NodeRef`] *and* the new [`Cursor`] — feed the
//! cursor straight to [`watch`](SceneAgent::watch) to observe what
//! happens past your edit.
//!
//! # The agent loop, end-to-end
//!
//! ```no_run
//! use kyoso_agent::{
//!     NodePattern, PatternSpec, SceneAgent, SceneRead,
//!     ScanOpts, UpdatePatch, WalkOpts, WatchOpts,
//! };
//! use kyoso_core::NodeKind;
//!
//! let mut agent = SceneAgent::new();
//! # let _ = kyoso_agent::spawn_demo_scene(agent.scene_world());
//! # agent.scene_world().update();
//!
//! // 1. Catalog: cheap overview to plan from.
//! let index = agent.scan(ScanOpts::default());
//! // 2. Drill: pick a branch by cost, walk it.
//! let root = &index.roots[0].node;
//! let walk = agent.walk(root.clone(), WalkOpts { depth_limit: Some(2), ..Default::default() });
//! // 3. Find: structural query for connections.
//! let mut spec = PatternSpec::new();
//! let a = spec.add_node(NodePattern::of_kind(NodeKind::Text));
//! let b = spec.add_node(NodePattern::any());
//! spec.add_edge(a, b);
//! let matches = agent.r#match(&spec);
//! // 4. Edit: rename via the first match's source.
//! if let Some(m) = matches.first() {
//!     agent.update(m.nodes[0].clone(), UpdatePatch::default().with_text_content("new"))
//!         .unwrap();
//! }
//! // 5. Watch: feed the mutate cursor straight into the change stream.
//! //    (cursor below would come from the mutate result in real code.)
//! let baseline = agent.watch(None, WatchOpts::default()).next_cursor;
//! let _next = agent.watch(Some(baseline), WatchOpts::default());
//! ```
//!
//! # Identity & cursors
//!
//! Every result row carries a [`NodeRef`] — the content-addressed
//! handle defined in [`handle`]. It's the agent's working currency:
//! returned by every read, accepted by every method. Methods that
//! take a node accept either a raw `Entity` or a `NodeRef` via
//! `impl Into<NodeTarget>`, so the agent can pass back whatever the
//! last call returned.
//!
//! Across process boundaries the path inside `NodeRef` is canonical;
//! the `entity` cache is per-session. See [`handle`] for the full
//! identity story (`SessionId`, `ScenePath`, `Cursor`, CRDT id).
//!
//! # Architecture: trait vs. inherent
//!
//! The surface is two layers:
//!
//! - **Inherent methods on [`SceneAgent`]** — the **source of truth**.
//!   Each one delegates to a helper in one of `scan` / `walk` /
//!   `navigate` / `watch` / `mutate` / `query`, or holds its own
//!   small slice of logic (`inspect`, `r#match`). Inherent methods
//!   accept `impl Into<NodeTarget>` so Rust callers can pass an
//!   `Entity` or a `NodeRef` directly.
//!
//! - **Trait methods on [`SceneRead`] / [`SceneMutate`]** — the **wire
//!   contract**. Concrete types only (no `impl Into<…>`), object-safe,
//!   walkable by FFI/MCP codegen. Each trait method is a one-line
//!   forwarder to the inherent method.
//!
//! Drift is structurally impossible: trait impls hold no logic of
//! their own, only forward. Adding a verb edits the trait first, then
//! the inherent forwarder. Editing a verb's behaviour touches the
//! helper or the inherent body only.
//!
//! # Power-user escape hatches
//!
//! Two layers below the semantic verbs:
//!
//! - [`SceneAgent::scene_view`] — closure access to
//!   [`WorldSceneView<&SceneNode, &SceneEdge>`], the SystemParam that
//!   bundles the tree (`ChildOf` / `Children` / `OrderKey`) and the
//!   entity-edge graph (`SceneEdge` / `EdgeFrom` / `EdgeTo`). Use when
//!   you need both layers in one borrow.
//! - [`SceneAgent::find_matches`] — raw `Vec<Match>` from
//!   [`kyoso_graph`], for callers who want `Entity` bindings.
//!
//! Below those, [`SceneAgent::scene_world`] hands you the bare
//! [`kyoso_core::SceneWorld`] — the Bevy `App` is yours.
//!
//! [`bevy::remote`]: https://docs.rs/bevy/latest/bevy/remote/index.html

pub mod handle;
pub mod mutate;
pub mod navigate;
pub mod query;
pub mod scan;
pub mod tools;
pub mod walk;
pub mod watch;

pub use handle::{Cursor, NodeRef, NodeTarget, ParsePathError, PathPart, ScenePath, SessionId};
// Re-exports of the lower-level kyoso_graph traversal types — exposed
// so callers using [`SceneAgent::scene_view`] don't have to pull
// `kyoso_graph` directly for the common case. `TraversalQuery` /
// `WorldEntityRef` are also used by the existing `subtree` /
// `subtree_typed` escape hatches.
pub use kyoso_graph::traversal::{TraversalQuery, WorldSceneView};
pub use mutate::{CreateSpec, MoveSpec, MutateError, MutateResult, NewNode, UpdatePatch};
pub use navigate::{NavDir, NavEdgeFilter, NavOpts};
pub use query::{QueryResult, QuerySpec};
pub use scan::{Catalog, OutlineNode, ScanOpts, SceneIndex, SubtreeCost};
pub use tools::{SceneMutate, SceneRead};
pub use walk::{Walk, WalkOpts, WalkRow, WalkStrategy};
pub use watch::{
    Change, ChangeKind, ReplicatedOpCursor, WatchBuffer, WatchOpts, WatchPage, WatchPlugin,
    WorldGeneration,
};

use bevy::prelude::*;
use kyoso_core::{
    Frame, FrameData, Node, NodeKind, Rectangle, RectangleData, SceneEdge, SceneNode, SceneWorld,
    Text, TextData,
};
use kyoso_crdt::CrdtId;
use kyoso_graph::components::{EdgeFrom, EdgeTo};
use kyoso_graph::descriptor::SceneGraphDescriptor;
use kyoso_graph::pattern::{Direction, PEdge, PNode, Pattern, PatternBuilder};
use kyoso_graph::subgraph::Match;
use kyoso_graph::traversal::WorldEntityRef;
use kyoso_graph::traverse::GraphTraverseEdges;
use kyoso_graph::tree::OrderKey;
use kyoso_graph_sync::EntityCrdtIndex;
use serde::{Deserialize, Serialize};

/// Owned [`SceneWorld`] plus tool-shaped methods, scoped to one
/// [`SessionId`].
///
/// Construction policy:
///
/// - [`SceneAgent::new`] adds [`WatchPlugin`] for you. The watch
///   change-stream + generation counter are live from the first
///   `update()` call. The plugin auto-includes
///   `GraphManagerPlugin<SceneNode, SceneEdge>` if not already present.
///
/// - [`SceneAgent::from_scene_world`] wraps a world the caller already
///   configured. If the caller wants `watch()` to work, they must add
///   `WatchPlugin::new(agent.session())` themselves before calling
///   `watch`. The other methods (scan/inspect/walk/navigate/match)
///   work without it.
pub struct SceneAgent {
    sw: SceneWorld,
    session: SessionId,
}

impl SceneAgent {
    pub fn new() -> Self {
        let session = SessionId::new();
        let mut sw = SceneWorld::new();
        sw.app_mut().add_plugins(WatchPlugin::new(session));
        Self { sw, session }
    }

    pub fn from_scene_world(sw: SceneWorld) -> Self {
        Self {
            sw,
            session: SessionId::new(),
        }
    }

    pub fn scene_world(&mut self) -> &mut SceneWorld {
        &mut self.sw
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Resolve any `NodeTarget` (Entity or NodeRef) to a live entity in
    /// the current world. Returned None if neither hint locates a live
    /// entity.
    fn resolve(&mut self, target: impl Into<NodeTarget>) -> Option<Entity> {
        target.into().resolve(&mut self.sw, self.session)
    }

    /// Build a `NodeRef` for `entity`. Helper used internally and by
    /// callers that received an `Entity` from somewhere outside the
    /// agent surface (e.g. a `Match` row).
    pub fn node_ref(&mut self, entity: Entity) -> Option<NodeRef> {
        handle::node_ref_for(&mut self.sw, entity, self.session)
    }

    // ========================================================================
    // Scan — catalog + depth-bounded outline (Rerun-style scan)
    // ========================================================================

    /// Catalog + depth-bounded outline of the scene. Cheap, designed
    /// to be the first call an agent makes. See [`ScanOpts`].
    pub fn scan(&mut self, opts: ScanOpts) -> SceneIndex {
        scan::build_scene_index(&mut self.sw, self.session, &opts)
    }

    // ========================================================================
    // Scene-shape introspection (LLM-friendly)
    // ========================================================================

    /// Full LLM-shaped JSON dump of the scene. Each row has the
    /// node's variant tag (`"frame"` / `"rectangle"` / `"text"`),
    /// depth, and serde-encoded `data`. Prefer [`SceneAgent::scan`]
    /// for new code — it's bounded and lets the agent plan; `describe`
    /// remains for compatibility with the original demo.
    pub fn describe(&mut self) -> SceneGraphDescriptor {
        self.sw.scene_descriptor()
    }

    /// Schemaless component-name dump for a target, plus the typed
    /// variant materialisation when the target carries a known kind.
    pub fn inspect(&mut self, target: impl Into<NodeTarget>) -> EntityReport {
        let target = target.into();
        let entity = match target.resolve(&mut self.sw, self.session) {
            Some(e) => e,
            None => {
                return EntityReport {
                    node: NodeRef::from_path(ScenePath::root()),
                    entity_bits: None,
                    variant: None,
                    component_names: Vec::new(),
                };
            }
        };
        let component_names = self.sw.component_names(entity);
        let variant = self.sw.materialize_at::<SceneNode>(entity);
        let node = self
            .node_ref(entity)
            .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
        EntityReport {
            node,
            entity_bits: Some(entity.to_bits()),
            variant,
            component_names,
        }
    }

    // ========================================================================
    // Typed-variant listing — now NodeRef-shaped
    // ========================================================================

    /// Every Frame in the scene with its owned bundle data.
    pub fn list_frames(&mut self) -> Vec<(NodeRef, FrameData)> {
        let raw = self.sw.iter_as::<Frame>();
        self.with_refs(raw)
    }

    /// Every Rectangle in the scene with its owned bundle data.
    pub fn list_rectangles(&mut self) -> Vec<(NodeRef, RectangleData)> {
        let raw = self.sw.iter_as::<Rectangle>();
        self.with_refs(raw)
    }

    /// Every Text node in the scene with its owned bundle data.
    pub fn list_texts(&mut self) -> Vec<(NodeRef, TextData)> {
        let raw = self.sw.iter_as::<Text>();
        self.with_refs(raw)
    }

    fn with_refs<D>(&mut self, rows: Vec<(Entity, D)>) -> Vec<(NodeRef, D)> {
        rows.into_iter()
            .map(|(e, d)| {
                let r = self
                    .node_ref(e)
                    .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
                (r, d)
            })
            .collect()
    }

    // ========================================================================
    // Walk — subtree, depth/budget-bounded
    // ========================================================================

    /// Walk the subtree rooted at `root` with the given [`WalkOpts`].
    /// Returns one [`WalkRow`] per yielded node, each carrying a
    /// [`NodeRef`]. Truncation surfaces via `Walk::truncated`.
    pub fn walk(&mut self, root: impl Into<NodeTarget>, opts: WalkOpts) -> Walk {
        let entity = match self.resolve(root) {
            Some(e) => e,
            None => {
                return Walk {
                    root: NodeRef::from_path(ScenePath::root()),
                    rows: Vec::new(),
                    truncated: false,
                    cost: SubtreeCost::default(),
                };
            }
        };
        walk::run_walk(&mut self.sw, self.session, entity, &opts)
    }

    /// Lower-level escape hatch: raw [`TraversalQuery`] over the tree.
    /// Returns the underlying [`WorldEntityRef<CrdtId>`] rows. Prefer
    /// [`SceneAgent::walk`] for new code — `walk` returns `NodeRef`s
    /// and respects the agent-facing budget surface.
    pub fn subtree(
        &mut self,
        root: impl Into<NodeTarget>,
        query: TraversalQuery,
    ) -> Vec<WorldEntityRef<CrdtId>> {
        let entity = match self.resolve(root) {
            Some(e) => e,
            None => return Vec::new(),
        };
        self.sw.traverse(&query.start_at(entity))
    }

    /// Typed-variant escape hatch — same as [`SceneAgent::subtree`]
    /// but each row is also resolved to its [`Node`] sum-type form.
    pub fn subtree_typed(
        &mut self,
        root: impl Into<NodeTarget>,
        query: TraversalQuery,
    ) -> Vec<(WorldEntityRef<CrdtId>, Node)> {
        let entity = match self.resolve(root) {
            Some(e) => e,
            None => return Vec::new(),
        };
        self.sw.traverse_typed::<SceneNode>(&query.start_at(entity))
    }

    // ========================================================================
    // Navigate — local neighborhood
    // ========================================================================

    /// One or more hops away from `from`, in the direction(s) and over
    /// the edge family specified by [`NavOpts`]. The result is the raw
    /// crawl primitive — pass a returned `NodeRef` straight back as
    /// `from` for the next step.
    pub fn navigate(&mut self, from: impl Into<NodeTarget>, opts: NavOpts) -> Vec<NodeRef> {
        let entity = match self.resolve(from) {
            Some(e) => e,
            None => return Vec::new(),
        };
        navigate::run_navigate(&mut self.sw, self.session, entity, &opts)
    }

    // ========================================================================
    // Pattern matching
    // ========================================================================

    /// Run a [`PatternSpec`] over the scene's entity-edge graph.
    /// Returns every binding in `NodeRef` space. Mirrors
    /// [`SceneRead::r#match`] — this is the wire-friendly form
    /// (owned, serde-serialisable) so MCP / FFI callers can describe
    /// patterns without crossing a Rust lifetime.
    ///
    /// Pattern semantics:
    /// - Structural shape (node count, edges, directions, anchors)
    ///   is enforced inside the subgraph-isomorphism iterator.
    /// - Per-node filters ([`NodePattern::kind`] / [`NodePattern::name`])
    ///   are applied as a **post-match** filter — matches survive only
    ///   if every bound node satisfies its pattern's filters. This is
    ///   simpler than pushing filters into the pattern's predicates
    ///   (which would need to capture a `Query` with a delicate
    ///   lifetime). High-selectivity filters could be hoisted in a
    ///   future optimisation pass.
    pub fn r#match(&mut self, spec: &PatternSpec) -> Vec<MatchRefs> {
        // 1. Resolve anchors (NodeTarget → Entity).
        let anchors: Vec<(usize, Entity)> = spec
            .anchors
            .iter()
            .filter_map(|a| {
                let entity = a.target.clone().resolve(&mut self.sw, self.session)?;
                Some((a.pattern_node, entity))
            })
            .collect();

        // 2. Build a structural-only Pattern (predicates all `true`).
        let mut builder = PatternBuilder::new();
        let pnodes: Vec<PNode> = (0..spec.nodes.len()).map(|_| builder.node(|_| true)).collect();
        for e in &spec.edges {
            builder.edge_dir(pnodes[e.from], pnodes[e.to], e.direction.into());
        }
        for (pidx, entity) in &anchors {
            builder.anchor(pnodes[*pidx], *entity);
        }
        let pattern = builder.build();

        // 3. Run and post-filter. The two-stage form (collect after
        // filter, then map) avoids holding a `&self` borrow from the
        // filter closure across the `&mut self` borrow that
        // `match_refs` needs.
        let raw = self.find_matches(&pattern);
        let filtered: Vec<Match> = raw
            .into_iter()
            .filter(|m| self.match_passes_filters(m, spec))
            .collect();
        filtered.iter().map(|m| self.match_refs(m)).collect()
    }

    fn match_passes_filters(&self, m: &Match, spec: &PatternSpec) -> bool {
        for (i, pat) in spec.nodes.iter().enumerate() {
            let entity = m.nodes[i];
            if let Some(want_kind) = pat.kind {
                let actual = self.sw.world().get::<NodeKind>(entity).copied();
                if actual != Some(want_kind) {
                    return false;
                }
            }
            if let Some(want_name) = &pat.name {
                let frame_name = self.sw.world().get::<Frame>(entity).map(|f| f.name.as_str());
                let text_name = self.sw.world().get::<Text>(entity).map(|t| t.content.as_str());
                let actual = frame_name.or(text_name);
                if actual != Some(want_name.as_str()) {
                    return false;
                }
            }
        }
        true
    }

    /// Rust-side escape hatch: run a `Pattern<'_>` with arbitrary
    /// closure-based predicates and get `NodeRef`-shaped results.
    /// Use when [`PatternSpec`] isn't expressive enough (custom
    /// component predicates, edge-entity filters, etc.).
    pub fn match_pattern(&mut self, pattern: &Pattern<'_>) -> Vec<MatchRefs> {
        let raw = self.find_matches(pattern);
        raw.iter().map(|m| self.match_refs(m)).collect()
    }

    /// Lowest-level escape hatch — returns the raw `Vec<Match>` with
    /// `Entity` bindings. Use [`SceneAgent::r#match`] for the
    /// wire-friendly form, or [`SceneAgent::match_pattern`] for the
    /// `NodeRef`-shaped Rust form.
    pub fn find_matches(&mut self, pattern: &Pattern<'_>) -> Vec<Match> {
        let view = self.sw.scene_view();
        view.scene().graph.subgraph_matches(pattern).collect()
    }

    pub fn pattern_builder<'p>() -> PatternBuilder<'p> {
        PatternBuilder::new()
    }

    /// Convert a [`Match`]'s raw `Entity` bindings into NodeRef space.
    /// Each pattern node becomes a [`NodeRef`]; each pattern edge
    /// becomes an [`EdgeRef`] carrying the source/target NodeRefs plus
    /// the edge entity bits (edges don't sit in the tree so they don't
    /// have a path of their own — the agent identifies them by their
    /// endpoints).
    pub fn match_refs(&mut self, m: &Match) -> MatchRefs {
        let nodes: Vec<NodeRef> = m
            .nodes
            .iter()
            .map(|e| {
                self.node_ref(*e)
                    .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()))
            })
            .collect();
        let edges: Vec<EdgeRef> = m
            .edges
            .iter()
            .map(|edge_entity| {
                let from_e = self.sw.world().get::<EdgeFrom>(*edge_entity).map(|e| e.0);
                let to_e = self.sw.world().get::<EdgeTo>(*edge_entity).map(|e| e.0);
                let from = from_e
                    .and_then(|e| self.node_ref(e))
                    .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
                let to = to_e
                    .and_then(|e| self.node_ref(e))
                    .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
                EdgeRef {
                    entity: Some(edge_entity.to_bits()),
                    from,
                    to,
                }
            })
            .collect();
        MatchRefs { nodes, edges }
    }

    // ========================================================================
    // Advanced — direct SceneView access for Rust callers
    // ========================================================================

    /// Run `f` against a [`WorldSceneView`] over the scene's tree
    /// (Bevy `ChildOf` / `Children` / `OrderKey`) and entity-edge
    /// graph (`SceneEdge` / `EdgeFrom` / `EdgeTo`) as a single
    /// `SystemParam`.
    ///
    /// This is the **power-user Rust escape hatch** — for queries
    /// that need both layers in one borrow, or for traversals the
    /// semantic verbs don't expose. The closure form is required
    /// because `WorldSceneView` borrows the world; returning it
    /// would tie up the agent's borrow indefinitely.
    ///
    /// Most agent code should reach for [`Self::walk`] /
    /// [`Self::navigate`] / [`Self::scan`] first — they cover the
    /// common cases through a stable surface. Drop to `scene_view`
    /// when those don't fit.
    pub fn scene_view<F, R>(&mut self, f: F) -> R
    where
        F: for<'w, 's> FnOnce(
            &WorldSceneView<'w, 's, &'static SceneNode, &'static SceneEdge>,
        ) -> R,
    {
        let view = self.sw.scene_view();
        f(&view)
    }

    // ========================================================================
    // Query — generic ECS-component filter (escape hatch)
    // ========================================================================

    /// Generic filter by component-type-name presence. Returns
    /// `NodeRef`s the agent can feed back into [`Self::inspect`] for
    /// detail. See [`QuerySpec`] for the shape; see
    /// [`crate::query`] for the doc on when to reach for this vs. a
    /// semantic verb (or [BRP](https://docs.rs/bevy/latest/bevy/remote/index.html)
    /// for component-data projection).
    pub fn query(&mut self, spec: QuerySpec) -> QueryResult {
        query::run_query(&mut self.sw, self.session, &spec)
    }

    // ========================================================================
    // Mutate — create / update / delete / move
    // ========================================================================

    /// Spawn a new node. See [`CreateSpec`].
    pub fn create(&mut self, spec: CreateSpec) -> Result<MutateResult, MutateError> {
        mutate::create(&mut self.sw, self.session, spec)
    }

    /// Apply an [`UpdatePatch`] to `target`.
    pub fn update(
        &mut self,
        target: impl Into<NodeTarget>,
        patch: UpdatePatch,
    ) -> Result<MutateResult, MutateError> {
        mutate::update(&mut self.sw, self.session, target.into(), patch)
    }

    /// Despawn `target` (cascades through `ChildOf`).
    pub fn delete(&mut self, target: impl Into<NodeTarget>) -> Result<MutateResult, MutateError> {
        mutate::delete(&mut self.sw, self.session, target.into())
    }

    /// Reparent / reorder a node. See [`MoveSpec`].
    pub fn r#move(&mut self, spec: MoveSpec) -> Result<MutateResult, MutateError> {
        mutate::move_node(&mut self.sw, self.session, spec)
    }

    // ========================================================================
    // Watch — change stream
    // ========================================================================

    /// Drain coalesced changes since `since`. `None` = read everything
    /// in the buffer. Returns a [`WatchPage`] whose `next_cursor`
    /// should be fed into the agent's next call. On `buffer_overflow`,
    /// the agent must call [`SceneAgent::scan`] to rebuild its mental
    /// model.
    ///
    /// Requires [`WatchPlugin`] to be registered (auto-added by
    /// [`SceneAgent::new`]). For [`SceneAgent::from_scene_world`] users,
    /// see the crate-level docs.
    pub fn watch(&mut self, since: Option<Cursor>, opts: WatchOpts) -> WatchPage {
        watch::read_page(&mut self.sw, self.session, since, &opts)
    }
}

impl Default for SceneAgent {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Trait impls — the wire contract. **Pure delegation only.** Every
// method is a one-line forwarder to the inherent method of the same
// name (which holds the real logic and the `impl Into<NodeTarget>`
// sugar). Do not grow logic in this block; if you find yourself
// wanting to, the logic belongs in the inherent method or a module
// helper. See the crate-level "Architecture" doc.
// ============================================================================

impl SceneRead for SceneAgent {
    fn scan(&mut self, opts: ScanOpts) -> SceneIndex {
        self.scan(opts)
    }
    fn inspect(&mut self, target: NodeTarget) -> EntityReport {
        self.inspect(target)
    }
    fn walk(&mut self, root: NodeTarget, opts: WalkOpts) -> Walk {
        self.walk(root, opts)
    }
    fn navigate(&mut self, from: NodeTarget, opts: NavOpts) -> Vec<NodeRef> {
        self.navigate(from, opts)
    }
    fn r#match(&mut self, spec: &PatternSpec) -> Vec<MatchRefs> {
        self.r#match(spec)
    }
    fn watch(&mut self, since: Option<Cursor>, opts: WatchOpts) -> WatchPage {
        self.watch(since, opts)
    }
    fn query(&mut self, spec: QuerySpec) -> QueryResult {
        self.query(spec)
    }
}

impl SceneMutate for SceneAgent {
    fn create(&mut self, spec: CreateSpec) -> Result<MutateResult, MutateError> {
        self.create(spec)
    }
    fn update(
        &mut self,
        target: NodeTarget,
        patch: UpdatePatch,
    ) -> Result<MutateResult, MutateError> {
        self.update(target, patch)
    }
    fn delete(&mut self, target: NodeTarget) -> Result<MutateResult, MutateError> {
        self.delete(target)
    }
    fn r#move(&mut self, spec: MoveSpec) -> Result<MutateResult, MutateError> {
        self.r#move(spec)
    }
}

// ============================================================================
// Projection — shared "how much detail" knob for read verbs
// ============================================================================

/// How much per-row detail an agent wants embedded in read-verb
/// results.
///
/// Threaded through the read verbs so an agent can pay token cost in
/// proportion to what it actually plans to read — keeping the default
/// row shape small (a [`NodeRef`] and structural metadata) while
/// letting callers opt in to the typed [`Node`] sum or full component
/// dump without an [`SceneAgent::inspect`] round-trip per row.
///
/// Variant dispatch goes through
/// [`SceneWorld::materialize_at`](kyoso_core::SceneWorld::materialize_at)
/// / [`materialize_many`](kyoso_core::SceneWorld::materialize_many),
/// which are driven by the `Graph::Variants` tuple — new node variants
/// added to [`SceneNode`] are picked up automatically.
///
/// Currently consumed by [`WalkOpts::include`]. Slated to thread
/// through [`NavOpts`] / [`ScanOpts`] / [`PatternSpec`] next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeProjection {
    /// [`NodeRef`] + structural metadata only (`kind`, `depth`, …).
    /// Cheapest; matches today's row shape.
    #[default]
    Ref,
    /// Adds the typed [`Node`] sum-type materialisation. Pays one
    /// `QueryState`-tuple build per call site (amortised across the
    /// row set when the verb batches its lookups).
    Variant,
    /// `Variant` plus the schemaless component-name dump (a full
    /// archetype enumeration per row). Use when the agent needs to
    /// discover non-builtin components; pricey on large subtrees.
    Full,
}

impl NodeProjection {
    /// `true` for [`Self::Variant`] and [`Self::Full`].
    pub fn wants_variant(self) -> bool {
        matches!(self, Self::Variant | Self::Full)
    }

    /// `true` for [`Self::Full`].
    pub fn wants_component_names(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// What [`SceneAgent::inspect`] returns: the resolved [`NodeRef`],
/// the typed sum-type [`Node`] materialisation (if it matches a known
/// variant), and the schemaless component-name dump.
///
/// `entity_bits` is the live `Entity::to_bits()` at the moment of the
/// call — only meaningful when paired with this run's [`SessionId`].
/// Cross-process callers should ignore it and route work through
/// `node` ([`NodeRef`]).
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EntityReport {
    pub node: NodeRef,
    /// Per-session opaque entity handle — `Entity::to_bits()`. `None`
    /// if the target couldn't be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_bits: Option<u64>,
    /// Typed `Node` materialisation, when the entity carries a known
    /// kind. `None` for entities with no matching variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<Node>,
    pub component_names: Vec<String>,
}

impl EntityReport {
    /// Recover the live `Entity` from `entity_bits`. Caller must
    /// verify the report came from this run's [`SessionId`] before
    /// using the returned entity — there's no session check here.
    pub fn entity(&self) -> Option<Entity> {
        self.entity_bits.map(Entity::from_bits)
    }
}

/// NodeRef-shaped projection of a [`Match`] returned by
/// [`SceneAgent::match_refs`]. Indices match `PNode.0` / `PEdge.0` so
/// the [`pattern_builder()`](SceneAgent::pattern_builder)-issued
/// handles stay valid as lookups.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MatchRefs {
    pub nodes: Vec<NodeRef>,
    pub edges: Vec<EdgeRef>,
}

impl MatchRefs {
    pub fn node(&self, n: PNode) -> &NodeRef {
        &self.nodes[n.0]
    }

    pub fn edge(&self, e: PEdge) -> &EdgeRef {
        &self.edges[e.0]
    }
}

/// Agent-facing reference to an edge entity. Edges don't sit in the
/// tree, so they don't have a `ScenePath` — they're identified by the
/// `(from, to)` NodeRef pair plus the entity bits.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EdgeRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<u64>,
    pub from: NodeRef,
    pub to: NodeRef,
}

// ============================================================================
// PatternSpec — owned, serde-friendly pattern shape (the wire form of
// `kyoso_graph::pattern::Pattern<'_>`).
// ============================================================================

/// Declarative description of a subgraph pattern to match.
///
/// Crosses the FFI / MCP wire without lifetimes. Indices into `nodes`
/// and `edges` form the slot space — i.e. `MatchRefs.nodes[i]` is the
/// binding of `spec.nodes[i]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct PatternSpec {
    pub nodes: Vec<NodePattern>,
    pub edges: Vec<EdgePattern>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<PatternAnchor>,
}

impl PatternSpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a node slot, returning its index. The index is what
    /// [`EdgePattern`] / [`PatternAnchor`] refer to.
    pub fn add_node(&mut self, pattern: NodePattern) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(pattern);
        idx
    }

    pub fn add_edge(&mut self, from: usize, to: usize) -> usize {
        let idx = self.edges.len();
        self.edges.push(EdgePattern {
            from,
            to,
            direction: PatternDirection::Forward,
        });
        idx
    }

    pub fn add_anchor(&mut self, pattern_node: usize, target: NodeTarget) {
        self.anchors.push(PatternAnchor {
            pattern_node,
            target,
        });
    }
}

/// Filters that apply to a single pattern node slot. Applied as a
/// post-match step — matches survive only if every bound node passes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodePattern {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NodeKind>,
    /// Match against Frame's `name` or Text's `content`. Other variants
    /// always fail this filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl NodePattern {
    pub fn any() -> Self {
        Self::default()
    }
    pub fn of_kind(kind: NodeKind) -> Self {
        Self {
            kind: Some(kind),
            name: None,
        }
    }
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EdgePattern {
    pub from: usize,
    pub to: usize,
    #[serde(default)]
    pub direction: PatternDirection,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum PatternDirection {
    /// Match a graph edge `bound(from) → bound(to)`.
    #[default]
    Forward,
    /// Match a graph edge `bound(to) → bound(from)` (pattern arrow
    /// points opposite to the actual edge).
    Backward,
}

impl From<PatternDirection> for Direction {
    fn from(d: PatternDirection) -> Self {
        match d {
            PatternDirection::Forward => Direction::Forward,
            PatternDirection::Backward => Direction::Backward,
        }
    }
}

/// Pin a specific pattern node slot to a known scene entity.
/// Anchored matches start search from this binding.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatternAnchor {
    pub pattern_node: usize,
    pub target: NodeTarget,
}

// ============================================================================
// Test/demo helpers — fixture builders so agent tests + the demo binary
// share scene-construction code.
// ============================================================================

pub struct DemoSceneEntities {
    pub root: Entity,
    pub header: Entity,
    pub label: Entity,
    pub body: Entity,
    pub body_caption: Entity,
    /// Cross-frame edge entity from `label` → `body_caption`.
    pub label_to_caption: Entity,
}

/// Spawn a small, deliberately-shaped scene for tests + the demo:
///
/// ```text
/// root (Frame "Root")
/// ├── header (Frame "Header")
/// │   └── label (Text "Title")  ─────────────┐
/// └── body (Rectangle)                       │
///     └── body_caption (Text "Caption")  ◀───┘ (cross-frame edge)
/// ```
pub fn spawn_demo_scene(sw: &mut SceneWorld) -> DemoSceneEntities {
    use bevy::ecs::system::RunSystemOnce;

    let ents = sw
        .world_mut()
        .run_system_once(|mut commands: Commands| {
            let root = commands
                .spawn((
                    FrameData {
                        frame: Frame { name: "Root".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                ))
                .id();
            let header = commands
                .spawn((
                    FrameData {
                        frame: Frame { name: "Header".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(root),
                    OrderKey("a".into()),
                ))
                .id();
            let label = commands
                .spawn((
                    TextData {
                        text: Text { content: "Title".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(header),
                    OrderKey("a".into()),
                ))
                .id();
            let body = commands
                .spawn((
                    RectangleData::default(),
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(root),
                    OrderKey("b".into()),
                ))
                .id();
            let body_caption = commands
                .spawn((
                    TextData {
                        text: Text { content: "Caption".into(), ..default() },
                        ..default()
                    },
                    Transform::IDENTITY,
                    SceneNode,
                    ChildOf(body),
                    OrderKey("a".into()),
                ))
                .id();
            let label_to_caption = commands
                .spawn((EdgeFrom(label), EdgeTo(body_caption), SceneEdge))
                .id();
            DemoSceneEntities {
                root,
                header,
                label,
                body,
                body_caption,
                label_to_caption,
            }
        })
        .expect("spawn demo scene");

    let mut index = EntityCrdtIndex::default();
    index.bind_node(ents.root, CrdtId::new(1, 1));
    index.bind_node(ents.header, CrdtId::new(1, 2));
    index.bind_node(ents.body, CrdtId::new(1, 3));
    sw.world_mut().insert_resource(index);

    ents
}
