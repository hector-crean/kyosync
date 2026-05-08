//! Camera controller implementations.

pub mod pan_orbit_camera;


pub use pan_orbit_camera::{CameraInputEvent, OrbitCameraController, OrbitCameraControllerPlugin};

use bevy::prelude::*;

/// System sets for camera controller ordering.
/// Shared across all camera controller plugins.
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub enum CameraControllerSystemSet {
    /// Input handling (reads pointer/scroll events)
    InputHandling,
    /// Process camera input events
    ProcessInput,
    /// Update camera transform (runs last, after all interactions)
    UpdateTransform,
}

pub trait CameraController: Component
where
    Self: Sized,
{
    fn update_camera_transform_system(
        query: Query<(&Self, &mut Transform), (Or<(Changed<Self>, Added<Self>)>, With<Camera3d>)>,
    );
}

/// Global resource gating input handlers based on external pointer ownership.
///
/// When locked, all camera controller input handlers (mouse, keyboard, scroll)
/// are suppressed. The drag systems, select tool, and markup tools call
/// `lock()` / `unlock()` during pointer-claiming operations.
pub trait InputLock: Resource + Clone + Send + Sync + 'static {
    fn is_locked(&self) -> bool;
    fn lock(&mut self);
    fn unlock(&mut self);
}

/// Backward-compatible alias for the renamed trait.
pub use InputLock as CameraSettings;

/// Default implementation for when no specific settings are needed.
#[derive(Resource, Clone, Default)]
pub struct DefaultCameraSettings {
    pub locked: bool,
}

impl InputLock for DefaultCameraSettings {
    fn is_locked(&self) -> bool {
        self.locked
    }
    fn lock(&mut self) {
        self.locked = true;
    }
    fn unlock(&mut self) {
        self.locked = false;
    }
}

/// Per-entity gate controlling whether a camera controller's input handlers
/// and transform update systems run.
///
/// Replaces the old `controller.enabled` field and `ActiveCameraController`
/// resource with a single, per-entity component.
#[derive(Component, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControllerGate {
    /// Input handlers AND transform updates both run.
    #[default]
    Active,
    /// Built-in input handlers are blocked, but transform updates still run.
    /// Useful when app code writes events directly (e.g. click-to-align).
    InputOnly,
    /// Fully frozen: no input, no transform updates.
    Frozen,
}

impl ControllerGate {
    /// Returns `true` when built-in input handlers should process events.
    pub fn allows_input(&self) -> bool {
        *self == Self::Active
    }

    /// Returns `true` when the transform update system should run.
    pub fn allows_transform_update(&self) -> bool {
        *self != Self::Frozen
    }
}
