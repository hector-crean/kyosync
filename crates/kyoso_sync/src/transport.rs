//! Multi-model transport layer.
//!
//! [`SyncTransportPlugin`] is the foundation that per-model plugins
//! ([`kyoso_graph_sync::GraphSyncPlugin`], [`kyoso_comments_sync::CommentsSyncPlugin`])
//! sit on top of. It owns:
//!
//! - **[`WsBridge`]** — the [`crate::WsClient`] connection. One per app.
//! - **[`ModelRegistry`]** — the list of [`ModelId`]s the peer subscribes
//!   to. Per-model plugins push their model id during their `build()`.
//! - **[`PeerIdGen`]** — the peer-level [`IdGen`] handle. Per-model
//!   plugins clone this so all CRDT models on the peer share the same
//!   `LocalSeq` namespace (which is what makes cross-model `CrdtId`
//!   references collision-free).
//! - **Dispatch** — a `PreUpdate` system drains [`WsClient::try_recv`]
//!   and re-emits each event as a [`WsInbound`] Bevy event so multiple
//!   per-model plugins can each consume the events they care about.
//!
//! ## Plugin order
//!
//! Add `SyncTransportPlugin` **first**, then per-model plugins:
//!
//! ```ignore
//! App::new()
//!     .add_plugins(SyncTransportPlugin::new("ws://...", "room"))
//!     .add_plugins(GraphSyncPlugin::<MyNode, MyEdge>::default())
//!     .add_plugins(CommentsSyncPlugin::default())
//!     .run();
//! ```
//!
//! Per-model plugins register their model id during `build()`. The
//! transport opens the WebSocket in `PreStartup` (after every plugin's
//! `build()` has run), so the initial `Hello` carries every registered
//! model.

use bevy::prelude::*;
use kyoso_crdt::{
    EnvelopeClientMsg, GlobalSeq, IdGen, ModelGreeting, ModelId, PeerId, RoomId, Tier,
};

use crate::client::{Inbound, WsClient};

/// Bevy event mirroring [`Inbound`] one-to-one.
///
/// The transport layer drains the [`WsClient`]'s single-consumer
/// channel and emits one [`WsInbound`] per event so multiple per-model
/// systems can fan out without contending for the channel.
#[derive(Message, Event, Clone, Debug)]
pub enum WsInbound {
    Welcome {
        peer: PeerId,
        tier_granted: Tier,
        models: Vec<ModelGreeting>,
        presence: Vec<(PeerId, Vec<u8>)>,
    },
    /// One stamped op for one model — live (uncoalesced) path.
    ModelApply {
        model: ModelId,
        payload: Vec<u8>,
    },
    /// Multiple stamped ops for one model on the coalesced reader
    /// path. Apply each `payload` in order; semantics match
    /// [`Self::ModelApply::payload`].
    ModelApplyBatch {
        model: ModelId,
        payloads: Vec<Vec<u8>>,
    },
    ModelCatchup {
        model: ModelId,
        payload: Vec<u8>,
    },
    PresenceUpdate {
        peer: PeerId,
        state: Vec<u8>,
    },
    PresenceLeft {
        peer: PeerId,
    },
    ServerError(String),
    Disconnected,
}

/// Connection lifecycle observable from the host. Consumers can gate
/// game logic on `is_synced()` returning true.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    AwaitingConnect,
    AwaitingWelcome,
    Connected { peer: PeerId },
    Disconnected,
}

impl SyncStatus {
    pub fn is_connected(self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Holds the open [`WsClient`]. Inserted by the connect system in
/// `PreStartup` once every model plugin has registered its [`ModelId`].
#[derive(Resource)]
pub struct WsBridge {
    pub(crate) client: WsClient,
}

impl WsBridge {
    /// Send any envelope through the transport.
    pub fn send_envelope(&self, msg: EnvelopeClientMsg) -> bool {
        self.client.send_envelope(msg)
    }

    /// Convenience: wrap a per-model op payload in
    /// [`EnvelopeClientMsg::Submit`] and queue it.
    pub fn submit(&self, model: ModelId, payload: Vec<u8>) -> bool {
        self.client.submit(model, payload)
    }

    /// Convenience: per-model catchup request.
    pub fn catchup(&self, model: ModelId, since: GlobalSeq) -> bool {
        self.client.catchup(model, since)
    }

    /// Convenience: per-model ack.
    pub fn ack(&self, model: ModelId, applied_seq: GlobalSeq) -> bool {
        self.client.ack(model, applied_seq)
    }

    pub fn send_presence(&self, state: Vec<u8>) -> bool {
        self.client.send_presence(state)
    }

    pub fn leave_presence(&self) -> bool {
        self.client.leave_presence()
    }
}

/// Models the peer subscribes to. Per-model plugins push their
/// [`ModelId`] here during `build()`; [`SyncTransportPlugin`] reads the
/// list on connect.
#[derive(Resource, Default, Debug)]
pub struct ModelRegistry {
    models: Vec<ModelId>,
}

impl ModelRegistry {
    /// Register a model id. Idempotent — duplicate registrations are
    /// silently dropped.
    pub fn register(&mut self, model: ModelId) {
        if !self.models.contains(&model) {
            self.models.push(model);
        }
    }

    /// All registered models, in registration order.
    #[must_use]
    pub fn all(&self) -> &[ModelId] {
        &self.models
    }
}

/// Cloneable handle to the peer-level [`IdGen`].
///
/// Inserted as a Bevy resource by [`SyncTransportPlugin`]. Per-model
/// plugins clone this and pass it to their model backend's
/// `with_shared_ids` constructor — that's how all CRDT models on the
/// peer end up minting from one `LocalSeq` counter, which is what makes
/// cross-model `CrdtId` references safe.
#[derive(Resource, Clone, Debug, Default)]
pub struct PeerIdGen {
    ids: IdGen,
}

impl PeerIdGen {
    pub fn new(peer: PeerId) -> Self {
        Self {
            ids: IdGen::new(peer),
        }
    }

    /// Cloneable handle suitable for passing to per-model backends.
    #[must_use]
    pub fn handle(&self) -> IdGen {
        self.ids.clone()
    }

    /// Read the peer id this handle is currently bound to.
    pub fn peer(&self) -> PeerId {
        self.ids.peer()
    }

    /// Reset to a different peer once the server's `Welcome` arrives.
    /// Visible to every cloned handle held by per-model backends.
    pub fn set_peer(&self, peer: PeerId) {
        self.ids.set_peer(peer);
    }
}

/// Per-peer ephemeral presence/awareness state. Bytes are opaque —
/// each consumer postcard-encodes their own struct (cursor + selection
/// + display name + colour + …). Updated by the transport's inbound
/// dispatch as `PresenceUpdate` / `PresenceLeft` arrive; cleared on
/// disconnect.
///
/// **Not** part of any CRDT: no `GlobalSeq`, no log, no persistence.
/// Lost when peers disconnect.
#[derive(Resource, Default, Debug)]
pub struct RawPresence(pub std::collections::HashMap<PeerId, Vec<u8>>);

/// Push a new presence state for the local peer.
#[derive(Message, Event, Debug, Clone)]
pub struct SetLocalPresence(pub Vec<u8>);

/// Clear the local peer's presence.
#[derive(Message, Event, Debug, Clone, Copy)]
pub struct ClearLocalPresence;

/// Observation of remote peers' presence changes.
#[derive(Message, Event, Debug, Clone)]
pub enum RawPresenceEvent {
    /// Initial room snapshot, delivered once per `Welcome`.
    Snapshot(Vec<(PeerId, Vec<u8>)>),
    /// Peer `peer` updated their presence.
    Updated { peer: PeerId, state: Vec<u8> },
    /// Peer `peer` cleared their presence (explicit leave or disconnect).
    Left { peer: PeerId },
}

/// Multi-model transport plugin.
///
/// Owns the [`WsClient`] and dispatches inbound envelopes as
/// [`WsInbound`] Bevy events. Add this plugin **first**; per-model
/// plugins register their model with [`ModelRegistry`] during `build()`,
/// then the transport sends `Hello` with the full model list in
/// `PreStartup`.
pub struct SyncTransportPlugin {
    pub url: String,
    pub room: RoomId,
    /// Bandwidth/permission tier requested at `Hello`. Defaults to
    /// [`Tier::ReadWrite`]; passive viewers should pass [`Tier::Read`]
    /// to land on the server's coalesced fanout path.
    pub tier: Tier,
}

impl SyncTransportPlugin {
    pub fn new(url: impl Into<String>, room: impl Into<RoomId>) -> Self {
        Self {
            url: url.into(),
            room: room.into(),
            tier: Tier::ReadWrite,
        }
    }

    /// Builder: connect as a passive observer on the coalesced reader
    /// fanout path. Per-model handlers may still allow some submits
    /// (e.g. comments).
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.tier = Tier::Read;
        self
    }
}

impl Plugin for SyncTransportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ModelRegistry>();
        app.init_resource::<PeerIdGen>();
        app.init_resource::<RawPresence>();
        app.insert_resource(SyncStatus::AwaitingConnect);
        app.add_message::<WsInbound>();
        app.add_message::<SetLocalPresence>();
        app.add_message::<ClearLocalPresence>();
        app.add_message::<RawPresenceEvent>();

        let url = self.url.clone();
        let room = self.room.clone();
        let tier = self.tier;
        app.add_systems(
            PreStartup,
            move |mut commands: Commands,
                  registry: Res<ModelRegistry>,
                  mut status: ResMut<SyncStatus>| {
                let models: Vec<_> = registry.all().iter().map(|m| (m.clone(), 0)).collect();
                if models.is_empty() {
                    tracing::warn!(
                        "SyncTransportPlugin: no models registered; \
                         the connection will still open but the server will reject the Hello"
                    );
                }
                let client = match WsClient::connect(&url, &room, tier, models) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(error = ?e, "ws connect failed");
                        *status = SyncStatus::Disconnected;
                        return;
                    }
                };
                commands.insert_resource(WsBridge { client });
                *status = SyncStatus::AwaitingWelcome;
            },
        );

        // Drain the WsClient channel into Bevy events, and update peer
        // id + presence on Welcome / PresenceUpdate / PresenceLeft.
        app.add_systems(
            PreUpdate,
            (drain_inbound_system, presence_outbound_system).chain(),
        );
    }
}

/// Drain [`WsClient::try_recv`] into [`WsInbound`] events. Also
/// updates [`PeerIdGen`] on Welcome (so per-model backends see the
/// assigned peer id), refreshes [`RawPresence`], and emits
/// [`RawPresenceEvent`] for downstream presence projection.
fn drain_inbound_system(
    bridge: Option<Res<WsBridge>>,
    mut events: MessageWriter<WsInbound>,
    mut status: ResMut<SyncStatus>,
    peer_ids: Res<PeerIdGen>,
    mut raw_presence: ResMut<RawPresence>,
    mut presence_events: MessageWriter<RawPresenceEvent>,
) {
    let Some(bridge) = bridge else { return };
    while let Some(inbound) = bridge.client.try_recv() {
        match &inbound {
            Inbound::Welcome { peer, presence, .. } => {
                peer_ids.set_peer(*peer);
                raw_presence.0.clear();
                raw_presence
                    .0
                    .extend(presence.iter().map(|(p, s)| (*p, s.clone())));
                presence_events.write(RawPresenceEvent::Snapshot(presence.clone()));
                *status = SyncStatus::Connected { peer: *peer };
            }
            Inbound::PresenceUpdate { peer, state } => {
                raw_presence.0.insert(*peer, state.clone());
                presence_events.write(RawPresenceEvent::Updated {
                    peer: *peer,
                    state: state.clone(),
                });
            }
            Inbound::PresenceLeft { peer } => {
                raw_presence.0.remove(peer);
                presence_events.write(RawPresenceEvent::Left { peer: *peer });
            }
            Inbound::ServerError(msg) => {
                tracing::warn!(message = %msg, "server error");
            }
            Inbound::Disconnected => {
                *status = SyncStatus::Disconnected;
                raw_presence.0.clear();
            }
            _ => {}
        }
        events.write(inbound_to_event(inbound));
    }
}

fn inbound_to_event(inbound: Inbound) -> WsInbound {
    match inbound {
        Inbound::Welcome {
            peer,
            tier_granted,
            models,
            presence,
        } => WsInbound::Welcome {
            peer,
            tier_granted,
            models,
            presence,
        },
        Inbound::ModelApply { model, payload } => WsInbound::ModelApply { model, payload },
        Inbound::ModelApplyBatch { model, payloads } => {
            WsInbound::ModelApplyBatch { model, payloads }
        }
        Inbound::ModelCatchup { model, payload } => WsInbound::ModelCatchup { model, payload },
        Inbound::PresenceUpdate { peer, state } => WsInbound::PresenceUpdate { peer, state },
        Inbound::PresenceLeft { peer } => WsInbound::PresenceLeft { peer },
        Inbound::ServerError(s) => WsInbound::ServerError(s),
        Inbound::Disconnected => WsInbound::Disconnected,
    }
}

/// Drain [`SetLocalPresence`] / [`ClearLocalPresence`] events and
/// forward to the wire. Each event is one frame — coalesce upstream if
/// you want to throttle.
fn presence_outbound_system(
    bridge: Option<Res<WsBridge>>,
    status: Res<SyncStatus>,
    mut sets: MessageReader<SetLocalPresence>,
    mut clears: MessageReader<ClearLocalPresence>,
) {
    let Some(bridge) = bridge else {
        sets.clear();
        clears.clear();
        return;
    };
    if !status.is_connected() {
        // Drop pending presence updates — they're valid only against a
        // live connection. The next reconnect's Welcome will give us a
        // fresh map and the consumer can re-emit.
        sets.clear();
        clears.clear();
        return;
    }
    for SetLocalPresence(state) in sets.read() {
        let _ = bridge.send_presence(state.clone());
    }
    for _ in clears.read() {
        let _ = bridge.leave_presence();
    }
}
