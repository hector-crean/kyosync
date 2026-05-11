//! Typed presence/awareness layer on top of `kyoso_sync`'s opaque
//! `RawPresence`.
//!
//! Each peer broadcasts a [`PresenceState`] every time their cursor
//! moves (throttled). Other peers decode it from the
//! [`RawPresenceEvent`] stream and render a coloured cursor sprite.
//! Same shape as the kyoso_client presence module — circuit design has
//! the same awareness needs (see another peer's cursor in real time).

use std::collections::HashMap;

use bevy::prelude::*;
use kyoso_crdt::PeerId;
use kyoso_sync::{
    ClearLocalPresence, RawPresence, RawPresenceEvent, SetLocalPresence, SyncStatus,
};
use serde::{Deserialize, Serialize};

use crate::msg::{Pos3, Rgb};

pub const PRESENCE_VERSION: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PresenceFrame {
    version: u16,
    payload: PresenceState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PresenceState {
    pub cursor: Pos3,
    pub color: Rgb,
    pub name: String,
}

pub fn encode_presence(state: &PresenceState) -> Result<Vec<u8>, postcard::Error> {
    let frame = PresenceFrame {
        version: PRESENCE_VERSION,
        payload: state.clone(),
    };
    postcard::to_allocvec(&frame)
}

pub fn decode_presence(bytes: &[u8]) -> Result<Option<PresenceState>, postcard::Error> {
    let frame: PresenceFrame = postcard::from_bytes(bytes)?;
    if frame.version != PRESENCE_VERSION {
        return Ok(None);
    }
    Ok(Some(frame.payload))
}

#[derive(Resource, Default, Debug)]
pub struct Presence {
    pub peers: HashMap<PeerId, PresenceState>,
}

#[derive(Message, Event, Debug, Clone)]
pub enum PresenceEvent {
    Joined { peer: PeerId, state: PresenceState },
    Updated { peer: PeerId, state: PresenceState },
    Left { peer: PeerId },
}

#[derive(Component, Debug, Clone, Copy)]
pub struct RemoteCursor {
    pub peer: PeerId,
}

#[derive(Resource, Default, Debug, Clone)]
pub struct LocalPresence(pub PresenceState);

pub struct PresencePlugin;

impl Plugin for PresencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Presence>();
        app.init_resource::<LocalPresence>();
        app.add_message::<PresenceEvent>();
        app.add_systems(
            Update,
            (
                update_typed_presence,
                broadcast_local_cursor,
                spawn_or_move_remote_cursors,
                despawn_left_cursors,
            ),
        );
    }
}

fn update_typed_presence(
    mut raw_events: MessageReader<RawPresenceEvent>,
    raw: Res<RawPresence>,
    mut typed: ResMut<Presence>,
    mut events: MessageWriter<PresenceEvent>,
) {
    for ev in raw_events.read() {
        match ev {
            RawPresenceEvent::Snapshot(_) => {
                typed.peers.clear();
                for (peer, bytes) in raw.0.iter() {
                    match decode_presence(bytes) {
                        Ok(Some(state)) => {
                            typed.peers.insert(*peer, state.clone());
                            events.write(PresenceEvent::Joined { peer: *peer, state });
                        }
                        Ok(None) => {
                            tracing::debug!(
                                peer = peer,
                                "presence snapshot frame: unsupported version, dropping"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(?e, peer = peer, "decode presence on snapshot");
                        }
                    }
                }
            }
            RawPresenceEvent::Updated { peer, state } => match decode_presence(state) {
                Ok(Some(decoded)) => {
                    let already = typed.peers.contains_key(peer);
                    typed.peers.insert(*peer, decoded.clone());
                    if already {
                        events.write(PresenceEvent::Updated {
                            peer: *peer,
                            state: decoded,
                        });
                    } else {
                        events.write(PresenceEvent::Joined {
                            peer: *peer,
                            state: decoded,
                        });
                    }
                }
                Ok(None) => {
                    tracing::debug!(
                        peer = peer,
                        "presence update: unsupported version, dropping"
                    );
                }
                Err(e) => {
                    tracing::warn!(?e, peer = peer, "decode presence on update");
                }
            },
            RawPresenceEvent::Left { peer } => {
                typed.peers.remove(peer);
                events.write(PresenceEvent::Left { peer: *peer });
            }
        }
    }
}

const PRESENCE_THROTTLE_SECS: f32 = 1.0 / 30.0;

fn broadcast_local_cursor(
    status: Res<SyncStatus>,
    time: Res<Time>,
    // `RayMap` only exists when the picking pipeline is wired (i.e. in
    // `VisualPlugin`); in headless tests it's absent and presence
    // broadcasts are simply skipped.
    ray_map: Option<Res<bevy::picking::backend::ray::RayMap>>,
    cameras: Query<Entity, With<kyoso_camera::markers::MainCamera>>,
    mut local: ResMut<LocalPresence>,
    mut sets: MessageWriter<SetLocalPresence>,
    mut last_sent_secs: Local<f32>,
) {
    let SyncStatus::Connected { peer } = *status else {
        return;
    };
    let Some(ray_map) = ray_map.as_deref() else {
        return;
    };
    let Some(world) = cursor_to_world(ray_map, &cameras) else {
        return;
    };
    let new_state = PresenceState {
        cursor: Pos3::from(world),
        color: peer_color(peer),
        name: format!("peer {peer}"),
    };
    if new_state == local.0 {
        return;
    }
    let now = time.elapsed_secs();
    if now - *last_sent_secs < PRESENCE_THROTTLE_SECS {
        return;
    }
    let bytes = match encode_presence(&new_state) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(?e, "encode local presence");
            return;
        }
    };
    sets.write(SetLocalPresence(bytes));
    local.0 = new_state;
    *last_sent_secs = now;
}

fn spawn_or_move_remote_cursors(
    presence: Res<Presence>,
    status: Res<SyncStatus>,
    mut cursors: Query<(&mut Transform, &RemoteCursor)>,
    mut commands: Commands,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let local_peer = match *status {
        SyncStatus::Connected { peer } => Some(peer),
        _ => None,
    };
    let mut existing: HashMap<PeerId, Entity> = cursors
        .iter()
        .map(|(_, rc)| (rc.peer, Entity::PLACEHOLDER))
        .collect();
    for (mut t, rc) in cursors.iter_mut() {
        if let Some(state) = presence.peers.get(&rc.peer) {
            t.translation = Vec3::new(state.cursor.x, state.cursor.y, state.cursor.z);
        }
        existing.insert(rc.peer, Entity::PLACEHOLDER);
    }
    for (peer, state) in presence.peers.iter() {
        if Some(*peer) == local_peer {
            continue;
        }
        if existing.contains_key(peer) {
            continue;
        }
        let mesh = meshes.add(Mesh::from(bevy::math::primitives::Sphere::new(0.18)));
        let mat = materials.add(StandardMaterial {
            base_color: Color::srgb(state.color.r, state.color.g, state.color.b),
            unlit: true,
            ..default()
        });
        commands.spawn((
            RemoteCursor { peer: *peer },
            Mesh3d(mesh),
            MeshMaterial3d(mat),
            Transform::from_xyz(state.cursor.x, state.cursor.y, state.cursor.z),
        ));
    }
}

fn despawn_left_cursors(
    presence: Res<Presence>,
    cursors: Query<(Entity, &RemoteCursor)>,
    mut commands: Commands,
) {
    for (entity, rc) in cursors.iter() {
        if !presence.peers.contains_key(&rc.peer) {
            commands.entity(entity).despawn();
        }
    }
}

fn peer_color(peer: PeerId) -> Rgb {
    let h = ((peer as u64).wrapping_mul(2654435761) % 360) as f32 / 360.0;
    let (r, g, b) = hsv_to_rgb(h, 0.7, 0.95);
    Rgb { r, g, b }
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Project the local mouse cursor onto the y=0 ground plane, returning
/// a world-space point. Used for broadcasting presence — peers see
/// each other's cursor as a small sphere on the ground plane.
fn cursor_to_world(
    ray_map: &bevy::picking::backend::ray::RayMap,
    cameras: &Query<Entity, With<kyoso_camera::markers::MainCamera>>,
) -> Option<Vec3> {
    use kyoso_camera::raycast::RayMapExt;
    let camera = cameras.iter().next()?;
    ray_map.pointer_plane_intersection(
        camera,
        bevy::picking::pointer::PointerId::Mouse,
        Vec3::ZERO,
        Vec3::Y,
    )
}

#[allow(dead_code)]
fn _ensure_export() {
    let _: Option<ClearLocalPresence> = None;
}
