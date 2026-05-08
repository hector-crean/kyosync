use super::{CameraControllerSystemSet, ControllerGate, InputLock};

use super::CameraController;
use bevy::{
    input::mouse::{MouseScrollUnit, MouseWheel},
    picking::pointer::{PointerAction, PointerInput},
    prelude::*,
};

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::ops::RangeInclusive;

/// Default half-span for pitch clamp (matches [`super::probe_controller::ProbeCameraController`]).
const ORBIT_PITCH_LIMIT: f32 = FRAC_PI_2 * 0.99;

#[inline]
fn shortest_angle_delta(from: f32, to: f32) -> f32 {
    (to - from + PI).rem_euclid(TAU) - PI
}

#[inline]
fn normalize_angle_pi(rad: f32) -> f32 {
    (rad + PI).rem_euclid(TAU) - PI
}

#[derive(Default)]
pub struct OrbitCameraControllerPlugin<T: InputLock>(pub T);

impl<T: InputLock + Send + Sync + 'static + Default> Plugin for OrbitCameraControllerPlugin<T> {
    fn build(&self, app: &mut App) {
        app.init_resource::<T>()
            .add_message::<CameraInputEvent>()
            .configure_sets(
                Update,
                (
                    CameraControllerSystemSet::InputHandling,
                    CameraControllerSystemSet::ProcessInput
                        .after(CameraControllerSystemSet::InputHandling),
                ),
            )
            .configure_sets(PostUpdate, (CameraControllerSystemSet::UpdateTransform,))
            .add_systems(
                Update,
                (Self::handle_pointer_input, Self::handle_scroll_input)
                    .chain()
                    .in_set(CameraControllerSystemSet::InputHandling),
            )
            .add_systems(
                Update,
                Self::process_camera_input.in_set(CameraControllerSystemSet::ProcessInput),
            )
            .add_systems(
                PostUpdate,
                Self::update_camera_transform
                    .in_set(CameraControllerSystemSet::UpdateTransform),
            );
    }
}

#[derive(Event, Message)]
pub enum CameraInputEvent {
    Rotate { delta: Vec2 },
    Pan { delta: Vec2 },
    Zoom { delta: f32 },
}

#[derive(Component, Clone)]
pub struct OrbitCameraController {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub center: Vec3,
    pub rotate_sensitivity: f32,
    pub pan_sensitivity: f32,
    pub zoom_sensitivity: f32,
    pub rotate_button: MouseButton,
    pub pan_button: MouseButton,
    /// Exponential smoothing toward input targets. `0.0` follows instantly; values closer to `1.0`
    /// lag more (same response curve as before when in `(0, 1)`).
    pub smoothing: f32,
    /// Allowed pitch range (radians) to keep the eye off the orbit poles and avoid `look_at`-style singularities.
    pub pitch_range: RangeInclusive<f32>,
    target_yaw: f32,
    target_pitch: f32,
    target_distance: f32,
    target_center: Vec3,
}

impl Default for OrbitCameraController {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: FRAC_PI_2 * 0.3,
            distance: 20.0,
            center: Vec3::ZERO,
            // Pointer deltas are in pixels per event; do not multiply by frame dt (see `apply_rotation` / `apply_pan`).
            // Defaults match the old `* delta_secs()` behavior at ~60 Hz for a single move per frame.
            rotate_sensitivity: 1.0 / 60.0,
            pan_sensitivity: 1.0 / 60.0,
            zoom_sensitivity: 1.0,
            rotate_button: MouseButton::Middle,
            pan_button: MouseButton::Right,
            smoothing: 0.9,
            pitch_range: -ORBIT_PITCH_LIMIT..=ORBIT_PITCH_LIMIT,
            target_yaw: 0.0,
            target_pitch: FRAC_PI_2 * 0.3,
            target_distance: 20.0,
            target_center: Vec3::ZERO,
        }
    }
}

impl OrbitCameraController {
    pub fn new(distance: f32, center: Vec3, initial_transform: Transform) -> Self {
        let to_center = (center - initial_transform.translation).normalize();

        let yaw = (-to_center.x).atan2(-to_center.z);
        let pitch = to_center
            .y
            .asin()
            .clamp(-ORBIT_PITCH_LIMIT, ORBIT_PITCH_LIMIT);

        Self {
            distance,
            center,
            yaw,
            pitch,
            target_yaw: yaw,
            target_pitch: pitch,
            target_distance: distance,
            target_center: center,
            ..Default::default()
        }
    }

    /// Build the camera transform from orbit parameters.
    ///
    /// Uses the rotation quaternion directly rather than [`Transform::looking_at`] to avoid
    /// unstable basis construction when the view direction aligns with world up (orbit poles).
    pub fn generate_transform(&self) -> Transform {
        let rotation = Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0);
        let offset = rotation * Vec3::new(0.0, 0.0, self.distance);
        let translation = self.center + offset;

        Transform::from_translation(translation).with_rotation(rotation)
    }

    fn apply_rotation(&mut self, delta: Vec2) {
        self.target_yaw -= delta.x * self.rotate_sensitivity;
        self.target_pitch -= delta.y * self.rotate_sensitivity;
        self.target_pitch = self
            .target_pitch
            .clamp(*self.pitch_range.start(), *self.pitch_range.end());
    }

    fn apply_pan(&mut self, delta: Vec2, transform: &Transform) {
        let right = transform.right();
        let up = transform.up();
        let pan_vector = (right * -delta.x + up * delta.y)
            * self.pan_sensitivity
            * (self.distance * 0.1);

        self.target_center += pan_vector;
    }

    fn apply_zoom(&mut self, delta: f32) {
        let zoom_factor = 1.0 + delta * self.zoom_sensitivity * 0.1;
        self.target_distance /= zoom_factor;
    }

    fn update_smooth(&mut self, time_delta: f32) {
        let lerp_factor = if self.smoothing <= 0.0 || self.smoothing >= 1.0 {
            1.0
        } else {
            1.0 - (1.0 - self.smoothing).powf(time_delta * 60.0)
        };

        let yaw_delta = shortest_angle_delta(self.yaw, self.target_yaw);
        self.yaw += yaw_delta * lerp_factor;
        self.yaw = normalize_angle_pi(self.yaw);

        self.pitch += (self.target_pitch - self.pitch) * lerp_factor;
        self.pitch = self
            .pitch
            .clamp(*self.pitch_range.start(), *self.pitch_range.end());
        self.distance += (self.target_distance - self.distance) * lerp_factor;
        self.center = self.center.lerp(self.target_center, lerp_factor);
    }
}

impl CameraController for OrbitCameraController {
    fn update_camera_transform_system(
        mut query: Query<
            (&Self, &mut Transform),
            (Or<(Changed<Self>, Added<Self>)>, With<Camera3d>),
        >,
    ) {
        for (controller, mut transform) in query.iter_mut() {
            *transform = controller.generate_transform();
        }
    }
}

impl<T: InputLock> OrbitCameraControllerPlugin<T> {
    fn handle_pointer_input(
        mut pointer_events: MessageReader<PointerInput>,
        mouse_input: Res<ButtonInput<MouseButton>>,
        keyboard_input: Res<ButtonInput<KeyCode>>,
        mut camera_events: MessageWriter<CameraInputEvent>,
        camera_query: Query<(&OrbitCameraController, Option<&ControllerGate>)>,
        settings: Res<T>,
    ) {
        if settings.is_locked() {
            return;
        }

        let Some((controller, gate)) = camera_query.iter().next() else {
            return;
        };
        if !gate.copied().unwrap_or_default().allows_input() {
            return;
        }

        let left_pressed = mouse_input.pressed(MouseButton::Left);
        let space_held =
            keyboard_input.pressed(KeyCode::Space);
        let alt_held =
            keyboard_input.pressed(KeyCode::AltLeft) || keyboard_input.pressed(KeyCode::AltRight);
        let super_held = keyboard_input.pressed(KeyCode::SuperLeft)
            || keyboard_input.pressed(KeyCode::SuperRight);

        for event in pointer_events.read() {
            if let PointerAction::Move { delta } = event.action {
                if space_held && left_pressed {
                    camera_events.write(CameraInputEvent::Pan { delta });
                } else if ((alt_held || super_held) && left_pressed)
                    || mouse_input.pressed(controller.rotate_button)
                {
                    camera_events.write(CameraInputEvent::Rotate { delta });
                } else if mouse_input.pressed(controller.pan_button) {
                    camera_events.write(CameraInputEvent::Pan { delta });
                }
            }
        }
    }

    fn handle_scroll_input(
        mut scroll_events: MessageReader<MouseWheel>,
        mut camera_events: MessageWriter<CameraInputEvent>,
        camera_query: Query<(&OrbitCameraController, Option<&ControllerGate>)>,
        settings: Res<T>,
    ) {
        if settings.is_locked() {
            return;
        }

        let Some((_controller, gate)) = camera_query.iter().next() else {
            return;
        };
        if !gate.copied().unwrap_or_default().allows_input() {
            return;
        }

        let mut total_delta = 0.0;
        for event in scroll_events.read() {
            total_delta += event.y
                * match event.unit {
                    MouseScrollUnit::Line => 1.0,
                    MouseScrollUnit::Pixel => 0.035,
                };
        }

        if total_delta != 0.0 {
            camera_events.write(CameraInputEvent::Zoom { delta: total_delta });
        }
    }

    fn process_camera_input(
        mut camera_events: MessageReader<CameraInputEvent>,
        mut camera_query: Query<(&mut OrbitCameraController, &Transform)>,
    ) {
        let Ok((mut controller, transform)) = camera_query.single_mut() else {
            return;
        };

        for event in camera_events.read() {
            match event {
                CameraInputEvent::Rotate { delta } => {
                    controller.apply_rotation(*delta);
                }
                CameraInputEvent::Pan { delta } => {
                    controller.apply_pan(*delta, transform);
                }
                CameraInputEvent::Zoom { delta } => {
                    controller.apply_zoom(*delta);
                }
            }
        }
    }

    fn update_camera_transform(
        mut camera_query: Query<(
            &mut OrbitCameraController,
            &mut Transform,
            Option<&ControllerGate>,
        ), With<Camera3d>>,
        settings: Res<T>,
        time: Res<Time>,
    ) {
        if settings.is_locked() {
            return;
        }

        for (mut controller, mut transform, gate) in camera_query.iter_mut() {
            if gate.copied().unwrap_or_default().allows_transform_update() {
                controller.update_smooth(time.delta_secs());
                *transform = controller.generate_transform();
            }
        }
    }
}
