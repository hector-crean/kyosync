//! Mutation verbs — the write-side companion to the [`crate::SceneRead`]
//! verb set.
//!
//! Four operations: [`create`], [`update`], [`delete`], [`move_node`].
//! Each goes through direct ECS manipulation and then pumps one frame
//! (`SceneWorld::update`) so the change-detection pipeline flushes and
//! [`crate::watch`] sees the result before the agent's next call.
//!
//! Bypasses [`kyoso_graph::GraphCommand`] on purpose: GraphCommand is
//! intent-based and async (events consumed next frame). The agent
//! surface wants "do X, return the post-state" — synchronous. If
//! transaction recording of agent edits is wanted later, it can layer
//! on without touching this surface.

use bevy::prelude::*;
use kyoso_core::{
    Frame, FrameData, NodeKind, RectangleData, SceneNode, SceneWorld, Text, TextData,
};
use kyoso_graph::tree::OrderKey;
use serde::{Deserialize, Serialize};

use crate::handle::{node_ref_for, Cursor, NodeRef, NodeTarget, ScenePath, SessionId};
use crate::watch::{ReplicatedOpCursor, WorldGeneration};

// =============================================================================
// Specs
// =============================================================================

/// What to create + where to put it. `parent: None` makes a scene
/// root; `position: None` defaults to `OrderKey("a")` when a parent
/// is set (deterministic placement).
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateSpec {
    pub data: NewNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<OrderKey>,
}

/// The variant data for a new node. Mirrors the variant set in
/// [`kyoso_core::Node`] but is *write-shaped* — it carries the full
/// bundle the entity will be spawned with.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NewNode {
    Frame(FrameData),
    Rectangle(RectangleData),
    Text(TextData),
}

impl NewNode {
    pub fn kind(&self) -> NodeKind {
        match self {
            NewNode::Frame(_) => NodeKind::Frame,
            NewNode::Rectangle(_) => NodeKind::Rectangle,
            NewNode::Text(_) => NodeKind::Text,
        }
    }
}

/// Partial update. All fields optional — the builder methods are the
/// expected entry point. Variant-specific fields no-op silently on
/// non-matching variants today; we'll surface that as a typed error
/// once we have a stronger reason to (see [`MutateError::VariantMismatch`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct UpdatePatch {
    /// Rename a Frame. No-op on non-Frame entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_name: Option<String>,
    /// Set Text content. No-op on non-Text entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    /// Replace the entity's [`Transform`] wholesale (works on any
    /// variant — every scene entity carries one). On the wire we use
    /// [`TransformPatch`] because Bevy's `Transform` doesn't carry
    /// `Serialize`/`Deserialize` in the workspace's `default-features = false`
    /// build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TransformPatch>,
}

impl UpdatePatch {
    pub fn with_frame_name(mut self, name: impl Into<String>) -> Self {
        self.frame_name = Some(name.into());
        self
    }
    pub fn with_text_content(mut self, content: impl Into<String>) -> Self {
        self.text_content = Some(content.into());
        self
    }
    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = Some(t.into());
        self
    }
    pub fn is_empty(&self) -> bool {
        self.frame_name.is_none() && self.text_content.is_none() && self.transform.is_none()
    }
}

/// Wire-friendly mirror of Bevy's `Transform`. Translation as `[x, y, z]`,
/// rotation as `[x, y, z, w]` (quaternion), uniform/non-uniform scale as
/// `[x, y, z]`. Round-trips through `From<Transform>` / `Into<Transform>`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransformPatch {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl From<Transform> for TransformPatch {
    fn from(t: Transform) -> Self {
        Self {
            translation: t.translation.to_array(),
            rotation: t.rotation.to_array(),
            scale: t.scale.to_array(),
        }
    }
}

impl From<TransformPatch> for Transform {
    fn from(p: TransformPatch) -> Self {
        Transform {
            translation: Vec3::from_array(p.translation),
            rotation: Quat::from_array(p.rotation),
            scale: Vec3::from_array(p.scale),
        }
    }
}

/// Move (reparent / reorder) a node. `new_parent: None` promotes to
/// scene root.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveSpec {
    pub target: NodeTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_parent: Option<NodeTarget>,
    pub position: OrderKey,
}

// =============================================================================
// Results / errors
// =============================================================================

/// Returned from every mutation. `node` is the post-mutation [`NodeRef`]
/// for the affected entity (for [`delete`], it's the *pre-mutation*
/// ref since the entity no longer exists). `cursor` is the full
/// post-mutation [`Cursor`] — pass it directly to
/// [`crate::SceneRead::watch`] to observe everything *after* this
/// mutation lands.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MutateResult {
    pub node: NodeRef,
    pub cursor: Cursor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum MutateError {
    /// The target couldn't be resolved (path didn't match, entity dead, …).
    TargetNotFound,
    /// `parent` / `new_parent` couldn't be resolved.
    ParentNotFound,
    /// An `UpdatePatch` field doesn't apply to this entity's variant.
    /// E.g. `frame_name` on a Text node. Today we silently no-op for
    /// such fields; this variant is reserved for the future "strict"
    /// mode.
    #[allow(dead_code)]
    VariantMismatch {
        expected: NodeKind,
        patched: &'static str,
    },
    /// The patch had no fields set.
    EmptyPatch,
}

impl std::fmt::Display for MutateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MutateError::TargetNotFound => write!(f, "target node not found"),
            MutateError::ParentNotFound => write!(f, "parent node not found"),
            MutateError::VariantMismatch { expected, patched } => write!(
                f,
                "update patch field '{}' doesn't apply to variant {:?}",
                patched, expected
            ),
            MutateError::EmptyPatch => write!(f, "update patch had no fields set"),
        }
    }
}

impl std::error::Error for MutateError {}

// =============================================================================
// Implementations
// =============================================================================

pub fn create(
    sw: &mut SceneWorld,
    session: SessionId,
    spec: CreateSpec,
) -> Result<MutateResult, MutateError> {
    let parent_entity = match spec.parent {
        Some(target) => Some(target.resolve(sw, session).ok_or(MutateError::ParentNotFound)?),
        None => None,
    };
    let position = spec
        .position
        .or_else(|| parent_entity.map(|_| OrderKey("a".into())));

    let world = sw.world_mut();
    let entity = match spec.data {
        NewNode::Frame(data) => world.spawn((data, Transform::IDENTITY, SceneNode)).id(),
        NewNode::Rectangle(data) => world.spawn((data, Transform::IDENTITY, SceneNode)).id(),
        NewNode::Text(data) => world.spawn((data, Transform::IDENTITY, SceneNode)).id(),
    };
    if let Some(parent) = parent_entity {
        world.entity_mut(entity).insert(ChildOf(parent));
    }
    if let Some(pos) = position {
        world.entity_mut(entity).insert(pos);
    }

    sw.update();
    Ok(finalize(sw, session, entity))
}

pub fn update(
    sw: &mut SceneWorld,
    session: SessionId,
    target: NodeTarget,
    patch: UpdatePatch,
) -> Result<MutateResult, MutateError> {
    if patch.is_empty() {
        return Err(MutateError::EmptyPatch);
    }
    let entity = target.resolve(sw, session).ok_or(MutateError::TargetNotFound)?;

    if let Some(name) = patch.frame_name {
        if let Some(mut frame) = sw.world_mut().get_mut::<Frame>(entity) {
            frame.name = name;
        }
    }
    if let Some(content) = patch.text_content {
        if let Some(mut text) = sw.world_mut().get_mut::<Text>(entity) {
            text.content = content;
        }
    }
    if let Some(t) = patch.transform {
        if let Some(mut transform) = sw.world_mut().get_mut::<Transform>(entity) {
            *transform = t.into();
        }
    }

    sw.update();
    Ok(finalize(sw, session, entity))
}

pub fn delete(
    sw: &mut SceneWorld,
    session: SessionId,
    target: NodeTarget,
) -> Result<MutateResult, MutateError> {
    let entity = target.resolve(sw, session).ok_or(MutateError::TargetNotFound)?;

    // Capture the NodeRef *before* the entity dies — the watch
    // observer also captures it, but we want a synchronous answer
    // for this method's return value.
    let pre = node_ref_for(sw, entity, session)
        .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));

    sw.world_mut().despawn(entity);
    sw.update();

    let cursor = current_cursor(sw, session);
    Ok(MutateResult { node: pre, cursor })
}

pub fn move_node(
    sw: &mut SceneWorld,
    session: SessionId,
    spec: MoveSpec,
) -> Result<MutateResult, MutateError> {
    let entity = spec
        .target
        .resolve(sw, session)
        .ok_or(MutateError::TargetNotFound)?;
    let new_parent = match spec.new_parent {
        Some(t) => Some(t.resolve(sw, session).ok_or(MutateError::ParentNotFound)?),
        None => None,
    };

    let world = sw.world_mut();
    match new_parent {
        Some(p) => {
            world
                .entity_mut(entity)
                .insert((ChildOf(p), spec.position));
        }
        None => {
            world.entity_mut(entity).remove::<ChildOf>();
            world.entity_mut(entity).insert(spec.position);
        }
    }

    sw.update();
    Ok(finalize(sw, session, entity))
}

fn finalize(sw: &mut SceneWorld, session: SessionId, entity: Entity) -> MutateResult {
    let cursor = current_cursor(sw, session);
    let node = node_ref_for(sw, entity, session)
        .unwrap_or_else(|| NodeRef::from_path(ScenePath::root()));
    MutateResult { node, cursor }
}

/// Snapshot the post-mutation [`Cursor`] from the world's
/// [`WorldGeneration`] + [`ReplicatedOpCursor`] resources. Used by
/// both `finalize` and `delete` (which can't go through `finalize`
/// because the entity is gone before we build the result).
fn current_cursor(sw: &SceneWorld, session: SessionId) -> Cursor {
    let world = sw.world();
    let generation = world
        .get_resource::<WorldGeneration>()
        .copied()
        .unwrap_or_default()
        .0;
    let last_replicated_op = world
        .get_resource::<ReplicatedOpCursor>()
        .copied()
        .unwrap_or_default()
        .0;
    Cursor {
        session,
        generation,
        last_replicated_op,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_demo_scene;
    use crate::watch::WatchPlugin;
    use kyoso_core::Frame;

    fn agent_world() -> (SceneWorld, SessionId) {
        let session = SessionId::new();
        let mut sw = SceneWorld::new();
        sw.app_mut().add_plugins(WatchPlugin::new(session));
        sw.update();
        (sw, session)
    }

    #[test]
    fn create_under_parent_places_node_at_path() {
        let (mut sw, session) = agent_world();
        let parent = create(
            &mut sw,
            session,
            CreateSpec {
                data: NewNode::Frame(FrameData {
                    frame: Frame {
                        name: "P".into(),
                        ..default()
                    },
                    ..default()
                }),
                parent: None,
                position: None,
            },
        )
        .expect("create parent");
        assert_eq!(parent.node.path.to_string(), "/P");

        let child = create(
            &mut sw,
            session,
            CreateSpec {
                data: NewNode::Frame(FrameData {
                    frame: Frame {
                        name: "C".into(),
                        ..default()
                    },
                    ..default()
                }),
                parent: Some(NodeTarget::Ref(parent.node.clone())),
                position: None,
            },
        )
        .expect("create child");
        assert_eq!(child.node.path.to_string(), "/P/C");
    }

    #[test]
    fn update_changes_frame_name() {
        let (mut sw, session) = agent_world();
        let ents = spawn_demo_scene(&mut sw);
        sw.update();
        let result = update(
            &mut sw,
            session,
            NodeTarget::Entity(ents.header),
            UpdatePatch::default().with_frame_name("Renamed"),
        )
        .expect("update");
        // Path now reflects the new name.
        assert_eq!(result.node.path.to_string(), "/Root/Renamed");
        // And the actual component changed.
        let frame = sw.read_as::<Frame>(ents.header).expect("read frame");
        assert_eq!(frame.frame.name, "Renamed");
    }

    #[test]
    fn empty_patch_is_rejected() {
        let (mut sw, session) = agent_world();
        let ents = spawn_demo_scene(&mut sw);
        sw.update();
        let err = update(
            &mut sw,
            session,
            NodeTarget::Entity(ents.header),
            UpdatePatch::default(),
        )
        .unwrap_err();
        assert_eq!(err, MutateError::EmptyPatch);
    }

    #[test]
    fn delete_returns_pre_mutation_ref() {
        let (mut sw, session) = agent_world();
        let ents = spawn_demo_scene(&mut sw);
        sw.update();
        let result = delete(&mut sw, session, NodeTarget::Entity(ents.header))
            .expect("delete");
        // The returned NodeRef labels the dead entity.
        assert_eq!(result.node.path.to_string(), "/Root/Header");
        // Header is gone.
        assert!(sw.world().get_entity(ents.header).is_err());
        // Cascading: label was a child of header, so it's gone too.
        assert!(sw.world().get_entity(ents.label).is_err());
    }

    #[test]
    fn move_relocates_subtree() {
        let (mut sw, session) = agent_world();
        let ents = spawn_demo_scene(&mut sw);
        sw.update();
        // Move `label` from under `header` to under `body`.
        let result = move_node(
            &mut sw,
            session,
            MoveSpec {
                target: NodeTarget::Entity(ents.label),
                new_parent: Some(NodeTarget::Entity(ents.body)),
                position: OrderKey("z".into()),
            },
        )
        .expect("move");
        assert_eq!(result.node.path.to_string(), "/Root/[b]/Title");
    }

    #[test]
    fn missing_target_returns_error() {
        let (mut sw, session) = agent_world();
        let bogus = Entity::from_raw_u32(99_999).unwrap();
        let err = delete(&mut sw, session, NodeTarget::Entity(bogus)).unwrap_err();
        assert_eq!(err, MutateError::TargetNotFound);
    }
}
