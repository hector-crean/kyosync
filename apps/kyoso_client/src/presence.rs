//! Typed presence/awareness layer on top of `kyoso_sync`'s opaque
//! `RawPresence`.
//!
//! Each peer broadcasts a [`PresenceState`] every time their cursor
//! moves (throttled). Other peers decode it from the
//! [`RawPresenceEvent`] stream and render a coloured cursor sprite.
//!
//! ## Why this lives in the client crate, not in `kyoso_sync`
//!
//! Presence content is **product-specific**. A graph editor wants
//! `cursor + selection + display name`; a text editor wants `cursor
//! position in the document + selection range`. `kyoso_sync` keeps
//! presence opaque (`Vec<u8>`) so each consumer defines its own struct
//! and encoding. This module is the kyoso graph editor's choice.
//!
//! ## Why presence does **not** flow through `AppCommand`/`AppEvent`
//!
//! Volume. A cursor at 60 fps is 60 broadcasts per second per peer; in
//! a busy room that's the high-rate path of the entire system.
//! Routing those through the Duplex crossbeam bus would force every
//! external producer/observer to drain an awareness firehose they
//! don't care about. The wire-level split (`ClientMsg::Presence` vs
//! `ClientMsg::Submit`) is mirrored here in-process: presence has its
//! own resource (`Presence`), its own events (`PresenceEvent`), its
//! own systems. Consumers that want presence subscribe directly;
//! consumers that don't pay nothing.

use std::collections::HashMap;

use bevy::prelude::*;
use kyoso_crdt::PeerId;
use kyoso_sync::{
    ClearLocalPresence, RawPresence, RawPresenceEvent, SetLocalPresence, SyncStatus,
};
use serde::{Deserialize, Serialize};

use crate::msg::{Pos2, Rgb};

/// Wire-version this client speaks. Bumped any time [`PresenceState`]
/// changes shape in a way postcard can't decode forward-compat-style
/// (positional encoding makes adding new tail fields a breaking change
/// — postcard sees a too-short buffer and errors).
///
/// Decoders compare the incoming version and drop frames they don't
/// understand. Producers always tag with the version they encoded.
pub const PRESENCE_VERSION: u16 = 1;

/// On-wire envelope. Allows heterogeneous-client rooms to silently
/// drop frames from too-new peers instead of crashing on decode.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PresenceFrame {
    version: u16,
    payload: PresenceState,
}

/// The typed shape this consumer (graph editor) uses for awareness.
/// Encoded with postcard before crossing the wire, wrapped in a
/// [`PresenceFrame`] for versioning.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PresenceState {
    /// Cursor position in document/world coordinates.
    pub cursor: Pos2,
    /// Cursor display colour. Derived from `PeerId` so it's stable
    /// across reconnects.
    pub color: Rgb,
    /// Optional human-readable name. Empty string if unset.
    pub name: String,
}

/// Encode `state` as a versioned wire frame. Used by the cursor
/// broadcaster and by callers that want to push a one-off presence
/// update via [`SetLocalPresence`].
pub fn encode_presence(state: &PresenceState) -> Result<Vec<u8>, postcard::Error> {
    let frame = PresenceFrame {
        version: PRESENCE_VERSION,
        payload: state.clone(),
    };
    postcard::to_allocvec(&frame)
}

/// Decode a wire frame. Returns `Ok(None)` if the version is one we
/// don't recognise (the producer is newer; we silently ignore).
/// Returns `Err` only on actual malformed bytes.
pub fn decode_presence(bytes: &[u8]) -> Result<Option<PresenceState>, postcard::Error> {
    let frame: PresenceFrame = postcard::from_bytes(bytes)?;
    if frame.version != PRESENCE_VERSION {
        return Ok(None);
    }
    Ok(Some(frame.payload))
}

/// Typed view over `kyoso_sync::RawPresence`. Updated by
/// [`update_typed_presence`] whenever the raw layer fires an event.
#[derive(Resource, Default, Debug)]
pub struct Presence {
    pub peers: HashMap<PeerId, PresenceState>,
}

/// Higher-level events for consumers of the typed presence layer.
#[derive(Message, Event, Debug, Clone)]
pub enum PresenceEvent {
    Joined { peer: PeerId, state: PresenceState },
    Updated { peer: PeerId, state: PresenceState },
    Left { peer: PeerId },
}

/// Marker on the visual cursor sprite spawned per remote peer. The
/// `peer` field lets us look up the entity to despawn / move when a
/// `PresenceEvent` fires.
#[derive(Component, Debug, Clone, Copy)]
pub struct RemoteCursor {
    pub peer: PeerId,
}

/// Local-side resource holding the host's most recent broadcasted
/// presence — used to coalesce ("only send if changed") and avoid
/// flooding the wire with identical frames.
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

/// Decode raw presence events into the typed [`Presence`] resource and
/// fire [`PresenceEvent`] for downstream consumers. Decode failures
/// just log a warning and skip — old/incompatible peers shouldn't
/// crash the room.
fn update_typed_presence(
    mut raw_events: MessageReader<RawPresenceEvent>,
    raw: Res<RawPresence>,
    mut typed: ResMut<Presence>,
    mut events: MessageWriter<PresenceEvent>,
) {
    for ev in raw_events.read() {
        match ev {
            RawPresenceEvent::Snapshot(_) => {
                // Replace the typed map from the raw map. Cheap re-decode.
                typed.peers.clear();
                for (peer, bytes) in raw.0.iter() {
                    match decode_presence(bytes) {
                        Ok(Some(state)) => {
                            typed.peers.insert(*peer, state.clone());
                            events.write(PresenceEvent::Joined {
                                peer: *peer,
                                state,
                            });
                        }
                        Ok(None) => {
                            tracing::debug!(peer = peer, "presence snapshot frame: unsupported version, dropping");
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
                    tracing::debug!(peer = peer, "presence update: unsupported version, dropping");
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

/// Maximum cursor-broadcast rate, in seconds between frames. 30 Hz is
/// usually enough for smooth remote-cursor motion and roughly halves
/// the network volume vs. 60 Hz Bevy `Update`. Tune to taste.
const PRESENCE_THROTTLE_SECS: f32 = 1.0 / 30.0;

/// Broadcast the local cursor position with two layers of suppression:
///
/// 1. **Dedup** — never re-send a payload identical to the last one
///    (cursor parked on the same pixel).
/// 2. **Rate cap** — at most one frame per [`PRESENCE_THROTTLE_SECS`].
///    A cursor sweep that moves every frame still emits at the cap,
///    not at the Bevy update rate.
///
/// The "last sent" state is tracked in [`LocalPresence`]; the throttle
/// uses a `Local<f32>` accumulator on the system itself.
fn broadcast_local_cursor(
    status: Res<SyncStatus>,
    time: Res<Time>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mut local: ResMut<LocalPresence>,
    mut sets: MessageWriter<SetLocalPresence>,
    mut last_sent_secs: Local<f32>,
) {
    let SyncStatus::Connected { peer } = *status else {
        return;
    };
    let Some(world) = cursor_to_world(&windows, &cameras) else {
        return;
    };
    let new_state = PresenceState {
        cursor: Pos2::from(world),
        color: peer_color(peer),
        name: format!("peer {peer}"),
    };
    if new_state == local.0 {
        return;
    }
    let now = time.elapsed_secs();
    if now - *last_sent_secs < PRESENCE_THROTTLE_SECS {
        // Rate-cap: defer to a later frame. The next frame's run will
        // see the same delta and try again, capping wire volume.
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

/// For each remote peer in [`Presence`], either spawn a new cursor
/// sprite or move an existing one. Excludes the local peer.
fn spawn_or_move_remote_cursors(
    presence: Res<Presence>,
    status: Res<SyncStatus>,
    mut cursors: Query<(&mut Transform, &RemoteCursor)>,
    mut commands: Commands,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        // Headless / test context — skip cursor visuals.
        return;
    };
    let local_peer = match *status {
        SyncStatus::Connected { peer } => Some(peer),
        _ => None,
    };
    // Build a quick set of which peers already have an entity so we
    // don't double-spawn.
    let mut existing: HashMap<PeerId, Entity> = cursors
        .iter()
        .map(|(_, rc)| (rc.peer, Entity::PLACEHOLDER))
        .collect();
    // Update positions for already-spawned cursors.
    for (mut t, rc) in cursors.iter_mut() {
        if let Some(state) = presence.peers.get(&rc.peer) {
            t.translation.x = state.cursor.x;
            t.translation.y = state.cursor.y;
            t.translation.z = 100.0; // above nodes
        }
        existing.insert(rc.peer, Entity::PLACEHOLDER);
    }
    // Spawn for any peer in the typed presence map but not yet on screen.
    for (peer, state) in presence.peers.iter() {
        if Some(*peer) == local_peer {
            continue;
        }
        if existing.contains_key(peer) {
            continue;
        }
        let mesh = meshes.add(Mesh::from(bevy::math::primitives::Circle::new(8.0)));
        let mat = materials.add(ColorMaterial::from_color(Color::srgb(
            state.color.r,
            state.color.g,
            state.color.b,
        )));
        commands.spawn((
            RemoteCursor { peer: *peer },
            Mesh2d(mesh),
            MeshMaterial2d(mat),
            Transform::from_xyz(state.cursor.x, state.cursor.y, 100.0),
        ));
    }
}

/// Despawn cursor sprites for peers that have left.
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

/// Stable per-peer colour: hash the peer id into HSV.
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

fn cursor_to_world(
    windows: &Query<&Window>,
    cameras: &Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) -> Option<Vec2> {
    let window = windows.iter().next()?;
    let cursor = window.cursor_position()?;
    let (camera, cam_t) = cameras.iter().next()?;
    camera.viewport_to_world_2d(cam_t, cursor).ok()
}

/// Suppresses an unused-import warning when the `ClearLocalPresence`
/// message type isn't directly referenced in this file but should
/// still be re-exportable.
#[allow(dead_code)]
fn _ensure_export() {
    let _: Option<ClearLocalPresence> = None;
}
