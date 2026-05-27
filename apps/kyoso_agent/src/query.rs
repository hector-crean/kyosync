//! `query` — the generic ECS-component filter escape hatch.
//!
//! When the semantic verbs (`scan` / `walk` / `navigate` / `r#match`)
//! don't cover what the agent needs, `query` lets it filter entities
//! by arbitrary component-type-name presence. This is the kyoso
//! analogue of BRP's `world.query`, but scoped tighter:
//!
//! - **Returns `NodeRef`s only.** No component-data projection — use
//!   [`crate::SceneAgent::inspect`] per result for typed data, or
//!   reach for BRP's `world.get_components` when the components
//!   aren't kyoso variants.
//! - **Names use the Rust type-path strings.** `"kyoso_core::Frame"`
//!   or short form `"Frame"`. Both are tried, in that order. Component
//!   types must be registered in the [`AppTypeRegistry`] (kyoso's
//!   `WatchPlugin` registers the scene variants for you).
//!
//! For Rust-side ergonomics, `QuerySpec::with_type::<T>()` /
//! `without_type::<T>()` produce the right type-name strings via
//! [`std::any::type_name`].

use bevy::ecs::component::ComponentId;
use bevy::prelude::*;
use bevy::reflect::TypeRegistry;
use kyoso_core::SceneWorld;
use serde::{Deserialize, Serialize};

use crate::handle::{node_ref_for, NodeRef, ScenePath, SessionId};

// =============================================================================
// Public types
// =============================================================================

/// Filter spec. `with`/`without` are component-type-name strings;
/// `under` confines the search to a subtree; `max_items` caps the
/// result.
#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct QuerySpec {
    /// Entities must carry **every** listed component.
    pub with: Vec<String>,
    /// Entities must carry **none** of the listed components.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub without: Vec<String>,
    /// Restrict the search to the subtree rooted at this node
    /// (inclusive). `None` = entire world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<NodeRef>,
    /// Cap on result rows. `0` = unlimited.
    #[serde(default)]
    pub max_items: u32,
}

impl QuerySpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Require component `T` to be present. Uses [`std::any::type_name`]
    /// so the registry lookup works whether the type's path matches
    /// fully or by short name.
    pub fn with_type<T: 'static>(mut self) -> Self {
        self.with.push(std::any::type_name::<T>().to_string());
        self
    }

    /// Require component `T` to be **absent**.
    pub fn without_type<T: 'static>(mut self) -> Self {
        self.without.push(std::any::type_name::<T>().to_string());
        self
    }

    pub fn under(mut self, node: NodeRef) -> Self {
        self.under = Some(node);
        self
    }

    pub fn max_items(mut self, n: u32) -> Self {
        self.max_items = n;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryResult {
    /// Matched nodes in `NodeRef` space. For non-`SceneNode` entities
    /// (e.g. edges, internal infrastructure), the `path` is empty (root).
    pub rows: Vec<NodeRef>,
    /// `max_items` truncated the result before all candidates were
    /// considered. Issue another query with a higher cap or narrower
    /// filter.
    pub truncated: bool,
    /// Component-type names from the spec that the registry didn't
    /// recognise. The query still ran with the remaining names — this
    /// field lets the agent detect typos / unregistered types instead
    /// of silently matching everything.
    pub unknown_components: Vec<String>,
}

// =============================================================================
// Implementation
// =============================================================================

pub fn run_query(sw: &mut SceneWorld, session: SessionId, spec: &QuerySpec) -> QueryResult {
    // 1. Resolve `under` to an entity (or None).
    let under_entity = spec
        .under
        .as_ref()
        .and_then(|r| r.clone().resolve(sw, session));

    // 2. Resolve component type names to ComponentIds.
    let (with_ids, with_unknown) = resolve_component_ids(sw.world(), &spec.with);
    let (without_ids, mut unknown_components) = resolve_component_ids(sw.world(), &spec.without);
    // A required-but-unresolved `with` name is logically equivalent
    // to "filter for a component no entity has" → zero matches.
    // Setting this short-circuits the candidate scan.
    let with_impossible = !with_unknown.is_empty();
    unknown_components.extend(with_unknown);

    if with_impossible {
        return QueryResult {
            rows: Vec::new(),
            truncated: false,
            unknown_components,
        };
    }

    // 3. Filter candidates.
    let max = if spec.max_items == 0 {
        usize::MAX
    } else {
        spec.max_items as usize
    };

    let candidate_entities: Vec<Entity> = match under_entity {
        Some(root) => descendants_of(sw.world(), root),
        None => sw.world().iter_entities().map(|er| er.id()).collect(),
    };

    let mut matched: Vec<Entity> = Vec::new();
    let mut truncated = false;
    {
        let world = sw.world();
        for entity in candidate_entities {
            let Ok(entity_ref) = world.get_entity(entity) else {
                continue;
            };
            let archetype = entity_ref.archetype();
            if !with_ids.iter().all(|id| archetype.contains(*id)) {
                continue;
            }
            if without_ids.iter().any(|id| archetype.contains(*id)) {
                continue;
            }
            if matched.len() >= max {
                truncated = true;
                break;
            }
            matched.push(entity);
        }
    }

    // 4. Materialise NodeRefs.
    let rows: Vec<NodeRef> = matched
        .into_iter()
        .map(|e| node_ref_for(sw, e, session).unwrap_or_else(|| NodeRef::from_path(ScenePath::root())))
        .collect();

    QueryResult {
        rows,
        truncated,
        unknown_components,
    }
}

// -----------------------------------------------------------------------------
// Component-name → ComponentId lookup
// -----------------------------------------------------------------------------

/// Look up each name in the type registry (full path first, then short
/// name) and the world's `Components`. Returns the successful
/// ComponentIds plus a list of names that didn't resolve.
fn resolve_component_ids(
    world: &World,
    names: &[String],
) -> (Vec<ComponentId>, Vec<String>) {
    let mut ids = Vec::new();
    let mut unknown = Vec::new();

    let Some(registry_res) = world.get_resource::<AppTypeRegistry>() else {
        return (ids, names.iter().cloned().collect());
    };
    let registry = registry_res.read();
    let components = world.components();

    for name in names {
        let Some(component_id) = lookup_component_id(&registry, components, name) else {
            unknown.push(name.clone());
            continue;
        };
        ids.push(component_id);
    }

    (ids, unknown)
}

fn lookup_component_id(
    registry: &TypeRegistry,
    components: &bevy::ecs::component::Components,
    name: &str,
) -> Option<ComponentId> {
    let registration = registry
        .get_with_type_path(name)
        .or_else(|| registry.get_with_short_type_path(name))?;
    let type_id = registration.type_info().type_id();
    components.get_id(type_id)
}

// -----------------------------------------------------------------------------
// Subtree enumeration for `under`
// -----------------------------------------------------------------------------

fn descendants_of(world: &World, root: Entity) -> Vec<Entity> {
    let mut stack = vec![root];
    let mut out = Vec::new();
    while let Some(e) = stack.pop() {
        out.push(e);
        if let Some(children) = world.get::<Children>(e) {
            for c in children.iter() {
                stack.push(c);
            }
        }
    }
    out
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn_demo_scene;
    use crate::watch::WatchPlugin;
    use kyoso_core::{Frame, SceneNode, Text};
    use kyoso_graph::tree::OrderKey;

    fn agent_world() -> (SceneWorld, SessionId) {
        let session = SessionId::new();
        let mut sw = SceneWorld::new();
        sw.app_mut().add_plugins(WatchPlugin::new(session));
        sw.update();
        (sw, session)
    }

    #[test]
    fn query_with_scene_node_matches_all_scene_entities() {
        let (mut sw, session) = agent_world();
        let _ents = spawn_demo_scene(&mut sw);
        sw.update();
        let spec = QuerySpec::new().with_type::<SceneNode>();
        let result = run_query(&mut sw, session, &spec);
        assert!(result.unknown_components.is_empty(), "{:?}", result.unknown_components);
        // 5 scene nodes in the demo.
        assert_eq!(result.rows.len(), 5);
    }

    #[test]
    fn query_with_frame_matches_only_frames() {
        let (mut sw, session) = agent_world();
        let _ents = spawn_demo_scene(&mut sw);
        sw.update();
        let spec = QuerySpec::new().with_type::<Frame>();
        let result = run_query(&mut sw, session, &spec);
        // 2 frames (Root, Header).
        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn query_without_excludes_matching_entities() {
        let (mut sw, session) = agent_world();
        let _ents = spawn_demo_scene(&mut sw);
        sw.update();
        // SceneNodes that don't have OrderKey = root-level (no
        // sibling-ordering). In the demo, exactly the Root.
        let spec = QuerySpec::new()
            .with_type::<SceneNode>()
            .without_type::<OrderKey>();
        let result = run_query(&mut sw, session, &spec);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].path.to_string(), "/Root");
    }

    #[test]
    fn query_under_restricts_to_subtree() {
        let (mut sw, session) = agent_world();
        let ents = spawn_demo_scene(&mut sw);
        sw.update();
        let header_ref = node_ref_for(&mut sw, ents.header, session).unwrap();
        let spec = QuerySpec::new()
            .with_type::<Text>()
            .under(header_ref);
        let result = run_query(&mut sw, session, &spec);
        // Only `label` (a Text) lives under header.
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].path.to_string(), "/Root/Header/Title");
    }

    #[test]
    fn unknown_component_name_surfaces_in_result() {
        let (mut sw, session) = agent_world();
        let _ents = spawn_demo_scene(&mut sw);
        sw.update();
        let spec = QuerySpec {
            with: vec!["MadeUpComponent".into()],
            ..Default::default()
        };
        let result = run_query(&mut sw, session, &spec);
        assert_eq!(result.unknown_components, vec!["MadeUpComponent".to_string()]);
        // No registered name in `with` → no entity can match.
        assert!(result.rows.is_empty());
    }

    #[test]
    fn max_items_truncates_and_flags() {
        let (mut sw, session) = agent_world();
        let _ents = spawn_demo_scene(&mut sw);
        sw.update();
        let spec = QuerySpec::new().with_type::<SceneNode>().max_items(2);
        let result = run_query(&mut sw, session, &spec);
        assert_eq!(result.rows.len(), 2);
        assert!(result.truncated);
    }

    #[test]
    fn kind_query_round_trips_through_short_name() {
        let (mut sw, session) = agent_world();
        let _ents = spawn_demo_scene(&mut sw);
        sw.update();
        // Use the short name "NodeKind" — the lookup falls back to it.
        let spec = QuerySpec {
            with: vec!["NodeKind".to_string()],
            ..Default::default()
        };
        let result = run_query(&mut sw, session, &spec);
        assert!(
            result.unknown_components.is_empty(),
            "short-name lookup failed: {:?}",
            result.unknown_components
        );
        // 5 entities carry NodeKind (one per SceneNode).
        assert_eq!(result.rows.len(), 5);
    }
}
