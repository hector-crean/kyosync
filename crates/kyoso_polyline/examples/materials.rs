//! Materials showcase demonstrating different polyline material properties.
//!
//! Shows various widths, colors, transparency, and depth bias settings.
//!
//! Run with: `cargo run --package kyoso_polyline --example materials`

use bevy::prelude::*;
use kyoso_polyline::prelude::*;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, PolylinePlugin))
        .add_systems(Startup, setup)
        .run();
}

fn setup(
    mut commands: Commands,
    mut polylines: ResMut<Assets<Polyline>>,
    mut materials: ResMut<Assets<PolylineMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
) {
    // Camera
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 3.0, 12.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Create a simple horizontal line template
    let line_template =
        |y: f32| -> Vec<Vec3> { vec![Vec3::new(-4.0, y, 0.0), Vec3::new(4.0, y, 0.0)] };

    // --- Width Comparison ---
    let widths = [1.0, 2.0, 4.0, 8.0, 16.0];
    for (i, &width) in widths.iter().enumerate() {
        let y = 3.0 - i as f32 * 0.8;
        commands.spawn((
            PolylineHandle(polylines.add(Polyline {
                vertices: line_template(y),
            })),
            PolylineMaterialHandle(materials.add(PolylineMaterial {
                width,
                color: Color::srgb(0.9, 0.6, 0.2).to_linear(),
                ..default()
            })),
            Transform::from_xyz(-5.0, 0.0, 0.0),
        ));
    }

    // --- Color Gradient ---
    let colors = [
        Color::srgb(1.0, 0.2, 0.2), // Red
        Color::srgb(1.0, 0.6, 0.2), // Orange
        Color::srgb(1.0, 1.0, 0.2), // Yellow
        Color::srgb(0.2, 1.0, 0.2), // Green
        Color::srgb(0.2, 0.6, 1.0), // Blue
        Color::srgb(0.6, 0.2, 1.0), // Purple
    ];
    for (i, color) in colors.iter().enumerate() {
        let y = 3.0 - i as f32 * 0.8;
        commands.spawn((
            PolylineHandle(polylines.add(Polyline {
                vertices: line_template(y),
            })),
            PolylineMaterialHandle(materials.add(PolylineMaterial {
                width: 6.0,
                color: color.to_linear(),
                ..default()
            })),
            Transform::from_xyz(5.0, 0.0, 0.0),
        ));
    }

    // --- Transparency Demo ---
    let alphas = [1.0, 0.8, 0.6, 0.4, 0.2];
    for (i, &alpha) in alphas.iter().enumerate() {
        let y = -2.0 - i as f32 * 0.6;
        commands.spawn((
            PolylineHandle(polylines.add(Polyline {
                vertices: line_template(y),
            })),
            PolylineMaterialHandle(materials.add(PolylineMaterial {
                width: 10.0,
                color: LinearRgba::new(0.2, 0.8, 0.9, alpha),
                ..default()
            })),
            Transform::from_xyz(0.0, 0.0, 0.0),
        ));
    }

    // --- Depth Bias Demo ---
    // Create a plane to demonstrate depth bias
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::splat(2.0)))),
        MeshMaterial3d(std_materials.add(StandardMaterial {
            base_color: Color::srgb(0.5, 0.5, 0.5),
            ..default()
        })),
        Transform::from_xyz(0.0, 1.0, 3.0),
    ));

    // Lines with different depth biases on the plane
    let depth_biases = [0.0, -0.1, -0.3, -0.5];
    for (i, &bias) in depth_biases.iter().enumerate() {
        let y = 2.0 - i as f32 * 0.6;
        commands.spawn((
            PolylineHandle(polylines.add(Polyline {
                vertices: vec![Vec3::new(-1.5, y, 3.0), Vec3::new(1.5, y, 3.0)],
            })),
            PolylineMaterialHandle(materials.add(PolylineMaterial {
                width: 4.0,
                color: Color::srgb(1.0, 0.3, 0.5).to_linear(),
                depth_bias: bias,
                ..default()
            })),
            Transform::default(),
        ));
    }

    // Lighting
    commands.spawn((
        DirectionalLight {
            illuminance: 10000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.5, 0.5, 0.0)),
    ));

    // Instructions
    commands.spawn((
        Text::new(
            "Materials Showcase\n\
             Left: Width (1-16px) | Right: Colors\n\
             Center Bottom: Transparency | Center Top: Depth Bias on plane",
        ),
        TextFont {
            font_size: FontSize::Px(18.0),
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
