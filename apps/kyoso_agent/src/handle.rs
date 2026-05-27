//! Stable, content-addressed references to scene nodes — the identity
//! system the agent SDK hands across process boundaries.
//!
//! Three types compose the identity story:
//!
//! - [`SessionId`] — one per `SceneAgent::new()`. Returned to the agent
//!   on connect; stamped onto every [`NodeRef`] / [`Cursor`] so the SDK
//!   can detect stale per-run caches (e.g. an `Entity` from a previous
//!   process).
//!
//! - [`ScenePath`] — content-addressed hierarchical path, Rerun-style.
//!   Middle path: a segment is **[`PathPart::Named`]** when the variant
//!   carries a human-facing name (Frame, Text), **[`PathPart::Ordered`]**
//!   when it doesn't (Rectangle). Named segments carry a `#index`
//!   disambiguator for same-named siblings (0 = first when sorted by
//!   [`OrderKey`]). This is the canonical, durable identity — survives
//!   reruns as long as the design persists.
//!
//! - [`NodeRef`] — `ScenePath` + optional cache hints (`CrdtId` for
//!   replicated nodes, `Entity` per-session). The resolver tries the
//!   cheapest hint first and falls back to walking the path. Agents
//!   round-trip these without caring which hint is live.
//!
//! - [`Cursor`] — opaque "where I was last" position for `watch(since)`.
//!   Hybrid: a session-local generation counter plus an optional CRDT
//!   op cursor for cross-process reasoning.

use std::fmt;
use std::str::FromStr;

use bevy::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use kyoso_core::{Frame, SceneNode, SceneWorld, Text};
use kyoso_crdt::CrdtId;
use kyoso_graph::tree::OrderKey;
use kyoso_graph_sync::EntityCrdtIndex;

// =============================================================================
// SessionId
// =============================================================================

/// Identifies one run of the scene agent. New UUID per `SceneAgent::new()`.
///
/// Equivalent to Rerun's `RecordingId`. An `ApplicationId` analogue (a
/// stable schema name across runs) can be layered on later if multiple
/// kyoso apps need to coexist in one agent — for now, single UUID is
/// enough to detect "this `NodeRef` is from a previous process."
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// =============================================================================
// ScenePath
// =============================================================================

/// A single segment of a [`ScenePath`].
///
/// Per the middle-path decision: prefer human-readable names when the
/// variant provides one, fall back to the tree's [`OrderKey`] when it
/// doesn't. Same-named siblings get a `#index` disambiguator on the
/// [`PathPart::Named`] form (0 = first when sorted by `OrderKey`).
///
/// Wire format (see [`ScenePath`]):
/// - `Named { name, index: 0 }` → `name` (with `/`, `#`, `[`, `]`, `\`
///   backslash-escaped inside `name`).
/// - `Named { name, index: n }` → `name#n`.
/// - `Ordered { key }` → `[key]` (with `]`, `\` escaped inside).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PathPart {
    /// Variant carries a human-facing name (Frame, Text).
    /// `index` disambiguates same-named siblings; default 0.
    Named { name: String, index: u16 },
    /// Variant has no human-facing name — use the sibling-order key.
    Ordered { key: OrderKey },
}

impl fmt::Display for PathPart {
    /// Human-readable, round-trippable rendering used by [`ScenePath::to_string`].
    ///
    /// - `Named { name, index: 0 }` → `name` (with `/`, `#`, `[`, `]`, `\` escaped as `\X`)
    /// - `Named { name, index: n }` → `name#n`
    /// - `Ordered { key }` → `[key]` (with `]`, `\` escaped inside)
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathPart::Named { name, index } => {
                write_escaped(f, name, &['/', '#', '[', ']', '\\'])?;
                if *index != 0 {
                    write!(f, "#{}", index)?;
                }
                Ok(())
            }
            PathPart::Ordered { key } => {
                write!(f, "[")?;
                write_escaped(f, &key.0, &[']', '\\'])?;
                write!(f, "]")
            }
        }
    }
}

fn write_escaped(f: &mut fmt::Formatter<'_>, s: &str, escape: &[char]) -> fmt::Result {
    for c in s.chars() {
        if escape.contains(&c) {
            write!(f, "\\{}", c)?;
        } else {
            write!(f, "{}", c)?;
        }
    }
    Ok(())
}

/// Hierarchical, content-addressed identity for a scene node.
///
/// **Wire format**: compact slash-separated string, Rerun-style. Root
/// is `/`. Each segment is rendered per [`PathPart`]'s Display
/// (`name`, `name#N`, or `[order_key]`). Special chars in names
/// (`/`, `#`, `[`, `]`, `\`) are backslash-escaped.
///
/// Both [`Serialize`] / [`Deserialize`] go through this string form so
/// the agent SDK sees `"/Root/Header/Title"` over JSON / MCP / FFI —
/// not a structured array. [`std::str::FromStr`] is the inverse.
///
/// Empty = the scene root (the synthetic parent of all top-level
/// `SceneNode` entities).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ScenePath {
    parts: Vec<PathPart>,
}

impl Serialize for ScenePath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ScenePath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ScenePath::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// `ScenePath` serializes as the slash-rendered string form (see
/// `ScenePath::Display`), so its JSON Schema is just `string`. We don't
/// emit a `format` because the grammar (Rerun-style with `#` indexing
/// and `[order_key]` segments) isn't standard.
impl schemars::JsonSchema for ScenePath {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ScenePath".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

impl ScenePath {
    /// The synthetic root — parent of every top-level `SceneNode`.
    pub fn root() -> Self {
        Self { parts: Vec::new() }
    }

    pub fn from_parts(parts: Vec<PathPart>) -> Self {
        Self { parts }
    }

    pub fn is_root(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn parts(&self) -> &[PathPart] {
        &self.parts
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &PathPart> + ExactSizeIterator {
        self.parts.iter()
    }

    pub fn last(&self) -> Option<&PathPart> {
        self.parts.last()
    }

    /// Drop the trailing segment. Returns `None` at root.
    pub fn parent(&self) -> Option<ScenePath> {
        if self.parts.is_empty() {
            return None;
        }
        let mut parts = self.parts.clone();
        parts.pop();
        Some(Self::from_parts(parts))
    }

    /// Append a segment.
    pub fn join(&self, part: PathPart) -> ScenePath {
        let mut parts = self.parts.clone();
        parts.push(part);
        Self::from_parts(parts)
    }
}

impl fmt::Display for ScenePath {
    /// Renders as `/segment1/segment2/...`. Root is `/`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_root() {
            return write!(f, "/");
        }
        for part in self.parts.iter() {
            write!(f, "/{}", part)?;
        }
        Ok(())
    }
}

/// Errors from [`ScenePath::from_str`] / [`PathPart`] parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsePathError {
    /// Path didn't begin with `/`.
    MissingLeadingSlash,
    /// A backslash appeared without a following character to escape.
    TrailingBackslash,
    /// Empty segment between slashes (e.g. `"//foo"` or trailing `/`).
    EmptySegment,
    /// `[` without matching `]`, or other malformed ordered segment.
    MalformedOrdered,
    /// Index suffix after `#` wasn't a `u16`.
    BadIndex,
}

impl fmt::Display for ParsePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParsePathError::MissingLeadingSlash => write!(f, "path must start with '/'"),
            ParsePathError::TrailingBackslash => write!(f, "trailing backslash"),
            ParsePathError::EmptySegment => write!(f, "empty path segment"),
            ParsePathError::MalformedOrdered => write!(f, "malformed ordered segment"),
            ParsePathError::BadIndex => write!(f, "invalid index after '#'"),
        }
    }
}

impl std::error::Error for ParsePathError {}

impl FromStr for ScenePath {
    type Err = ParsePathError;

    fn from_str(input: &str) -> Result<Self, ParsePathError> {
        if input.is_empty() || input == "/" {
            return Ok(Self::root());
        }
        if !input.starts_with('/') {
            return Err(ParsePathError::MissingLeadingSlash);
        }
        let segments = split_unescaped(&input[1..], '/')?;
        let parts = segments
            .into_iter()
            .map(|seg| seg.parse::<PathPart>())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_parts(parts))
    }
}

impl FromStr for PathPart {
    type Err = ParsePathError;

    /// Parse a single segment. Inputs are *still-escaped* — we own the
    /// unescape ourselves so name/order-key boundaries can be detected
    /// without double-handling backslashes.
    fn from_str(seg: &str) -> Result<Self, ParsePathError> {
        if seg.is_empty() {
            return Err(ParsePathError::EmptySegment);
        }

        // Ordered form: must start with unescaped '[' and end with
        // unescaped ']' that closes that bracket.
        if seg.starts_with('[') {
            if !seg.ends_with(']') || seg.len() < 2 {
                return Err(ParsePathError::MalformedOrdered);
            }
            let inner = &seg[1..seg.len() - 1];
            let key = unescape(inner)?;
            return Ok(PathPart::Ordered { key: OrderKey(key) });
        }

        // Named form. Look for an *unescaped* '#'; everything after
        // is a decimal index.
        let (raw_name, raw_index) = split_at_unescaped_hash(seg)?;
        let name = unescape(&raw_name)?;
        let index = match raw_index {
            Some(s) => s.parse::<u16>().map_err(|_| ParsePathError::BadIndex)?,
            None => 0,
        };
        Ok(PathPart::Named { name, index })
    }
}

/// Split on the first **unescaped** `#`, returning `(before, after)`.
fn split_at_unescaped_hash(s: &str) -> Result<(String, Option<String>), ParsePathError> {
    let mut before = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => {
                    before.push('\\');
                    before.push(next);
                }
                None => return Err(ParsePathError::TrailingBackslash),
            }
        } else if c == '#' {
            // Everything remaining is the index.
            let after: String = chars.collect();
            return Ok((before, Some(after)));
        } else {
            before.push(c);
        }
    }
    Ok((before, None))
}

/// Split a string on **unescaped** `delim`. Backslash escapes are
/// preserved in the output (segments are unescaped later by their
/// `PathPart` parser).
fn split_unescaped(s: &str, delim: char) -> Result<Vec<String>, ParsePathError> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => {
                    cur.push('\\');
                    cur.push(next);
                }
                None => return Err(ParsePathError::TrailingBackslash),
            }
        } else if c == delim {
            if cur.is_empty() {
                return Err(ParsePathError::EmptySegment);
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if cur.is_empty() {
        return Err(ParsePathError::EmptySegment);
    }
    out.push(cur);
    Ok(out)
}

/// Unescape a string by collapsing each `\X` to `X`. Errors on
/// trailing backslash.
fn unescape(s: &str) -> Result<String, ParsePathError> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(esc) => out.push(esc),
                None => return Err(ParsePathError::TrailingBackslash),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

// =============================================================================
// NodeRef
// =============================================================================

/// Agent-facing reference to a scene node — what tools return and accept.
///
/// `path` is canonical; the other fields are cache hints. Resolver
/// strategy (see [`NodeRef::resolve`]):
///
/// 1. `entity` if `session` matches the current session and the entity
///    still exists in the world.
/// 2. `crdt_id` via [`EntityCrdtIndex::entity_for_node`].
/// 3. Walk the `path` from root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeRef {
    pub path: ScenePath,
    /// Set for replicated nodes. Durable across reruns (it's the CRDT
    /// site identity), so this is the preferred fast path after `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crdt_id: Option<CrdtId>,
    /// Per-session cache. `Entity` is encoded via `Entity::to_bits()`
    /// for serde compat. Honoured only when `session` matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<u64>,
    /// Session this `entity` cache belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
}

impl NodeRef {
    /// Construct from a path with no cache hints.
    pub fn from_path(path: ScenePath) -> Self {
        Self {
            path,
            crdt_id: None,
            entity: None,
            session: None,
        }
    }

    pub fn with_crdt_id(mut self, id: CrdtId) -> Self {
        self.crdt_id = Some(id);
        self
    }

    pub fn with_cached(mut self, entity: Entity, session: SessionId) -> Self {
        self.entity = Some(entity.to_bits());
        self.session = Some(session);
        self
    }

    /// Cached `Entity` if it's still valid for this run. Honoured only
    /// when `session` matches. Doesn't verify the entity is alive —
    /// that's [`NodeRef::resolve`]'s job.
    pub fn cached_entity(&self, current_session: SessionId) -> Option<Entity> {
        if self.session? != current_session {
            return None;
        }
        Some(Entity::from_bits(self.entity?))
    }

    /// Resolve to a live `Entity` in this world. Tries each hint in
    /// order; falls back to walking `path` from root.
    pub fn resolve(&self, sw: &mut SceneWorld, current_session: SessionId) -> Option<Entity> {
        // 1. Cached entity (only if session matches and still alive).
        if let Some(e) = self.cached_entity(current_session) {
            if sw.world().get_entity(e).is_ok() {
                return Some(e);
            }
        }

        // 2. CRDT id lookup.
        if let Some(crdt) = self.crdt_id {
            if let Some(index) = sw.world().get_resource::<EntityCrdtIndex>() {
                if let Some(e) = index.entity_for_node(crdt) {
                    if sw.world().get_entity(e).is_ok() {
                        return Some(e);
                    }
                }
            }
        }

        // 3. Walk the path from root.
        resolve_path(sw, &self.path)
    }
}

// =============================================================================
// NodeTarget — convenience for methods that accept either form
// =============================================================================

/// Input form for tool methods: a raw `Entity` (fast in-process) or a
/// [`NodeRef`] (durable). Both are resolved via [`NodeTarget::resolve`].
///
/// Most `SceneAgent` methods take `impl Into<NodeTarget>` so callers
/// can pass whichever they have. Returned rows always carry `NodeRef`
/// — so a typical loop is `entity_returned_from_query → next call`,
/// with no manual conversion.
///
/// Wire form: serialized as the inner `NodeRef` (with `entity:` and
/// `session:` cache hints when present). The `Entity`-only variant
/// serializes as `{"entity": <bits>}` — only meaningful in-process,
/// which is acceptable since cross-process callers always have a
/// `NodeRef`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum NodeTarget {
    Ref(NodeRef),
    Entity(
        #[serde(with = "entity_bits")]
        #[schemars(with = "u64")]
        Entity,
    ),
}

mod entity_bits {
    use bevy::prelude::Entity;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(e: &Entity, s: S) -> Result<S::Ok, S::Error> {
        e.to_bits().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Entity, D::Error> {
        let bits = u64::deserialize(d)?;
        Ok(Entity::from_bits(bits))
    }
}

impl NodeTarget {
    pub fn resolve(&self, sw: &mut SceneWorld, current_session: SessionId) -> Option<Entity> {
        match self {
            NodeTarget::Entity(e) => {
                if sw.world().get_entity(*e).is_ok() {
                    Some(*e)
                } else {
                    None
                }
            }
            NodeTarget::Ref(r) => r.resolve(sw, current_session),
        }
    }
}

impl From<Entity> for NodeTarget {
    fn from(e: Entity) -> Self {
        NodeTarget::Entity(e)
    }
}

impl From<NodeRef> for NodeTarget {
    fn from(r: NodeRef) -> Self {
        NodeTarget::Ref(r)
    }
}

impl From<&NodeRef> for NodeTarget {
    fn from(r: &NodeRef) -> Self {
        NodeTarget::Ref(r.clone())
    }
}

// =============================================================================
// Cursor
// =============================================================================

/// Position in the event stream for `watch(since)`.
///
/// Hybrid model:
/// - `generation` — session-local counter bumped on every relevant ECS
///   change. Covers derived/read-only state too.
/// - `last_replicated_op` — opaque monotonic identifier from the sync
///   engine, present when the change came through CRDT replication.
///   Lets the agent reason about cross-process ordering with the human
///   operator's edits.
///
/// `session` lets the SDK return `buffer_overflow: true` cleanly when a
/// cursor from a prior session is presented — the agent re-scans rather
/// than receiving stale-but-plausible deltas.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Cursor {
    pub session: SessionId,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_replicated_op: Option<u64>,
}

impl Cursor {
    /// Baseline cursor for a fresh session — before any events.
    pub fn baseline(session: SessionId) -> Self {
        Self {
            session,
            generation: 0,
            last_replicated_op: None,
        }
    }

    /// Is this cursor from the current session? If not, the SDK must
    /// treat it as a buffer-overflow signal and force a re-scan.
    pub fn matches(&self, current_session: SessionId) -> bool {
        self.session == current_session
    }
}

// =============================================================================
// Path ↔ Entity resolution
// =============================================================================

/// Walk a `ScenePath` from root to `Entity`. Returns `None` if any
/// segment fails to match (renamed nodes, deleted subtrees, etc.).
pub fn resolve_path(sw: &mut SceneWorld, path: &ScenePath) -> Option<Entity> {
    let mut current_parent: Option<Entity> = None;

    for part in path.iter() {
        let candidates = children_of(sw, current_parent);
        let next = match part {
            PathPart::Named { name, index } => find_named_child(sw, &candidates, name, *index)?,
            PathPart::Ordered { key } => find_ordered_child(sw, &candidates, key)?,
        };
        current_parent = Some(next);
    }

    current_parent
}

/// Build a `NodeRef` for an existing entity by walking up to root,
/// computing path segments along the way, and consulting the CRDT
/// index for the optional id.
pub fn node_ref_for(
    sw: &mut SceneWorld,
    entity: Entity,
    session: SessionId,
) -> Option<NodeRef> {
    if sw.world().get_entity(entity).is_err() {
        return None;
    }

    let path = path_for(sw, entity)?;
    let crdt_id = sw
        .world()
        .get_resource::<EntityCrdtIndex>()
        .and_then(|idx| idx.node_id(entity));

    let mut node_ref = NodeRef::from_path(path).with_cached(entity, session);
    if let Some(id) = crdt_id {
        node_ref = node_ref.with_crdt_id(id);
    }
    Some(node_ref)
}

/// Walk `entity` → ancestors → root, building one [`PathPart`] per step.
/// Returns `None` if any ancestor isn't a `SceneNode` (shouldn't happen
/// for well-formed scenes).
pub fn path_for(sw: &mut SceneWorld, entity: Entity) -> Option<ScenePath> {
    let mut chain: Vec<Entity> = vec![entity];
    let mut cursor = entity;
    while let Some(child_of) = sw.world().get::<ChildOf>(cursor) {
        cursor = child_of.0;
        chain.push(cursor);
    }
    chain.reverse();

    let mut parts = Vec::with_capacity(chain.len());
    let mut parent: Option<Entity> = None;
    for ent in chain {
        let part = part_for_child(sw, parent, ent)?;
        parts.push(part);
        parent = Some(ent);
    }
    Some(ScenePath::from_parts(parts))
}

// -----------------------------------------------------------------------------
// Internals — children enumeration + per-part lookup.
// -----------------------------------------------------------------------------

/// Children of `parent`, or the scene roots when `parent` is `None`.
/// Restricted to entities carrying [`SceneNode`].
fn children_of(sw: &mut SceneWorld, parent: Option<Entity>) -> Vec<Entity> {
    let world = sw.world_mut();
    match parent {
        Some(p) => world
            .get::<Children>(p)
            .map(|c| {
                c.iter()
                    .filter(|child| world.get::<SceneNode>(*child).is_some())
                    .collect()
            })
            .unwrap_or_default(),
        None => {
            // Roots: entities with SceneNode and no ChildOf.
            let mut q = world.query_filtered::<Entity, (With<SceneNode>, Without<ChildOf>)>();
            q.iter(world).collect()
        }
    }
}

/// Best-effort human-facing name for a scene entity.
/// Falls back to `None` for variants that don't carry one (Rectangle).
fn display_name_of(sw: &mut SceneWorld, entity: Entity) -> Option<String> {
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

fn order_key_of(sw: &mut SceneWorld, entity: Entity) -> Option<OrderKey> {
    sw.world().get::<OrderKey>(entity).cloned()
}

/// Compute the `PathPart` for `child` *relative to* `parent`. Needs
/// the parent's other children so the same-name disambiguator (`#index`)
/// can be computed for `Named`.
fn part_for_child(
    sw: &mut SceneWorld,
    parent: Option<Entity>,
    child: Entity,
) -> Option<PathPart> {
    if let Some(name) = display_name_of(sw, child) {
        let index = same_name_index(sw, parent, child, &name);
        return Some(PathPart::Named { name, index });
    }
    let key = order_key_of(sw, child)?;
    Some(PathPart::Ordered { key })
}

/// Sibling-ordered index of `child` among siblings sharing `name`.
/// 0 = first (when sorted by `OrderKey`). Returns 0 for roots (no siblings
/// to compare against meaningfully — caller still gets a usable path).
fn same_name_index(
    sw: &mut SceneWorld,
    parent: Option<Entity>,
    child: Entity,
    name: &str,
) -> u16 {
    let siblings = children_of(sw, parent);
    let mut named: Vec<(Entity, Option<OrderKey>)> = siblings
        .into_iter()
        .filter_map(|s| {
            let n = display_name_of(sw, s)?;
            if n == name {
                Some((s, order_key_of(sw, s)))
            } else {
                None
            }
        })
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    named
        .iter()
        .position(|(e, _)| *e == child)
        .map(|i| i as u16)
        .unwrap_or(0)
}

/// Find the `index`-th child of `parent` named `name` when sorted by `OrderKey`.
fn find_named_child(
    sw: &mut SceneWorld,
    candidates: &[Entity],
    name: &str,
    index: u16,
) -> Option<Entity> {
    let mut matches: Vec<(Entity, Option<OrderKey>)> = candidates
        .iter()
        .copied()
        .filter_map(|e| {
            let n = display_name_of(sw, e)?;
            if n == name {
                Some((e, order_key_of(sw, e)))
            } else {
                None
            }
        })
        .collect();
    matches.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    matches.get(index as usize).map(|(e, _)| *e)
}

fn find_ordered_child(
    sw: &mut SceneWorld,
    candidates: &[Entity],
    target: &OrderKey,
) -> Option<Entity> {
    candidates
        .iter()
        .copied()
        .find(|e| order_key_of(sw, *e).as_ref() == Some(target))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_demo_scene;

    #[test]
    fn path_display_round_trips_shape() {
        let path = ScenePath::from_parts(vec![
            PathPart::Named {
                name: "Header".into(),
                index: 0,
            },
            PathPart::Named {
                name: "Title".into(),
                index: 2,
            },
            PathPart::Ordered {
                key: OrderKey("a".into()),
            },
        ]);
        assert_eq!(path.to_string(), "/Header/Title#2/[a]");
    }

    #[test]
    fn path_display_escapes_special_chars() {
        let part = PathPart::Named {
            name: "foo/bar#baz".into(),
            index: 0,
        };
        assert_eq!(part.to_string(), r"foo\/bar\#baz");
    }

    #[test]
    fn root_path_displays_as_slash() {
        assert_eq!(ScenePath::root().to_string(), "/");
        assert!(ScenePath::root().is_root());
    }

    fn rt(input: &str) {
        let p: ScenePath = input.parse().expect("parse");
        assert_eq!(p.to_string(), input, "round-trip mismatch");
    }

    #[test]
    fn round_trip_via_from_str() {
        rt("/");
        rt("/Header");
        rt("/Header/Title");
        rt("/Header/Title#2");
        rt("/[a]");
        rt("/Root/[b]/Caption");
        rt(r"/foo\/bar");
        rt(r"/foo\#with-hash");
        rt(r"/[esc\]bracket]");
    }

    #[test]
    fn parser_rejects_missing_leading_slash() {
        assert_eq!(
            "Header".parse::<ScenePath>().unwrap_err(),
            ParsePathError::MissingLeadingSlash,
        );
    }

    #[test]
    fn parser_rejects_empty_segment() {
        assert_eq!(
            "//foo".parse::<ScenePath>().unwrap_err(),
            ParsePathError::EmptySegment,
        );
        assert_eq!(
            "/foo/".parse::<ScenePath>().unwrap_err(),
            ParsePathError::EmptySegment,
        );
    }

    #[test]
    fn parser_rejects_bad_index() {
        assert_eq!(
            "/foo#bar".parse::<ScenePath>().unwrap_err(),
            ParsePathError::BadIndex,
        );
    }

    #[test]
    fn serde_json_round_trips_as_string() {
        let path: ScenePath = "/Root/Header/Title#1".parse().unwrap();
        let json = serde_json::to_string(&path).unwrap();
        assert_eq!(json, "\"/Root/Header/Title#1\"");
        let back: ScenePath = serde_json::from_str(&json).unwrap();
        assert_eq!(path, back);
    }

    #[test]
    fn round_trip_via_demo_scene() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();

        // Build a NodeRef for the label entity and confirm it resolves back.
        let label_ref = node_ref_for(&mut sw, ents.label, session).expect("label noderef");
        assert!(matches!(label_ref.path.last(), Some(PathPart::Named { .. })));

        let resolved = label_ref.resolve(&mut sw, session);
        assert_eq!(resolved, Some(ents.label));

        // Drop the cached entity — must still resolve via path.
        let path_only = NodeRef::from_path(label_ref.path.clone());
        assert_eq!(path_only.resolve(&mut sw, session), Some(ents.label));

        // Wrong session → cached entity ignored; CRDT id (if set) or path
        // walk still works.
        let stale_session = SessionId::new();
        assert_eq!(label_ref.resolve(&mut sw, stale_session), Some(ents.label));
    }

    #[test]
    fn crdt_bound_root_resolves_via_crdt_id() {
        let mut sw = SceneWorld::new();
        let ents = spawn_demo_scene(&mut sw);
        let session = SessionId::new();

        let root_ref = node_ref_for(&mut sw, ents.root, session).expect("root noderef");
        assert!(root_ref.crdt_id.is_some(), "root should be CRDT-bound in demo");

        // Strip path + entity cache; CRDT id must carry the resolve.
        let crdt_only = NodeRef::from_path(ScenePath::root()).with_crdt_id(root_ref.crdt_id.unwrap());
        // Path is wrong (root), CRDT id should win.
        assert_eq!(crdt_only.resolve(&mut sw, session), Some(ents.root));
    }
}
