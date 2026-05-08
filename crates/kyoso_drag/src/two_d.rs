use bevy::prelude::*;

use kyoso_camera::CameraSettings;

/// Marker for entities that can be dragged in 2D.
#[derive(Component, Default)]
pub struct Draggable2d {
    dragging: bool,
    drag_start_entity_translation: Option<Vec3>,
    drag_start_pointer_world: Option<Vec2>,
}

impl Draggable2d {
    #[inline]
    fn offset(&self) -> Vec2 {
        let start_entity = self.drag_start_entity_translation.unwrap();
        let start_pointer = self.drag_start_pointer_world.unwrap();
        start_entity.truncate() - start_pointer
    }
}

/// Optional axis constraints for 2D dragging.
#[derive(Resource, Clone, Copy)]
pub struct Drag2dSettings {
    pub enabled: bool,
    pub grid_snapping: Option<Vec2>,
    pub axis_constraints: Option<BVec2>,
}

impl Default for Drag2dSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            grid_snapping: None,
            axis_constraints: None,
        }
    }
}

#[derive(Default)]
pub struct DragTransform2dPlugin<T: CameraSettings>(pub T);

impl<T: CameraSettings + Send + Sync + 'static> Plugin for DragTransform2dPlugin<T> {
    fn build(&self, app: &mut App) {
        app.init_resource::<Drag2dSettings>()
            .add_systems(
                Update,
                Self::pointer_drag_2d_system.run_if(run_criteria::<T>),
            );
    }
}

fn run_criteria<T: CameraSettings>(mode: Res<T>) -> bool {
    // !mode.is_locked()
    let _ = mode; // keep consistent with 3D variant; could gate on lock later
    true
}

impl<T: CameraSettings + Send + Sync + 'static> DragTransform2dPlugin<T> {
    fn pointer_drag_2d_system(
        mut drag_starts: MessageReader<Pointer<DragStart>>,
        mut drags: MessageReader<Pointer<Drag>>,
        mut drag_ends: MessageReader<Pointer<DragEnd>>,
        mut query: Query<(Entity, &mut Draggable2d, &mut Transform)>,
        cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
        settings: Res<Drag2dSettings>,
        mut camera_settings: ResMut<T>,
    ) {
        let camera_ok = cameras.single().ok();

        for drag_start in drag_starts.read() {
            let Some((camera, camera_transform)) = camera_ok else {
                continue;
            };

            let Ok((_entity, mut draggable, transform)) = query.get_mut(drag_start.entity) else {
                continue;
            };

            let screen_pos = drag_start.pointer_location.position;

            let Ok(world_pos_2d) = camera.viewport_to_world_2d(camera_transform, screen_pos) else {
                continue;
            };

            draggable.drag_start_entity_translation = Some(transform.translation);
            draggable.drag_start_pointer_world = Some(world_pos_2d);
            draggable.dragging = true;

            camera_settings.lock();
        }

        for drag in drags.read() {
            let Some((camera, camera_transform)) = camera_ok else {
                continue;
            };

            let Ok((_entity, draggable, mut transform)) = query.get_mut(drag.entity) else {
                continue;
            };

            let screen_pos = drag.pointer_location.position;

            let Ok(ptr_world_2d) = camera.viewport_to_world_2d(camera_transform, screen_pos) else {
                continue;
            };

            let mut new_xy = ptr_world_2d + draggable.offset();

            if let Some(grid) = settings.grid_snapping {
                if grid.x > 0.0 {
                    new_xy.x = (new_xy.x / grid.x).round() * grid.x;
                }
                if grid.y > 0.0 {
                    new_xy.y = (new_xy.y / grid.y).round() * grid.y;
                }
            }

            if let Some(axis) = settings.axis_constraints {
                if !axis.x {
                    new_xy.x = transform.translation.x;
                }
                if !axis.y {
                    new_xy.y = transform.translation.y;
                }
            }

            transform.translation.x = new_xy.x;
            transform.translation.y = new_xy.y;
        }

        for drag_end in drag_ends.read() {
            let Ok((_entity, mut draggable, _transform)) = query.get_mut(drag_end.entity) else {
                continue;
            };

            draggable.dragging = false;
            camera_settings.unlock();
        }
    }
}
