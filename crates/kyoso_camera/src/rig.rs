use bevy::camera::Camera;
use bevy::prelude::*;

pub trait CameraRig: Resource {
    fn add_camera(&mut self, camera: Camera);
    fn remove_camera(&mut self, camera: &Camera);
    fn update(&mut self, cameras: &mut Query<&mut Camera>);
    //add/remove controller marker components? We want there to be only one controller component per camera bundle?
}

/// Default rig stub. The `cameras` field is reserved for the upcoming
/// rig methods (`add_camera`/`remove_camera`/`update` from
/// [`CameraRig`]); until those are implemented, the field is unread.
pub struct DefaultCameraRig {
    #[allow(dead_code)]
    cameras: Vec<Entity>,
}

impl Default for DefaultCameraRig {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultCameraRig {
    pub fn new() -> Self {
        Self {
            cameras: Vec::new(),
        }
    }
}
