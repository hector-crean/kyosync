//! Perspective vs Non-Perspective polyline comparison.
//!
//! Demonstrates how the `perspective` flag affects line rendering:
//! - Non-perspective: Lines maintain constant screen-space width
//! - Perspective: Lines shrink with distance, simulating real 3D tubes
//!
//! Run with: `cargo run --package kyoso_polyline --example perspective`

use bevy::prelude::*;
use kyoso_polyline::prelude::*;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, PolylinePlugin))
        .add_systems(Startup, setup)
        .add_systems(Update, rotate_camera)
        .run();
}

#[derive(Component)]
struct CameraController {
    angle: f32,
    radius: f32,
    height: f32,
}

fn setup(
    mut commands: Commands,
    mut polylines: ResMut<Assets<Polyline>>,
    mut materials: ResMut<Assets<PolylineMaterial>>,
) {
    // Orbiting camera
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 5.0, 15.0).looking_at(Vec3::ZERO, Vec3::Y),
        CameraController {
            angle: 0.0,
            radius: 15.0,
            height: 5.0,
        },
    ));

    // Create a line going into the distance (Z axis)
    let depth_line: Vec<Vec3> = (0..30)
        .map(|i| Vec3::new(0.0, 0.0, -i as f32 * 2.0))
        .collect();

    // Non-perspective line (left side) - constant screen width
    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: depth_line.clone(),
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 10.0,
            color: Color::srgb(1.0, 0.3, 0.3).to_linear(),
            perspective: false,
            ..default()
        })),
        Transform::from_xyz(-3.0, 0.0, 0.0),
    ));

    // Perspective line (right side) - shrinks with distance
    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: depth_line,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 10.0,
            color: Color::srgb(0.3, 0.8, 1.0).to_linear(),
            perspective: true,
            ..default()
        })),
        Transform::from_xyz(3.0, 0.0, 0.0),
    ));

    // Grid of parallel lines at different depths - non-perspective
    for z in 0..5 {
        let y = -2.0;
        commands.spawn((
            PolylineHandle(polylines.add(Polyline {
                vertices: vec![
                    Vec3::new(-8.0, y, -z as f32 * 10.0),
                    Vec3::new(-4.0, y, -z as f32 * 10.0),
                ],
            })),
            PolylineMaterialHandle(materials.add(PolylineMaterial {
                width: 8.0,
                color: Color::srgb(1.0, 0.7, 0.2).to_linear(),
                perspective: false,
                ..default()
            })),
            Transform::default(),
        ));
    }

    // Grid of parallel lines at different depths - perspective
    for z in 0..5 {
        let y = -2.0;
        commands.spawn((
            PolylineHandle(polylines.add(Polyline {
                vertices: vec![
                    Vec3::new(4.0, y, -z as f32 * 10.0),
                    Vec3::new(8.0, y, -z as f32 * 10.0),
                ],
            })),
            PolylineMaterialHandle(materials.add(PolylineMaterial {
                width: 8.0,
                color: Color::srgb(0.5, 1.0, 0.5).to_linear(),
                perspective: true,
                ..default()
            })),
            Transform::default(),
        ));
    }

    // Spiral comparison
    let spiral: Vec<Vec3> = (0..100)
        .map(|i| {
            let t = i as f32 * 0.15;
            Vec3::new(t.cos() * 2.0, t.sin() * 2.0, -t)
        })
        .collect();

    // Non-perspective spiral
    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: spiral.clone(),
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 6.0,
            color: Color::srgb(0.9, 0.4, 0.9).to_linear(),
            perspective: false,
            ..default()
        })),
        Transform::from_xyz(-6.0, 3.0, 0.0),
    ));

    // Perspective spiral
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: spiral })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 6.0,
            color: Color::srgb(0.4, 0.9, 0.9).to_linear(),
            perspective: true,
            ..default()
        })),
        Transform::from_xyz(6.0, 3.0, 0.0),
    ));

    // Lighting
    commands.spawn((
        DirectionalLight {
            illuminance: 10000.0,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.5, 0.5, 0.0)),
    ));

    // Instructions
    commands.spawn((
        Text::new(
            "Perspective Comparison\n\
             RED/ORANGE/MAGENTA (Left): Non-perspective - constant screen width\n\
             CYAN/GREEN (Right): Perspective - shrinks with distance\n\
             Camera orbits automatically",
        ),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..default()
        },
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(10.0),
            ..default()
        },
    ));
}

fn rotate_camera(time: Res<Time>, mut query: Query<(&mut Transform, &mut CameraController)>) {
    for (mut transform, mut controller) in &mut query {
        controller.angle += time.delta_secs() * 0.2;
        let x = controller.angle.cos() * controller.radius;
        let z = controller.angle.sin() * controller.radius;
        transform.translation = Vec3::new(x, controller.height, z);
        transform.look_at(Vec3::new(0.0, 0.0, -15.0), Vec3::Y);
    }
}
