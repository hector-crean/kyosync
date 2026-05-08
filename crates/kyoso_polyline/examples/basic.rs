//! Basic polyline example demonstrating the core functionality of bild_polyline.
//!
//! Run with: `cargo run --package bild_polyline --example basic`

use bevy::prelude::*;
use bild_polyline::prelude::*;

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
) {
    // Camera
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 2.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Simple zig-zag polyline
    let zigzag_vertices = vec![
        Vec3::new(-3.0, 0.0, 0.0),
        Vec3::new(-2.0, 1.0, 0.0),
        Vec3::new(-1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(2.0, 1.0, 0.0),
        Vec3::new(3.0, 0.0, 0.0),
    ];

    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: zigzag_vertices,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 8.0,
            color: Color::srgb(0.9, 0.3, 0.3).to_linear(),
            ..default()
        })),
        Transform::default(),
    ));

    // A square/rectangle outline
    let square_vertices = vec![
        Vec3::new(-1.0, -1.0, 2.0),
        Vec3::new(1.0, -1.0, 2.0),
        Vec3::new(1.0, 1.0, 2.0),
        Vec3::new(-1.0, 1.0, 2.0),
        Vec3::new(-1.0, -1.0, 2.0), // Close the shape
    ];

    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: square_vertices,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 4.0,
            color: Color::srgb(0.3, 0.9, 0.3).to_linear(),
            ..default()
        })),
        Transform::default(),
    ));

    // A 3D spiral going into the screen
    let spiral_vertices: Vec<Vec3> = (0..50)
        .map(|i| {
            let t = i as f32 * 0.2;
            Vec3::new(
                t.cos() * (1.0 + t * 0.1),
                t.sin() * (1.0 + t * 0.1),
                -t * 0.3,
            )
        })
        .collect();

    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: spiral_vertices,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 6.0,
            color: Color::srgb(0.3, 0.5, 0.9).to_linear(),
            ..default()
        })),
        Transform::from_xyz(0.0, -1.5, 0.0),
    ));

    // Add some lighting for visual reference
    commands.spawn((
        DirectionalLight {
            illuminance: 10000.0,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.5, 0.5, 0.0)),
    ));

    // Text instructions
    commands.spawn((
        Text::new("Basic Polyline Example\nRed: Zig-zag | Green: Square | Blue: 3D Spiral"),
        TextFont {
            font_size: FontSize::Px(20.0),
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
