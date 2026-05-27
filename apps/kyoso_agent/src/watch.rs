//! `watch` — the change-stream surface that turns the agent SDK from
//! "REPL over the scene" into "agent loop watching its own edits land."
//!
//! Three resources do the work:
//!
//! - [`WorldGeneration`] — a `u64` bumped on every `GraphMessage` event.
//!   The session-local cursor coordinate. Lets the agent ask "what's
//!   changed since I last looked, regardless of CRDT origin?"
//!
//! - [`ReplicatedOpCursor`] — opaque monotonic counter the sync layer
//!   bumps when it applies a replicated op. Optional — only present
//!   when something is actually doing replication. Lets the agent
//!   reason about cross-process ordering with the human operator's
//!   edits in the same stream.
//!
//! - [`WatchBuffer`] — `HashMap<Entity, Change>` with one coalesced row
//!   per affected entity. The naive "every event is a row" shape would
//!   send 50 rows for a single drag; this shape sends one. On overflow
//!   the buffer sets a flag and tells the agent to re-scan.
//!
//! Wire it in by adding [`WatchPlugin`] to the app (or letting
//! `SceneAgent::new()` do it). The plugin auto-adds
//! `GraphManagerPlugin<SceneNode, SceneEdge>` if it isn't already
//! there — `watch` reads `GraphMessage`, so something has to be
//! emitting it.

use std::collections::HashMap;

use bevy::ecs::lifecycle::Remove;
use bevy::prelude::*;
use kyoso_core::{Frame, NodeKind, SceneEdge, SceneNode, Text};
use kyoso_crdt::CrdtId;
use kyoso_graph::tree::OrderKey;
use kyoso_graph::{GraphManagerPlugin, GraphMessage};
use kyoso_graph_sync::EntityCrdtIndex;
use serde::{Deserialize, Serialize};

use crate::handle::{node_ref_for, Cursor, NodeRef, PathPart, ScenePath, SessionId};

// =============================================================================
// Resources
// =============================================================================

/// Session-local change-counter. Bumped by [`bump_generation`] each
/// time any relevant `GraphMessage` flows through. Read by tool methods
/// when they need to stamp a cursor onto a result.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct WorldGeneration(pub u64);

/// Optional cursor for changes that came in via the CRDT sync engine.
/// Bumped externally by the sync layer (kept here as a `Resource` so
/// `watch` can copy its value into the returned `Cursor` without
/// reaching into that crate). `None`/absent → no replication active.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct ReplicatedOpCursor(pub Option<u64>);

/// Default cap on entries in [`WatchBuffer`]. Tuned for ~one user-visible
/// edit-burst's worth of churn — drag = 1 entity, multi-select = a few.
/// Override via [`WatchPlugin::with_capacity`] when running at Figma scale.
pub const DEFAULT_WATCH_CAPACITY: usize = 4096;

/// Coalesced ring of changes since the last drain. Keyed by `Entity` —
/// repeated edits to the same entity collapse into one row whose
/// `change_count` records how many were folded in and whose `kind`
/// reflects the latest observation (with Added/Removed precedence
/// rules — see [`coalesce`]).
///
/// `overflowed`: set once the cap is exceeded. Stays sticky until the
/// next `scan()` (which calls [`WatchBuffer::reset`]). While set, every
/// [`watch`](crate::SceneAgent::watch) result carries
/// `buffer_overflow: true` so the agent doesn't act on a partial view.
#[derive(Resource, Debug)]
pub struct WatchBuffer {
    pub capacity: usize,
    pub session: SessionId,
    /// Per-entity coalesced row. `None`-key never happens — kept as a
    /// `HashMap<Entity, _>` for cheap dedup.
    pub rows: HashMap<Entity, RawChange>,
    pub overflowed: bool,
}

/// Pre-resolution version of [`Change`]. Stored in the buffer because
/// computing a `NodeRef` requires `&mut SceneWorld`, which the drain
/// system doesn't have cheap access to in the moment — we materialise
/// the ref lazily on `watch()` read.
///
/// For [`ChangeKind::Removed`] events the path is captured *before*
/// the entity dies, by the `capture_removal_path` observer, and
/// stashed in `pre_resolved`. The agent thus gets a meaningful path
/// for deletions instead of a dead-entity stub.
#[derive(Clone, Debug)]
pub struct RawChange {
    pub entity: Entity,
    pub kind: ChangeKind,
    pub last_generation: u64,
    pub change_count: u32,
    pub pre_resolved: Option<NodeRef>,
}

/// Pre-removal path snapshots. Populated by the
/// `capture_removal_path` observer (which fires *before* the
/// `SceneNode` component is removed, so it can still read the entity's
/// place in the tree). Drained on each `NodeRemoved` GraphMessage —
/// the snapshot rides along on the `RawChange::pre_resolved` slot.
///
/// Surviving entries (entities removed-and-observed but never reported
/// via `GraphMessage`) are GC'd opportunistically — the map is sized
/// by the cap on `WatchBuffer`.
#[derive(Resource, Debug, Default)]
pub struct RemovedPaths {
    pub paths: HashMap<Entity, NodeRef>,
}

impl WatchBuffer {
    pub fn new(capacity: usize, session: SessionId) -> Self {
        Self {
            capacity,
            session,
            rows: HashMap::new(),
            overflowed: false,
        }
    }

    /// Drop all rows and clear the overflow flag. Called after the
    /// agent re-scans — they've thrown away their mental model anyway,
    /// so the buffer no longer needs to remember anything.
    pub fn reset(&mut self) {
        self.rows.clear();
        self.overflowed = false;
    }

    /// Coalesce a fresh observation into the buffer. `pre_resolved`
    /// carries a pre-removal path snapshot — set for `Removed` events
    /// captured by `capture_removal_path`, `None` otherwise.
    pub fn record(
        &mut self,
        entity: Entity,
        kind: ChangeKind,
        generation: u64,
        pre_resolved: Option<NodeRef>,
    ) {
        if self.rows.len() >= self.capacity && !self.rows.contains_key(&entity) {
            self.overflowed = true;
            return;
        }
        self.rows
            .entry(entity)
            .and_modify(|row| {
                row.kind = coalesce(row.kind, kind);
                row.last_generation = generation;
                row.change_count = row.change_count.saturating_add(1);
                // Preserve any pre-resolved snapshot if we already
                // captured one for this entity (e.g. Removed → Added →
                // Removed in the same tick).
                if pre_resolved.is_some() {
                    row.pre_resolved = pre_resolved.clone();
                }
            })
            .or_insert(RawChange {
                entity,
                kind,
                last_generation: generation,
                change_count: 1,
                pre_resolved,
            });
    }
}

/// Coalescing rules for repeated events on the same entity within a
/// drain cycle. Final-state semantics: the agent wants to know where
/// it ended, not every step along the way.
///
/// - `Added → Removed` = `Removed` (created then deleted — net deleted)
/// - `Removed → Added` = `Added` (restored, e.g. undo)
/// - anything → `Removed` = `Removed`
/// - `Added` survives further `Modified`/`Moved` updates
/// - `Modified` ↔ `Moved` collapse to `Modified` (both mean "look again")
fn coalesce(prev: ChangeKind, next: ChangeKind) -> ChangeKind {
    use ChangeKind::*;
    match (prev, next) {
        (Added, Removed) | (Removed, Removed) | (Modified, Removed) | (Moved, Removed) => Removed,
        (Removed, Added) => Added,
        (Added, _) => Added,
        (_, Added) => Added, // shouldn't happen but harmless
        (_, Modified) | (_, Moved) => Modified,
    }
}

// =============================================================================
// Public types
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    Moved,
}

/// One row in the [`WatchPage`] result. `node` carries the path + cache
/// hints; `kind` is the coalesced kind; `change_count` is how many raw
/// events were folded into this row.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Change {
    pub node: NodeRef,
    pub kind: ChangeKind,
    pub change_count: u32,
    pub generation: u64,
    /// Best-effort `NodeKind` — present when the entity still exists
    /// at drain time. `None` for `Removed` and for any node the
    /// resolver can't classify.
    pub node_kind: Option<NodeKind>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct WatchOpts {
    /// Confine to changes whose `entity` resolves under this subtree.
    /// `None` = whole scene.
    pub under: Option<NodeRef>,
    /// Filter to specific kinds. Empty = no filter.
    pub kinds: Vec<NodeKind>,
    /// Filter to specific change kinds. Empty = no filter.
    pub events: Vec<ChangeKind>,
    /// Cap on rows in the returned page. 0 = unlimited.
    pub max_items: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchPage {
    pub changes: Vec<Change>,
    pub next_cursor: Cursor,
    /// More changes exist past `max_items` — issue another `watch`
    /// with `next_cursor` to get them. Today's draft drains in one
    /// pass so this is always false; left in the type so the wire
    /// shape doesn't need a future change.
    pub has_more: bool,
    /// Ring buffer evicted before the agent read. The agent's
    /// incremental view is gone; the recovery path is `scan()` →
    /// rebuild the mental model.
    pub buffer_overflow: bool,
}

// =============================================================================
// Plugin
// =============================================================================

/// Adds the change-counter, ring buffer, and the drain system that
/// folds [`GraphMessage`]s into [`WatchBuffer`]. Auto-includes
/// `GraphManagerPlugin<SceneNode, SceneEdge>` if not already present —
/// that's the source of the events we drain.
pub struct WatchPlugin {
    pub session: SessionId,
    pub capacity: usize,
}

impl WatchPlugin {
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            capacity: DEFAULT_WATCH_CAPACITY,
        }
    }

    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }
}

impl Plugin for WatchPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<GraphManagerPlugin<SceneNode, SceneEdge>>() {
            app.add_plugins(GraphManagerPlugin::<SceneNode, SceneEdge>::new());
        }
        // Register the scene component types with the AppTypeRegistry
        // so the agent's `query` verb can look them up by Rust type
        // path. Bevy's derive(Reflect) only puts them in the registry
        // when we explicitly register here.
        app.register_type::<SceneNode>()
            .register_type::<SceneEdge>()
            .register_type::<NodeKind>()
            .register_type::<Frame>()
            .register_type::<Text>()
            .register_type::<OrderKey>();
        app.insert_resource(WorldGeneration::default())
            .insert_resource(ReplicatedOpCursor::default())
            .insert_resource(WatchBuffer::new(self.capacity, self.session))
            .insert_resource(RemovedPaths::default())
            .add_systems(
                Update,
                drain_messages
                    .after(kyoso_graph::GraphSystemSet::EventPropagation),
            )
            .add_observer(capture_removal_path);
    }
}

// =============================================================================
// Drain system
// =============================================================================

/// Consume `GraphMessage`s and fold them into the buffer. Runs after
/// `EventPropagation` so `PropagationTriggered` events (which carry
/// affected-neighbor lists) are observable here too.
///
/// For `NodeRemoved` events, the path snapshot captured by the
/// `capture_removal_path` observer is moved out of [`RemovedPaths`]
/// and ridden onto the corresponding [`RawChange::pre_resolved`].
fn drain_messages(
    mut reader: MessageReader<GraphMessage>,
    mut buffer: ResMut<WatchBuffer>,
    mut world_gen: ResMut<WorldGeneration>,
    mut removed_paths: ResMut<RemovedPaths>,
) {
    for msg in reader.read() {
        world_gen.0 = world_gen.0.saturating_add(1);
        let g = world_gen.0;
        match msg {
            GraphMessage::NodeAdded { entity, .. } => {
                buffer.record(*entity, ChangeKind::Added, g, None);
            }
            GraphMessage::NodeRemoved { entity, .. } => {
                let snapshot = removed_paths.paths.remove(entity);
                buffer.record(*entity, ChangeKind::Removed, g, snapshot);
            }
            GraphMessage::NodeChanged { entity, .. } => {
                buffer.record(*entity, ChangeKind::Modified, g, None);
            }
            GraphMessage::TreePositionChanged { entity, .. } => {
                buffer.record(*entity, ChangeKind::Moved, g, None);
            }
            // Edge-level events don't get their own row today — they
            // surface to the agent as `Modified` on the endpoints via
            // the propagation pass.
            GraphMessage::EdgeAdded { .. }
            | GraphMessage::EdgeRemoved { .. }
            | GraphMessage::EdgeChanged { .. }
            | GraphMessage::NodeConnected { .. }
            | GraphMessage::NodeDisconnected { .. }
            | GraphMessage::PropagationTriggered { .. } => {}
        }
    }
}

/// Fires *before* a `SceneNode` component is removed, while the
/// entity still has its full archetype. Walks `ChildOf` to build a
/// best-effort [`ScenePath`] for the soon-to-be-dead entity and
/// stashes the result in [`RemovedPaths`] so [`drain_messages`] can
/// attach it to the corresponding `NodeRemoved` event.
///
/// Same-name disambiguation (`#index`) is **not** computed here — the
/// observer doesn't have cheap access to sibling lookups, and the
/// captured path is only used as a label for a dead entity (no
/// re-resolution required). For namespace collisions, the label may
/// alias; the cached `Entity` bits still identify the row uniquely.
fn capture_removal_path(
    trigger: On<Remove, SceneNode>,
    names: Query<(Option<&Frame>, Option<&Text>, Option<&OrderKey>, Option<&ChildOf>)>,
    crdt_index: Option<Res<EntityCrdtIndex>>,
    buffer: Res<WatchBuffer>,
    mut removed_paths: ResMut<RemovedPaths>,
) {
    let entity = trigger.entity;

    // Walk up the tree: entity, parent, parent's parent, …
    let mut chain: Vec<Entity> = vec![entity];
    let mut cursor = entity;
    while let Ok((_, _, _, parent)) = names.get(cursor) {
        match parent {
            Some(p) => {
                cursor = p.0;
                chain.push(cursor);
            }
            None => break,
        }
    }
    chain.reverse();

    let mut parts = Vec::with_capacity(chain.len());
    for ent in chain {
        let Ok((frame, text, order, _)) = names.get(ent) else {
            continue;
        };
        let part = if let Some(f) = frame.filter(|f| !f.name.is_empty()) {
            PathPart::Named { name: f.name.clone(), index: 0 }
        } else if let Some(t) = text.filter(|t| !t.content.is_empty()) {
            PathPart::Named { name: t.content.clone(), index: 0 }
        } else if let Some(k) = order {
            PathPart::Ordered { key: k.clone() }
        } else {
            // Last-resort label so the path stays non-empty.
            PathPart::Ordered { key: OrderKey(String::new()) }
        };
        parts.push(part);
    }

    let crdt_id: Option<CrdtId> = crdt_index
        .as_ref()
        .and_then(|idx| idx.node_id(entity));

    let mut node_ref = NodeRef::from_path(ScenePath::from_parts(parts))
        .with_cached(entity, buffer.session);
    if let Some(id) = crdt_id {
        node_ref = node_ref.with_crdt_id(id);
    }

    removed_paths.paths.insert(entity, node_ref);
}

// =============================================================================
// Read path — used by `SceneAgent::watch`
// =============================================================================

/// Read out a [`WatchPage`] starting at `since`. Materialises a
/// `NodeRef` for each row that's still resolvable; rows for removed
/// entities get a stub `NodeRef` with just the entity bits.
///
/// Mutating: clears the rows we returned. Idempotent on re-read of
/// the same cursor (those rows are already drained).
pub fn read_page(
    sw: &mut kyoso_core::SceneWorld,
    session: SessionId,
    since: Option<Cursor>,
    opts: &WatchOpts,
) -> WatchPage {
    let cursor_session_mismatch = since
        .as_ref()
        .map(|c| !c.matches(session))
        .unwrap_or(false);

    let since_gen = since.as_ref().map(|c| c.generation).unwrap_or(0);

    // Snapshot state out of the buffer first; we'll mutate ECS to build
    // NodeRefs after, which requires &mut World.
    let (raw_rows, current_gen, current_op, overflowed) = {
        let world = sw.world();
        let buffer = world
            .get_resource::<WatchBuffer>()
            .expect("WatchPlugin must be registered before calling watch()");
        let world_gen = world
            .get_resource::<WorldGeneration>()
            .copied()
            .unwrap_or_default();
        let op = world
            .get_resource::<ReplicatedOpCursor>()
            .copied()
            .unwrap_or_default();

        let raw: Vec<RawChange> = buffer
            .rows
            .values()
            .filter(|r| r.last_generation > since_gen)
            .cloned()
            .collect();
        (raw, world_gen.0, op.0, buffer.overflowed)
    };

    let next_cursor = Cursor {
        session,
        generation: current_gen,
        last_replicated_op: current_op,
    };

    if cursor_session_mismatch || overflowed {
        return WatchPage {
            changes: Vec::new(),
            next_cursor,
            has_more: false,
            buffer_overflow: true,
        };
    }

    let max = if opts.max_items == 0 {
        usize::MAX
    } else {
        opts.max_items as usize
    };

    let mut changes = Vec::with_capacity(raw_rows.len().min(max));
    let under = opts.under.clone();
    let under_entity = under
        .as_ref()
        .and_then(|r| r.resolve(sw, session));

    for raw in raw_rows {
        if !opts.events.is_empty() && !opts.events.contains(&raw.kind) {
            continue;
        }
        let node_kind = sw.world().get::<NodeKind>(raw.entity).copied();
        if !opts.kinds.is_empty() {
            match node_kind {
                Some(k) if opts.kinds.contains(&k) => {}
                _ => continue,
            }
        }
        if let Some(root) = under_entity {
            // Removed-with-snapshot rows can't be checked against the
            // live tree — the entity is gone. Always include them
            // when an `under` filter is set; the agent can re-filter
            // by path prefix on its side.
            if raw.pre_resolved.is_none() && !is_descendant_of(sw, raw.entity, root) {
                continue;
            }
        }
        // For Removed events with a pre-captured snapshot, prefer it
        // over a live re-resolve (the entity is gone).
        let node = match (&raw.pre_resolved, raw.kind) {
            (Some(snapshot), ChangeKind::Removed) => snapshot.clone(),
            _ => node_ref_for(sw, raw.entity, session)
                .or_else(|| raw.pre_resolved.clone())
                .unwrap_or_else(|| NodeRef::from_path(crate::handle::ScenePath::root())),
        };
        changes.push(Change {
            node,
            kind: raw.kind,
            change_count: raw.change_count,
            generation: raw.last_generation,
            node_kind,
        });
        if changes.len() >= max {
            break;
        }
    }

    // Drain the rows we just returned.
    if let Some(mut buffer) = sw.world_mut().get_resource_mut::<WatchBuffer>() {
        buffer
            .rows
            .retain(|_, r| r.last_generation <= since_gen);
    }

    WatchPage {
        changes,
        next_cursor,
        has_more: false,
        buffer_overflow: false,
    }
}

fn is_descendant_of(sw: &mut kyoso_core::SceneWorld, mut e: Entity, ancestor: Entity) -> bool {
    if e == ancestor {
        return true;
    }
    let world = sw.world();
    while let Some(co) = world.get::<ChildOf>(e) {
        if co.0 == ancestor {
            return true;
        }
        e = co.0;
    }
    false
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::SessionId;
    use kyoso_core::{Frame, FrameData, SceneNode, SceneWorld};

    fn world_with_plugin(session: SessionId) -> SceneWorld {
        let mut sw = SceneWorld::new();
        sw.app_mut().add_plugins(WatchPlugin::new(session));
        sw
    }

    #[test]
    fn fresh_buffer_returns_empty_page() {
        let session = SessionId::new();
        let mut sw = world_with_plugin(session);
        sw.update();

        let page = read_page(&mut sw, session, None, &WatchOpts::default());
        assert!(page.changes.is_empty());
        assert!(!page.buffer_overflow);
        assert_eq!(page.next_cursor.session, session);
    }

    #[test]
    fn spawning_a_node_shows_up_as_added() {
        let session = SessionId::new();
        let mut sw = world_with_plugin(session);
        sw.update(); // baseline

        let cursor_before = read_page(&mut sw, session, None, &WatchOpts::default()).next_cursor;

        sw.world_mut().spawn((
            FrameData {
                frame: Frame {
                    name: "X".into(),
                    ..default()
                },
                ..default()
            },
            Transform::IDENTITY,
            SceneNode,
        ));
        sw.update();

        let page = read_page(&mut sw, session, Some(cursor_before), &WatchOpts::default());
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].kind, ChangeKind::Added);
        assert_eq!(page.changes[0].node_kind, Some(NodeKind::Frame));
    }

    #[test]
    fn cross_session_cursor_signals_overflow() {
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let mut sw = world_with_plugin(session_a);
        sw.update();

        // Cursor from a foreign session.
        let foreign = Cursor::baseline(session_b);
        let page = read_page(&mut sw, session_a, Some(foreign), &WatchOpts::default());
        assert!(page.buffer_overflow);
    }

    #[test]
    fn removal_captures_pre_removal_path() {
        let session = SessionId::new();
        let mut sw = world_with_plugin(session);
        sw.update();

        // Spawn a named Frame and pump.
        let entity = sw
            .world_mut()
            .spawn((
                FrameData {
                    frame: Frame {
                        name: "Doomed".into(),
                        ..default()
                    },
                    ..default()
                },
                Transform::IDENTITY,
                SceneNode,
            ))
            .id();
        sw.update();

        // Drain the Added events to advance the cursor past the spawn.
        let cursor_after_spawn =
            read_page(&mut sw, session, None, &WatchOpts::default()).next_cursor;

        // Despawn and pump.
        sw.world_mut().despawn(entity);
        sw.update();

        let page = read_page(
            &mut sw,
            session,
            Some(cursor_after_spawn),
            &WatchOpts::default(),
        );
        assert_eq!(page.changes.len(), 1);
        let change = &page.changes[0];
        assert_eq!(change.kind, ChangeKind::Removed);
        // The path was captured *before* the despawn — so the row
        // carries the meaningful label, not a root stub.
        assert_eq!(change.node.path.to_string(), "/Doomed");
    }

    #[test]
    fn coalesce_added_then_removed_is_removed() {
        assert_eq!(coalesce(ChangeKind::Added, ChangeKind::Removed), ChangeKind::Removed);
        assert_eq!(coalesce(ChangeKind::Removed, ChangeKind::Added), ChangeKind::Added);
        assert_eq!(coalesce(ChangeKind::Modified, ChangeKind::Moved), ChangeKind::Modified);
        assert_eq!(coalesce(ChangeKind::Added, ChangeKind::Modified), ChangeKind::Added);
    }
}
