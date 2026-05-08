//! Geometric shapes drawn with polylines.
//!
//! Demonstrates creating various geometric primitives using polylines.
//!
//! Run with: `cargo run --package bild_polyline --example shapes`

use bevy::prelude::*;
use bild_polyline::prelude::*;
use std::f32::consts::{PI, TAU};

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, PolylinePlugin))
        .add_systems(Startup, setup)
        .add_systems(Update, rotate_shapes)
        .run();
}

#[derive(Component)]
struct RotatingShape;

fn setup(
    mut commands: Commands,
    mut polylines: ResMut<Assets<Polyline>>,
    mut materials: ResMut<Assets<PolylineMaterial>>,
) {
    // Camera
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // === 2D Shapes ===

    // Circle
    let circle = create_circle(1.5, 64);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: circle })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 4.0,
            color: Color::srgb(1.0, 0.4, 0.4).to_linear(),
            ..default()
        })),
        Transform::from_xyz(-6.0, 3.0, 0.0),
    ));

    // Pentagon
    let pentagon = create_regular_polygon(1.5, 5);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: pentagon })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 4.0,
            color: Color::srgb(1.0, 0.7, 0.2).to_linear(),
            ..default()
        })),
        Transform::from_xyz(-2.0, 3.0, 0.0),
    ));

    // Hexagon
    let hexagon = create_regular_polygon(1.5, 6);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: hexagon })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 4.0,
            color: Color::srgb(0.4, 1.0, 0.4).to_linear(),
            ..default()
        })),
        Transform::from_xyz(2.0, 3.0, 0.0),
    ));

    // Star
    let star = create_star(1.5, 0.6, 5);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: star })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 4.0,
            color: Color::srgb(0.4, 0.7, 1.0).to_linear(),
            ..default()
        })),
        Transform::from_xyz(6.0, 3.0, 0.0),
    ));

    // === 3D Shapes (Wireframes) ===

    // Cube wireframe
    let cube = create_cube_wireframe(2.0);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: cube })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 3.0,
            color: Color::srgb(0.9, 0.5, 0.9).to_linear(),
            ..default()
        })),
        Transform::from_xyz(-6.0, -2.0, 0.0),
        RotatingShape,
    ));

    // Tetrahedron wireframe
    let tetrahedron = create_tetrahedron_wireframe(2.0);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: tetrahedron,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 3.0,
            color: Color::srgb(0.5, 0.9, 0.9).to_linear(),
            ..default()
        })),
        Transform::from_xyz(-2.0, -2.0, 0.0),
        RotatingShape,
    ));

    // Octahedron wireframe
    let octahedron = create_octahedron_wireframe(1.5);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: octahedron,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 3.0,
            color: Color::srgb(1.0, 0.9, 0.4).to_linear(),
            ..default()
        })),
        Transform::from_xyz(2.0, -2.0, 0.0),
        RotatingShape,
    ));

    // Torus knot
    let torus_knot = create_torus_knot(1.0, 0.3, 3, 2, 200);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: torus_knot,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 3.0,
            color: Color::srgb(1.0, 0.4, 0.6).to_linear(),
            perspective: true,
            ..default()
        })),
        Transform::from_xyz(6.0, -2.0, 0.0),
        RotatingShape,
    ));

    // === Special Curves ===

    // Bezier curve
    let bezier = create_bezier_curve(
        Vec3::new(-3.0, 0.0, 0.0),
        Vec3::new(-1.0, 2.0, 0.0),
        Vec3::new(1.0, -2.0, 0.0),
        Vec3::new(3.0, 0.0, 0.0),
        50,
    );
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: bezier })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 5.0,
            color: Color::srgb(0.6, 0.9, 0.6).to_linear(),
            ..default()
        })),
        Transform::from_xyz(0.0, -6.0, 0.0),
    ));

    // Helix
    let helix = create_helix(1.0, 0.3, 4.0, 100);
    commands.spawn((
        PolylineHandle(polylines.add(Polyline { vertices: helix })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 3.0,
            color: Color::srgb(0.7, 0.5, 1.0).to_linear(),
            perspective: true,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -5.0),
        RotatingShape,
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
            "Geometric Shapes\n\
             Top: Circle, Pentagon, Hexagon, Star\n\
             Middle: Cube, Tetrahedron, Octahedron, Torus Knot\n\
             Bottom: Bezier Curve | Back: Helix",
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

fn rotate_shapes(time: Res<Time>, mut query: Query<&mut Transform, With<RotatingShape>>) {
    for mut transform in &mut query {
        transform.rotate_y(time.delta_secs() * 0.5);
        transform.rotate_x(time.delta_secs() * 0.3);
    }
}

// === Shape Generation Functions ===

fn create_circle(radius: f32, segments: usize) -> Vec<Vec3> {
    let mut vertices = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let angle = TAU * (i as f32 / segments as f32);
        vertices.push(Vec3::new(angle.cos() * radius, angle.sin() * radius, 0.0));
    }
    vertices
}

fn create_regular_polygon(radius: f32, sides: usize) -> Vec<Vec3> {
    let mut vertices = Vec::with_capacity(sides + 1);
    for i in 0..=sides {
        let angle = TAU * (i as f32 / sides as f32) - PI / 2.0;
        vertices.push(Vec3::new(angle.cos() * radius, angle.sin() * radius, 0.0));
    }
    vertices
}

fn create_star(outer_radius: f32, inner_radius: f32, points: usize) -> Vec<Vec3> {
    let mut vertices = Vec::with_capacity(points * 2 + 1);
    for i in 0..=points * 2 {
        let angle = TAU * (i as f32 / (points * 2) as f32) - PI / 2.0;
        let radius = if i % 2 == 0 {
            outer_radius
        } else {
            inner_radius
        };
        vertices.push(Vec3::new(angle.cos() * radius, angle.sin() * radius, 0.0));
    }
    vertices
}

fn create_cube_wireframe(size: f32) -> Vec<Vec3> {
    let h = size / 2.0;
    // Draw cube edges as a continuous path (some edges drawn twice)
    vec![
        // Bottom face
        Vec3::new(-h, -h, -h),
        Vec3::new(h, -h, -h),
        Vec3::new(h, -h, h),
        Vec3::new(-h, -h, h),
        Vec3::new(-h, -h, -h),
        // Up to top face
        Vec3::new(-h, h, -h),
        // Top face
        Vec3::new(h, h, -h),
        Vec3::new(h, h, h),
        Vec3::new(-h, h, h),
        Vec3::new(-h, h, -h),
        // Vertical edges
        Vec3::new(-h, h, h),
        Vec3::new(-h, -h, h),
        Vec3::new(h, -h, h),
        Vec3::new(h, h, h),
        Vec3::new(h, h, -h),
        Vec3::new(h, -h, -h),
    ]
}

fn create_tetrahedron_wireframe(size: f32) -> Vec<Vec3> {
    let h = size / 2.0;
    let sqrt2 = 2.0_f32.sqrt();
    let sqrt6 = 6.0_f32.sqrt();

    // Tetrahedron vertices
    let v0 = Vec3::new(0.0, h, 0.0);
    let v1 = Vec3::new(-h, -h / 3.0, h * sqrt2 / sqrt6 * 2.0);
    let v2 = Vec3::new(h, -h / 3.0, h * sqrt2 / sqrt6 * 2.0);
    let v3 = Vec3::new(0.0, -h / 3.0, -h * sqrt2 / sqrt6 * 2.0 * 2.0);

    vec![v0, v1, v2, v0, v3, v1, v2, v3]
}

fn create_octahedron_wireframe(size: f32) -> Vec<Vec3> {
    let top = Vec3::new(0.0, size, 0.0);
    let bottom = Vec3::new(0.0, -size, 0.0);
    let front = Vec3::new(0.0, 0.0, size);
    let back = Vec3::new(0.0, 0.0, -size);
    let left = Vec3::new(-size, 0.0, 0.0);
    let right = Vec3::new(size, 0.0, 0.0);

    vec![
        // Top pyramid
        top, front, right, top, back, right, top, back, left, top, front, left, front,
        // Bottom connections
        bottom, front, bottom, right, bottom, back, bottom, left,
    ]
}

fn create_torus_knot(radius: f32, tube_radius: f32, p: u32, q: u32, segments: usize) -> Vec<Vec3> {
    (0..=segments)
        .map(|i| {
            let t = TAU * (i as f32 / segments as f32);
            let pt = p as f32 * t;
            let qt = q as f32 * t;

            let r = radius + tube_radius * qt.cos();
            Vec3::new(r * pt.cos(), tube_radius * qt.sin(), r * pt.sin())
        })
        .collect()
}

fn create_bezier_curve(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, segments: usize) -> Vec<Vec3> {
    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let u = 1.0 - t;
            let tt = t * t;
            let uu = u * u;
            let uuu = uu * u;
            let ttt = tt * t;

            p0 * uuu + p1 * (3.0 * uu * t) + p2 * (3.0 * u * tt) + p3 * ttt
        })
        .collect()
}

fn create_helix(radius: f32, pitch: f32, height: f32, segments: usize) -> Vec<Vec3> {
    let turns = height / pitch;
    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let angle = TAU * turns * t;
            let y = height * t - height / 2.0;
            Vec3::new(angle.cos() * radius, y, angle.sin() * radius)
        })
        .collect()
}
