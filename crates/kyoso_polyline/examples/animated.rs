//! Animated polyline example demonstrating dynamic line updates.
//!
//! Shows how to modify polyline vertices at runtime for animations.
//!
//! Run with: `cargo run --package kyoso_polyline --example animated`

use bevy::prelude::*;
use kyoso_polyline::prelude::*;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, PolylinePlugin))
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (animate_wave, animate_spiral, animate_particle_trail),
        )
        .run();
}

#[derive(Component)]
struct WavePolyline {
    handle: Handle<Polyline>,
    phase: f32,
}

#[derive(Component)]
struct SpiralPolyline {
    handle: Handle<Polyline>,
    rotation: f32,
}

#[derive(Component)]
struct ParticleTrail {
    handle: Handle<Polyline>,
    positions: Vec<Vec3>,
    time: f32,
}

fn setup(
    mut commands: Commands,
    mut polylines: ResMut<Assets<Polyline>>,
    mut materials: ResMut<Assets<PolylineMaterial>>,
) {
    // Camera
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 5.0, 15.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // --- Animated Wave ---
    let wave_handle = polylines.add(Polyline {
        vertices: generate_wave(0.0),
        colors: None,
    });

    commands.spawn((
        PolylineHandle(wave_handle.clone()),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 6.0,
            color: Color::srgb(0.2, 0.8, 1.0).to_linear(),
            ..default()
        })),
        Transform::from_xyz(0.0, 2.0, 0.0),
        WavePolyline {
            handle: wave_handle,
            phase: 0.0,
        },
    ));

    // --- Animated Rotating Spiral ---
    let spiral_handle = polylines.add(Polyline {
        vertices: generate_spiral(0.0),
        colors: None,
    });

    commands.spawn((
        PolylineHandle(spiral_handle.clone()),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 4.0,
            color: Color::srgb(1.0, 0.5, 0.2).to_linear(),
            perspective: true,
            ..default()
        })),
        Transform::from_xyz(-5.0, -1.0, 0.0),
        SpiralPolyline {
            handle: spiral_handle,
            rotation: 0.0,
        },
    ));

    // --- Particle Trail (Lissajous curve) ---
    let trail_handle = polylines.add(Polyline {
        vertices: vec![],
        colors: None,
    });

    commands.spawn((
        PolylineHandle(trail_handle.clone()),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 3.0,
            color: Color::srgb(0.9, 0.3, 0.7).to_linear(),
            ..default()
        })),
        Transform::from_xyz(5.0, -1.0, 0.0),
        ParticleTrail {
            handle: trail_handle,
            positions: Vec::with_capacity(200),
            time: 0.0,
        },
    ));

    // Static reference grid
    let grid_vertices: Vec<Vec3> = (-5..=5)
        .flat_map(|i| {
            vec![
                Vec3::new(i as f32, -3.0, -5.0),
                Vec3::new(i as f32, -3.0, 5.0),
            ]
        })
        .collect();

    commands.spawn((
        PolylineHandle(polylines.add(Polyline {
            vertices: grid_vertices,
            colors: None,
        })),
        PolylineMaterialHandle(materials.add(PolylineMaterial {
            width: 1.0,
            color: LinearRgba::new(0.4, 0.4, 0.4, 0.5),
            ..default()
        })),
        Transform::default(),
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
            "Animated Polylines\n\
             Top: Sine wave animation\n\
             Bottom Left: Rotating spiral\n\
             Bottom Right: Lissajous curve trail",
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

fn generate_wave(phase: f32) -> Vec<Vec3> {
    (0..80)
        .map(|i| {
            let x = (i as f32 - 40.0) * 0.15;
            let y = (x * 2.0 + phase).sin() * 0.5 + (x * 3.0 + phase * 1.5).sin() * 0.3;
            Vec3::new(x, y, 0.0)
        })
        .collect()
}

fn generate_spiral(rotation: f32) -> Vec<Vec3> {
    (0..60)
        .map(|i| {
            let t = i as f32 * 0.2;
            let r = 0.5 + t * 0.15;
            let angle = t + rotation;
            Vec3::new(angle.cos() * r, t * 0.15, angle.sin() * r)
        })
        .collect()
}

fn animate_wave(
    time: Res<Time>,
    mut polylines: ResMut<Assets<Polyline>>,
    mut query: Query<&mut WavePolyline>,
) {
    for mut wave in &mut query {
        wave.phase += time.delta_secs() * 3.0;
        if let Some(mut polyline) = polylines.get_mut(&wave.handle) {
            polyline.vertices = generate_wave(wave.phase);
        }
    }
}

fn animate_spiral(
    time: Res<Time>,
    mut polylines: ResMut<Assets<Polyline>>,
    mut query: Query<&mut SpiralPolyline>,
) {
    for mut spiral in &mut query {
        spiral.rotation += time.delta_secs() * 2.0;
        if let Some(mut polyline) = polylines.get_mut(&spiral.handle) {
            polyline.vertices = generate_spiral(spiral.rotation);
        }
    }
}

fn animate_particle_trail(
    time: Res<Time>,
    mut polylines: ResMut<Assets<Polyline>>,
    mut query: Query<&mut ParticleTrail>,
) {
    for mut trail in &mut query {
        trail.time += time.delta_secs();

        // Lissajous curve parameters
        let t = trail.time * 1.5;
        let x = (t * 3.0).sin() * 2.0;
        let y = (t * 2.0).sin() * 1.5;
        let z = (t * 1.0).cos() * 1.0;

        trail.positions.push(Vec3::new(x, y, z));

        // Keep only last N positions for trail effect
        const MAX_TRAIL_LENGTH: usize = 150;
        if trail.positions.len() > MAX_TRAIL_LENGTH {
            trail.positions.remove(0);
        }

        if let Some(mut polyline) = polylines.get_mut(&trail.handle) {
            polyline.vertices = trail.positions.clone();
        }
    }
}
