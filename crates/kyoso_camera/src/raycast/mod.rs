use bevy::picking::backend::ray::RayMap;
use bevy::picking::pointer::PointerId;
use bevy::prelude::*;

pub mod analytical_plane_picking;
pub mod primitive;

pub use analytical_plane_picking::{
    AnalyticalInfinitePlane, AnalyticalPlanePickingPlugin, AnalyticalPlanePickingSettings,
};
pub use primitive::{
    IntersectablePlane, PlaneRayIntersect, PlaneRayIntersection, RayIntersect, RayIntersection,
};

// ---------------------------------------------------------------------------
// RayMapExt -- work with Bevy's pre-computed pointer rays
// ---------------------------------------------------------------------------

/// Extension trait for [`RayMap`] providing convenient ray-surface intersection
/// methods using Bevy's pre-computed pointer rays.
///
/// While [`CameraRaycast`] constructs rays from a Camera+Transform+screen-pos
/// (useful for custom viewports and probes), `RayMapExt` uses the rays that
/// Bevy's picking pipeline has already built in `PreUpdate`. These are
/// guaranteed to match the rays used by picking backends.
pub trait RayMapExt {
    /// Get the pre-computed world-space ray for a given camera and pointer.
    fn get_pointer_ray(&self, camera: Entity, pointer: PointerId) -> Option<&Ray3d>;

    /// Intersect the pointer ray with an infinite plane defined by origin and normal.
    fn pointer_plane_intersection(
        &self,
        camera: Entity,
        pointer: PointerId,
        plane_origin: Vec3,
        plane_normal: Vec3,
    ) -> Option<Vec3>;

    /// Intersect the pointer ray with a [`DrawingSurface`].
    fn pointer_surface_intersection(
        &self,
        camera: Entity,
        pointer: PointerId,
        surface: &dyn DrawingSurface,
    ) -> Option<Vec3>;
}

impl RayMapExt for RayMap {
    fn get_pointer_ray(&self, camera: Entity, pointer: PointerId) -> Option<&Ray3d> {
        use bevy::picking::backend::ray::RayId;
        self.map.get(&RayId { camera, pointer })
    }

    fn pointer_plane_intersection(
        &self,
        camera: Entity,
        pointer: PointerId,
        plane_origin: Vec3,
        plane_normal: Vec3,
    ) -> Option<Vec3> {
        let ray = self.get_pointer_ray(camera, pointer)?;
        let plane = InfinitePlane3d::new(plane_normal);
        let distance = ray.intersect_plane(plane_origin, plane)?;
        Some(ray.get_point(distance))
    }

    fn pointer_surface_intersection(
        &self,
        camera: Entity,
        pointer: PointerId,
        surface: &dyn DrawingSurface,
    ) -> Option<Vec3> {
        let ray = self.get_pointer_ray(camera, pointer)?;
        surface.ray_to_surface_point(*ray)
    }
}

// ---------------------------------------------------------------------------
// DrawingSurface trait (used by RayMapExt::pointer_surface_intersection)
// ---------------------------------------------------------------------------

/// Abstracts the surface that tools project pointer input onto.
pub trait DrawingSurface {
    fn ray_to_surface_point(&self, ray: Ray3d) -> Option<Vec3>;
    fn world_to_uv(&self, point: Vec3) -> Option<Vec2>;
    fn uv_to_world(&self, uv: Vec2) -> Vec3;
}

/// Extension trait for [`Camera`] providing raycasting utilities.
pub trait CameraRaycast {
    /// Get the world position where a cursor ray intersects a plane defined by an origin and normal.
    fn get_cursor_world_position(
        &self,
        transform: &GlobalTransform,
        cursor_pos: Vec2,
        plane_origin: Vec3,
        plane_normal: Vec3,
    ) -> Option<Vec3>;

    /// Get the world position at a fixed distance along the cursor ray.
    fn get_cursor_world_position_fixed_distance(
        &self,
        transform: &GlobalTransform,
        cursor_pos: Vec2,
        distance: f32,
    ) -> Option<Vec3>;

    /// Get the world position on a plane parallel to the camera's view plane that passes through a reference point.
    ///
    /// This is useful for dragging operations where you want to maintain the depth of a reference point
    /// while moving along a plane perpendicular to the camera's forward direction.
    ///
    /// # Arguments
    /// * `transform` - The camera's global transform
    /// * `cursor_pos` - The cursor position in viewport coordinates
    /// * `reference_point` - A world-space point that defines the depth of the plane
    ///
    /// # Returns
    /// The world-space position where the cursor ray intersects the view-aligned plane passing through the reference point.
    fn world_position_on_view_plane(
        &self,
        transform: &GlobalTransform,
        cursor_pos: Vec2,
        reference_point: Vec3,
    ) -> Option<Vec3>;
}

impl CameraRaycast for Camera {
    fn get_cursor_world_position(
        &self,
        transform: &GlobalTransform,
        cursor_pos: Vec2,
        plane_origin: Vec3,
        plane_normal: Vec3,
    ) -> Option<Vec3> {
        let ray = self.viewport_to_world(transform, cursor_pos).ok()?;
        let plane = IntersectablePlane::from_normal(plane_origin, plane_normal)?;
        let intersection = plane.intersect_ray(&ray)?;
        Some(intersection.point)
    }

    fn get_cursor_world_position_fixed_distance(
        &self,
        transform: &GlobalTransform,
        cursor_pos: Vec2,
        distance: f32,
    ) -> Option<Vec3> {
        let ray = self.viewport_to_world(transform, cursor_pos).ok()?;
        Some(ray.get_point(distance))
    }

    fn world_position_on_view_plane(
        &self,
        transform: &GlobalTransform,
        cursor_pos: Vec2,
        reference_point: Vec3,
    ) -> Option<Vec3> {
        let ray = self.viewport_to_world(transform, cursor_pos).ok()?;
        // Use the camera's forward direction as the plane normal
        // The plane is perpendicular to the view direction, parallel to the view plane
        let plane = IntersectablePlane::new(reference_point, transform.forward());
        let intersection = plane.intersect_ray(&ray)?;
        Some(intersection.point)
    }
}
