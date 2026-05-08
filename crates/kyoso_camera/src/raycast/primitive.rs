//! Generic ray-primitive intersection traits.
//!
//! This module provides traits for performing ray-primitive intersection tests.
//! Primitives implement these traits to enable raycasting operations.

use bevy::prelude::*;
use bevy::math::bounding::{Aabb3d, BoundingSphere, RayCast3d};
use bevy::math::primitives::Plane3d;

/// Result of a ray intersection with additional metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayIntersection {
    /// Distance along the ray from origin to intersection point.
    pub distance: f32,
    /// World-space position of the intersection.
    pub point: Vec3,
    /// Surface normal at the intersection point (if applicable).
    pub normal: Option<Dir3>,
}

impl RayIntersection {
    /// Creates a simple intersection with just distance and point.
    pub fn new(distance: f32, point: Vec3) -> Self {
        Self {
            distance,
            point,
            normal: None,
        }
    }

    /// Creates an intersection with a surface normal.
    pub fn with_normal(distance: f32, point: Vec3, normal: Dir3) -> Self {
        Self {
            distance,
            point,
            normal: Some(normal),
        }
    }
}

/// Result of a ray intersection with a plane that includes UV coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneRayIntersection {
    /// Base intersection information.
    pub intersection: RayIntersection,
    /// UV coordinates on the plane (if applicable for bounded planes).
    pub uv: Option<Vec2>,
}

impl PlaneRayIntersection {
    /// Creates a plane intersection with UV coordinates.
    pub fn new(distance: f32, point: Vec3, normal: Dir3, uv: Option<Vec2>) -> Self {
        Self {
            intersection: RayIntersection::with_normal(distance, point, normal),
            uv,
        }
    }

    /// Creates an unbounded plane intersection (no UV coordinates).
    pub fn unbounded(distance: f32, point: Vec3, normal: Dir3) -> Self {
        Self {
            intersection: RayIntersection::with_normal(distance, point, normal),
            uv: None,
        }
    }
}

/// Trait for primitives that can be intersected by a ray.
///
/// This is the most basic intersection trait - it only returns whether
/// an intersection exists and provides the distance/point.
pub trait RayIntersect {
    /// Intersects a ray with this primitive.
    ///
    /// # Returns
    /// - `Some(intersection)` if the ray intersects the primitive
    /// - `None` if no intersection (ray is parallel, behind origin, etc.)
    fn intersect_ray(&self, ray: &Ray3d) -> Option<RayIntersection>;
}

/// Trait for planes that can be intersected by a ray with UV coordinate support.
///
/// This extends `RayIntersect` with plane-specific operations that can
/// compute UV coordinates and surface normals.
pub trait PlaneRayIntersect: RayIntersect {
    /// Intersects a ray with this plane, returning plane-specific information.
    ///
    /// # Returns
    /// - `Some(intersection)` if the ray intersects the plane
    /// - `None` if no intersection (ray is parallel, behind origin, etc.)
    fn intersect_ray_plane(&self, ray: &Ray3d) -> Option<PlaneRayIntersection>;
}

// ============================================================================
// Implementations for Bevy primitives
// ============================================================================

/// Wrapper for `InfinitePlane3d` to enable ray intersection operations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntersectablePlane {
    /// Origin point on the plane.
    pub origin: Vec3,
    /// Normal vector of the plane.
    pub normal: Dir3,
}

impl IntersectablePlane {
    /// Creates a new intersectable plane from an origin and normal.
    pub fn new(origin: Vec3, normal: Dir3) -> Self {
        Self { origin, normal }
    }

    /// Creates a new intersectable plane from an origin point and normal vector.
    pub fn from_normal(origin: Vec3, normal: Vec3) -> Option<Self> {
        Dir3::new(normal).ok().map(|normal| Self { origin, normal })
    }
}

impl RayIntersect for IntersectablePlane {
    fn intersect_ray(&self, ray: &Ray3d) -> Option<RayIntersection> {
        let plane = InfinitePlane3d::new(self.normal);
        let distance = ray.intersect_plane(self.origin, plane)?;
        let point = ray.get_point(distance);
        Some(RayIntersection::with_normal(distance, point, self.normal))
    }
}

impl PlaneRayIntersect for IntersectablePlane {
    fn intersect_ray_plane(&self, ray: &Ray3d) -> Option<PlaneRayIntersection> {
        let intersection = self.intersect_ray(ray)?;
        Some(PlaneRayIntersection {
            intersection,
            uv: None, // Infinite planes don't have UV coordinates
        })
    }
}

// ============================================================================
// Implementations for Bevy bounding volumes
// ============================================================================

impl RayIntersect for BoundingSphere {
    fn intersect_ray(&self, ray: &Ray3d) -> Option<RayIntersection> {
        // Use optimized RayCast3d
        let ray_cast = RayCast3d::from_ray(*ray, f32::INFINITY);
        let distance = ray_cast.sphere_intersection_at(self)?;
        let point = ray.get_point(distance);
        // Normal of a sphere at intersection is (point - center) normalized
        // BoundingSphere center is Vec3A
        let center: Vec3 = self.center.into();
        let normal = Dir3::new(point - center).ok();
        
        Some(RayIntersection {
            distance,
            point,
            normal,
        })
    }
}

impl RayIntersect for Aabb3d {
    fn intersect_ray(&self, ray: &Ray3d) -> Option<RayIntersection> {
        // Use optimized RayCast3d
        let ray_cast = RayCast3d::from_ray(*ray, f32::INFINITY);
        let distance = ray_cast.aabb_intersection_at(self)?;
        let point = ray.get_point(distance);
        // Computing normal for AABB is a bit more involved, optional for now
        Some(RayIntersection::new(distance, point))
    }
}

// ============================================================================
// Implementations for Bevy primitives with context
// ============================================================================

/// Tuple implementation for an infinite plane defined by an origin point and plane primitive.
impl RayIntersect for (Vec3, InfinitePlane3d) {
    fn intersect_ray(&self, ray: &Ray3d) -> Option<RayIntersection> {
        let (origin, plane) = self;
        let distance = ray.intersect_plane(*origin, *plane)?;
        let point = ray.get_point(distance);
        // plane.normal is Dir3, passed directly
        Some(RayIntersection::with_normal(distance, point, plane.normal))
    }
}

impl PlaneRayIntersect for (Vec3, InfinitePlane3d) {
    fn intersect_ray_plane(&self, ray: &Ray3d) -> Option<PlaneRayIntersection> {
        let intersection = self.intersect_ray(ray)?;
        Some(PlaneRayIntersection {
            intersection,
            uv: None,
        })
    }
}

/// Tuple implementation for a bounded plane positioned by a GlobalTransform.
/// The transform defines the plane's center and orientation.
impl RayIntersect for (GlobalTransform, Plane3d) {
    fn intersect_ray(&self, ray: &Ray3d) -> Option<RayIntersection> {
        let (transform, plane) = self;
        let affine = transform.affine();
        
        // Transform ray to local space
        let inv_affine = affine.inverse();
        let local_origin = inv_affine.transform_point3(ray.origin);
        let local_dir = inv_affine.transform_vector3(*ray.direction);
        let local_ray = Ray3d::new(local_origin, Dir3::new(local_dir).ok()?);
        
        // Intersect with plane in local space.
        // Bevy's Plane3d is defined by a normal and half_size.
        // However, it doesn't specify which axes correspond to width/height relative to the normal 
        // without a basis.
        // Usually, Plane3d implies a specific orientation in local space if it's a primitive.
        // If we assume the standard Bevy Mesh builder behavior for Plane3d:
        // - Default normal is +Y.
        // - Plane is in XZ plane.
        
        // We handle the case where normal is +Y (standard).
        // If normal is different, we'd need to construct a rotation to align it to Y to check bounds.
        
        // For intersection, we use the plane's normal.
        let infinite_plane = InfinitePlane3d::new(plane.normal);
        let distance = local_ray.intersect_plane(Vec3::ZERO, infinite_plane)?;
        let local_point = local_ray.get_point(distance);
        
        // Check bounds
        // We need to project local_point onto the plane's tangential axes.
        // If normal is Y, axes are X and Z.
        if plane.normal == Dir3::Y {
            if local_point.x.abs() <= plane.half_size.x && local_point.z.abs() <= plane.half_size.y {
                 // Hit!
                 let world_point = affine.transform_point3(local_point);
                 let distance = ray.origin.distance(world_point);
                 let world_normal = affine.transform_vector3(*plane.normal);
                 
                 return Some(RayIntersection::with_normal(
                     distance, 
                     world_point, 
                     Dir3::new(world_normal).ok()?
                 ));
            }
        } else {
            // General case: Construct a basis from the normal
            // This is ambiguous without an "up" vector, but AnyOrthonormalBasis can work for bounds check 
            // if we assume Plane3d is just a rectangle centered at origin oriented by normal.
            // But orientation around normal matters for a rectangle (width vs height).
            // Bevy's Plane3d struct is slightly underspecified for arbitrary orientation without a Quat.
            // But usually primitives are axis aligned in their definition space.
            // If Plane3d has a non-Y normal, it's a rotated plane *definition*.
            
            // For now, we only support standard Y-normal Plane3d which is 99% of use cases.
            return None;
        }
        
        None
    }
}

