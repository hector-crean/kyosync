use bevy::prelude::*;
use bitflags::bitflags;
use std::f32::INFINITY;

use kyoso_camera::CameraRaycast;
use kyoso_camera::CameraSettings;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct TransformMode: u32 {
        const Translate = 1;
        const Rotate = 1 << 1;
        const Scale = 1 << 2;
    }
}

#[derive(Resource)]
pub struct DragController3dSettings {
    pub enabled: bool,
    pub grid_snapping: Option<Vec3>,
    pub translation_constraints: Option<BVec3>,
    pub rotation_constraints: Option<Vec3>, // Constrain rotations to certain axes
    pub scale_constraints: Option<Vec3>,    // Constrain scaling behavior
    /// World-space axis-aligned box; translation drags are clamped inside it (`None` = no global limit).
    ///
    /// Combined with per-entity [`TransformBounds`] on the dragged entity (intersection of both).
    pub translation_volume: Option<TransformBounds>,
}
impl Default for DragController3dSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            grid_snapping: Some(Vec3 {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            }),
            translation_constraints: Some(BVec3 {
                x: true,
                y: true,
                z: true,
            }),
            rotation_constraints: None,
            scale_constraints: None,
            translation_volume: None,
        }
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct TransformBounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for TransformBounds {
    fn default() -> Self {
        Self {
            min: Vec3::new(-INFINITY, -INFINITY, -INFINITY),
            max: Vec3::new(INFINITY, INFINITY, INFINITY),
        }
    }
}

impl TransformBounds {
    pub fn contains(&self, point: Vec3) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
            && point.z >= self.min.z
            && point.z <= self.max.z
    }

    /// Clamps `v` to lie inside this box (per axis).
    pub fn clamp_point(&self, v: Vec3) -> Vec3 {
        v.clamp(self.min, self.max)
    }
}

#[derive(Component)]
#[require(Transform, TransformBounds)]
pub struct DragController3d {
    pub enabled: bool,
    pub drag_start_pointer_position: Option<Vec3>,
    pub drag_start_entity_position: Option<Vec3>,
    pub mode: TransformMode,
}
impl Default for DragController3d {
    fn default() -> Self {
        Self {
            enabled: true,
            drag_start_pointer_position: None,
            drag_start_entity_position: None,
            mode: TransformMode::Translate,
        }
    }
}


/// World drag target for [`DragTransform3dPlugin`]. Without [`DragController3d`], drags are
/// translate-only (enabled, default mode). With a controller, `enabled` and `mode` apply.
#[derive(Component, Default)]
pub struct Draggable3d {
    dragging: bool,
    drag_start_entity_position: Option<Vec3>,
    drag_start_pointer_position: Option<Vec3>,
    /// Snapshot at drag start for rotate / scale modes (uses cumulative screen drag distance).
    drag_start_rotation: Option<Quat>,
    drag_start_scale: Option<Vec3>,
}
impl Draggable3d {
    pub fn new() -> Self {
        Self {
            dragging: false,
            drag_start_entity_position: None,
            drag_start_pointer_position: None,
            drag_start_rotation: None,
            drag_start_scale: None,
        }
    }
    fn cursor_offset(&self) -> Vec3 {
        self.drag_start_entity_position.unwrap() - self.drag_start_pointer_position.unwrap()
    }
    // Optionally, a method to do a standard "drag translation":
    pub fn compute_drag_translation(
        &self,
        cursor_pos: Vec2,
        camera: &Camera,
        camera_transform: &GlobalTransform,
    ) -> Option<Vec3> {
        let Some(entity_pos) = self.drag_start_entity_position else {
            return None;
        };
        let Some(_start_pointer_pos) = self.drag_start_pointer_position else {
            return None;
        };

        // The "plane intersection" at the drag-start pointer pos vs. new pointer pos
        let new_pos = camera
            .world_position_on_view_plane(camera_transform, cursor_pos, entity_pos)?
            + self.cursor_offset();

        Some(new_pos)
    }
}

/// Radians per pixel of screen drag distance for rotation drags.
const DRAG_ROTATE_SENSITIVITY: f32 = 0.008;
/// Scale change per pixel of vertical drag (positive Y drag down increases scale shrink factor).
const DRAG_SCALE_SENSITIVITY: f32 = 0.01;

#[derive(Default)]
pub struct DragTransform3dPlugin<T: CameraSettings>(pub T);

impl<T: CameraSettings + Send + Sync + 'static> Plugin for DragTransform3dPlugin<T> {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragController3dSettings>()
            .add_systems(
                Update,
                Self::pointer_drag_3d_system.run_if(run_criteria::<T>),
            );
    }
}

fn run_criteria<T: CameraSettings>(_mode: Res<T>) -> bool {
    // !(*mode).is_locked()
    true
}

impl<T: CameraSettings + Send + Sync + 'static> DragTransform3dPlugin<T> {
    /// Handles [`Pointer<DragStart>`], [`Pointer<Drag>`], then [`Pointer<DragEnd>`] in order so
    /// start state is applied before move in the same frame.
    ///
    /// [`Draggable3d`] alone implies translate-only drag (default enabled). Add [`DragController3d`]
    /// for rotate/scale modes and `enabled` / `mode`.
    fn pointer_drag_3d_system(
        mut drag_starts: MessageReader<Pointer<DragStart>>,
        mut drags: MessageReader<Pointer<Drag>>,
        mut drag_ends: MessageReader<Pointer<DragEnd>>,
        mut query: Query<(
            Entity,
            &mut Draggable3d,
            &mut Transform,
            Option<&DragController3d>,
            Option<&TransformBounds>,
        )>,
        camera_query: Query<(&GlobalTransform, &Camera)>,
        drag_settings: Res<DragController3dSettings>,
        mut camera_controller: ResMut<T>,
    ) {
        for drag_start in drag_starts.read() {
            let Ok((_entity, mut draggable, transform, _, _)) = query.get_mut(drag_start.entity) else {
                continue;
            };

            draggable.drag_start_entity_position = Some(transform.translation);
            draggable.drag_start_pointer_position = drag_start.hit.position;
            draggable.drag_start_rotation = Some(transform.rotation);
            draggable.drag_start_scale = Some(transform.scale);

            camera_controller.lock();
        }

        let camera_ok = camera_query.single().ok();

        for drag in drags.read() {
            let Some((camera_transform, camera)) = camera_ok else {
                continue;
            };

            let Ok((_entity, draggable, mut transform, controller, entity_bounds)) =
                query.get_mut(drag.entity)
            else {
                continue;
            };

            let enabled = drag_settings.enabled && controller.map(|c| c.enabled).unwrap_or(true);
            if !enabled {
                continue;
            }

            let mode = controller
                .map(|c| c.mode)
                .unwrap_or(TransformMode::Translate);
            if mode == TransformMode::Rotate {
                if let Some(start_rot) = draggable.drag_start_rotation {
                    let d = drag.distance;
                    let yaw = Quat::from_axis_angle(Vec3::Y, -d.x * DRAG_ROTATE_SENSITIVITY);
                    let right = camera_transform.right().as_vec3();
                    let pitch = Quat::from_axis_angle(right, -d.y * DRAG_ROTATE_SENSITIVITY);
                    transform.rotation = (pitch * yaw * start_rot).normalize();
                }
                continue;
            }
            if mode == TransformMode::Scale {
                if let Some(start_scale) = draggable.drag_start_scale {
                    let d = drag.distance;
                    let factor = (1.0 + (-d.y) * DRAG_SCALE_SENSITIVITY).clamp(0.05, 50.0);
                    transform.scale = start_scale * factor;
                }
                continue;
            }

            if let Some(new_translation) = draggable.compute_drag_translation(
                drag.pointer_location.position,
                camera,
                camera_transform,
            ) {
                let mut t = new_translation;
                if let Some(vol) = drag_settings.translation_volume.as_ref() {
                    t = vol.clamp_point(t);
                }
                if let Some(bounds) = entity_bounds {
                    t = bounds.clamp_point(t);
                }
                transform.translation = t;
            }
        }

        for drag_end in drag_ends.read() {
            let Ok((_entity, mut draggable, _transform, _, _)) = query.get_mut(drag_end.entity) else {
                continue;
            };

            draggable.dragging = false;
            draggable.drag_start_rotation = None;
            draggable.drag_start_scale = None;

            camera_controller.unlock();
        }
    }
}
