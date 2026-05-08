//! [`bevy_picking`] backend for infinite planes (analytical intersection, no mesh).
//!
//! Emits [`PointerHits`](bevy::picking::backend::PointerHits) so hover, press, and click use the same
//! pipeline as mesh and volume backends. Depth is the ray parameter from
//! [`Ray3d::intersect_plane`](bevy::math::Ray3d::intersect_plane), matching
//! [`MeshPickingPlugin`](bevy::picking::mesh_picking::MeshPickingPlugin) hit distance.
//!
//! # UI vs infinite plane
//!
//! A plane hit exists for almost every screen ray, so it can still appear in [`PointerHits`] even
//! when the user is over UI (depending on hover stacking). When
//! [`AnalyticalPlanePickingSettings::suppress_hits_when_pointer_over_ui`] is `true` (default), this
//! backend skips emitting hits for pointers whose **previous frame** hover set included any entity
//! with a UI [`Node`]. That uses one-frame-late UI state but matches when [`HoverMap`] is updated
//! relative to [`PickingSystems::Backend`].
//!
//! # Scope
//!
//! This crate intentionally implements **only** infinite-plane tests. Broader implicit surfaces
//! (signed-distance ray marching, GPU ID buffers) are left to future work if a concrete need
//! appears—they have different performance and integration trade-offs.

use bevy::picking::backend::{ray::RayMap, HitData, PointerHits};
use bevy::picking::hover::HoverMap;
use bevy::picking::pointer::PointerId;
use bevy::picking::prelude::Pickable;
use bevy::picking::PickingSystems;
use bevy::prelude::*;

/// Infinite plane registered with the analytical picking backend.
///
/// Attach to the entity that should receive pointer hits (often a logical parent; child meshes can
/// use [`Pickable::IGNORE`] so only this backend reports the ground).
#[derive(Component, Clone, Copy, Debug, Reflect)]
#[reflect(Component, Default, Debug, Clone)]
pub struct AnalyticalInfinitePlane {
    pub origin: Vec3,
    pub normal: Vec3,
}

impl AnalyticalInfinitePlane {
    pub fn new(origin: Vec3, normal: Vec3) -> Self {
        Self {
            origin,
            normal: normal.normalize_or_zero(),
        }
    }
}

impl Default for AnalyticalInfinitePlane {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            normal: Vec3::Y,
        }
    }
}

/// Toggles [`AnalyticalPlanePickingPlugin`] behaviour at runtime.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default, Debug, Clone)]
pub struct AnalyticalPlanePickingSettings {
    /// When `true`, omit plane [`PointerHits`] for a pointer if that pointer’s prior-frame
    /// [`HoverMap`] entry includes any entity with a UI [`Node`].
    pub suppress_hits_when_pointer_over_ui: bool,
}

impl Default for AnalyticalPlanePickingSettings {
    fn default() -> Self {
        Self {
            suppress_hits_when_pointer_over_ui: true,
        }
    }
}

#[inline]
fn pointer_hovered_ui(
    pointer: PointerId,
    hover_map: &HoverMap,
    ui_nodes: &Query<(), With<Node>>,
) -> bool {
    let Some(under_pointer) = hover_map.get(&pointer) else {
        return false;
    };
    under_pointer
        .keys()
        .any(|&entity| ui_nodes.get(entity).is_ok())
}

/// Registers [`analytical_plane_picking_backend`] in [`PickingSystems::Backend`].
pub struct AnalyticalPlanePickingPlugin;

impl Plugin for AnalyticalPlanePickingPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<AnalyticalInfinitePlane>()
            .register_type::<AnalyticalPlanePickingSettings>()
            .init_resource::<AnalyticalPlanePickingSettings>()
            .add_systems(
                PreUpdate,
                analytical_plane_picking_backend.in_set(PickingSystems::Backend),
            );
    }
}

fn analytical_plane_picking_backend(
    settings: Res<AnalyticalPlanePickingSettings>,
    hover_map: Res<HoverMap>,
    ray_map: Res<RayMap>,
    planes: Query<(
        Entity,
        &AnalyticalInfinitePlane,
        Option<&Pickable>,
        Option<&ViewVisibility>,
    )>,
    cameras: Query<&Camera>,
    ui_nodes: Query<(), With<Node>>,
    mut output: MessageWriter<PointerHits>,
) {
    for (&ray_id, &ray) in ray_map.iter() {
        if settings.suppress_hits_when_pointer_over_ui
            && pointer_hovered_ui(ray_id.pointer, &hover_map, &ui_nodes)
        {
            continue;
        }

        let Ok(camera) = cameras.get(ray_id.camera) else {
            continue;
        };
        let order = camera.order as f32;
        let mut picks = Vec::new();

        for (entity, plane, pickable, view_vis) in &planes {
            if view_vis.is_some_and(|v| !v.get()) {
                continue;
            }
            if pickable.is_some_and(|p| !p.is_hoverable) {
                continue;
            }

            let n = plane.normal.normalize_or_zero();
            if n.length_squared() < 1e-12 {
                continue;
            }

            let inf_plane = InfinitePlane3d::new(n);
            let Some(distance) = ray.intersect_plane(plane.origin, inf_plane) else {
                continue;
            };

            let position = ray.get_point(distance);
            picks.push((
                entity,
                HitData::new(
                    ray_id.camera,
                    distance,
                    Some(position),
                    Some(n),
                ),
            ));
        }

        if !picks.is_empty() {
            output.write(PointerHits::new(ray_id.pointer, picks, order));
        }
    }
}
