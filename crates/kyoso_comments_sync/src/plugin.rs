//! [`CommentsSyncPlugin`] — comments Bevy plugin.
//!
//! Mounts on top of [`kyoso_sync::SyncTransportPlugin`] (which it can
//! pull in automatically via [`Self::new`] for single-model apps).

use std::marker::PhantomData;

use bevy::prelude::*;
use kyoso_comments_crdt::{CommentOpKind, CommentsSnapshot, comments_model};
use kyoso_crdt::{Diff, GlobalSeq, Op};
use kyoso_sync::{ModelRegistry, PeerIdGen, SyncStatus, WsBridge, WsInbound};

use crate::resource::CommentsClient;

type CommentOp = Op<CommentOpKind>;

/// Emitted once per server-confirmed comments op as soon as the client
/// has applied it. Apps wanting to react to remote comment changes
/// (refresh a UI panel, fire a notification) read this stream.
#[derive(Message, Event, Clone, Debug)]
pub struct RemoteCommentApplied(pub CommentOp);

/// Tracks the last `applied_seq` we sent to the server via Ack so we
/// only emit a Ping when something actually changed.
#[derive(Resource, Default)]
struct CommentsLastAck(GlobalSeq);

/// Comments-model Bevy plugin. Add **after** (or together with)
/// [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin).
///
/// ```ignore
/// // Multi-model alongside graph
/// App::new()
///     .add_plugins((
///         SyncTransportPlugin::new("ws://...", "demo"),
///         GraphSyncPlugin::<N, E>::default(),
///         CommentsSyncPlugin::default(),
///     ))
///     .run();
///
/// // Single-model comments-only
/// App::new()
///     .add_plugins(CommentsSyncPlugin::new("ws://...", "demo"))
///     .run();
/// ```
pub struct CommentsSyncPlugin {
    /// When `Some`, this plugin also adds a `SyncTransportPlugin` with
    /// these `(url, room)` parameters during `build` (single-model
    /// convenience).
    transport: Option<(String, String)>,
    _phantom: PhantomData<()>,
}

impl Default for CommentsSyncPlugin {
    fn default() -> Self {
        Self {
            transport: None,
            _phantom: PhantomData,
        }
    }
}

impl CommentsSyncPlugin {
    /// Single-model convenience constructor. Bundles a
    /// [`SyncTransportPlugin`](kyoso_sync::SyncTransportPlugin) with
    /// `url` + `room` if the caller hasn't added one already.
    pub fn new(url: impl Into<String>, room: impl Into<String>) -> Self {
        Self {
            transport: Some((url.into(), room.into())),
            _phantom: PhantomData,
        }
    }
}

impl Plugin for CommentsSyncPlugin {
    fn build(&self, app: &mut App) {
        if let Some((url, room)) = &self.transport {
            if !app.is_plugin_added::<kyoso_sync::SyncTransportPlugin>() {
                app.add_plugins(kyoso_sync::SyncTransportPlugin::new(
                    url.clone(),
                    room.clone(),
                ));
            }
        }
        // Idempotent.
        app.init_resource::<ModelRegistry>();
        app.init_resource::<PeerIdGen>();

        // Register the comments model so the transport's Hello includes it.
        app.world_mut()
            .resource_mut::<ModelRegistry>()
            .register(comments_model());

        // Construct the client sharing the peer-level IdGen handle.
        let peer_ids = app.world().resource::<PeerIdGen>().handle();
        app.insert_resource(CommentsClient::with_shared_ids(peer_ids));

        app.add_message::<RemoteCommentApplied>();
        app.init_resource::<CommentsLastAck>();

        app.add_systems(
            Update,
            (comments_inbound_system, comments_outbound_system).chain(),
        );
    }
}

// ---------------------------------------------------------------------------
// Inbound — read WsInbound events, filter for comments, apply
// ---------------------------------------------------------------------------

fn comments_inbound_system(
    mut events: MessageReader<WsInbound>,
    mut client: ResMut<CommentsClient>,
    mut remote_events: MessageWriter<RemoteCommentApplied>,
) {
    let comments = comments_model();
    for event in events.read() {
        match event {
            WsInbound::Welcome { peer, models, .. } => {
                client.set_peer(*peer);
                let Some(greeting) = models.iter().find(|g| g.model == comments) else {
                    // Comments not subscribed by this connection — skip.
                    continue;
                };
                if let Some(snap_bytes) = &greeting.snapshot_payload {
                    match postcard::from_bytes::<CommentsSnapshot>(snap_bytes) {
                        Ok(snap) => client.restore(snap),
                        Err(e) => tracing::warn!(?e, "decode comments snapshot"),
                    }
                }
                let diff: Diff<CommentOpKind> =
                    match postcard::from_bytes(&greeting.diff_payload) {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(?e, "decode comments diff");
                            continue;
                        }
                    };
                for op in &diff.ops {
                    if let Err(e) = client.apply_remote(op) {
                        tracing::warn!(?e, ?op, "comments apply_remote rejected");
                        continue;
                    }
                    remote_events.write(RemoteCommentApplied(op.clone()));
                }
            }
            WsInbound::ModelApply { model, payload } if *model == comments => {
                let op: CommentOp = match postcard::from_bytes(payload) {
                    Ok(op) => op,
                    Err(e) => {
                        tracing::warn!(?e, "decode comments Apply payload");
                        continue;
                    }
                };
                if let Err(e) = client.apply_remote(&op) {
                    tracing::warn!(?e, ?op, "comments apply_remote rejected");
                    continue;
                }
                remote_events.write(RemoteCommentApplied(op));
            }
            WsInbound::ModelApplyBatch { model, payloads } if *model == comments => {
                for payload in payloads {
                    let op: CommentOp = match postcard::from_bytes(payload) {
                        Ok(op) => op,
                        Err(e) => {
                            tracing::warn!(?e, "decode comments ApplyBatch payload");
                            continue;
                        }
                    };
                    if let Err(e) = client.apply_remote(&op) {
                        tracing::warn!(?e, ?op, "comments apply_remote rejected");
                        continue;
                    }
                    remote_events.write(RemoteCommentApplied(op));
                }
            }
            WsInbound::ModelCatchup { model, payload } if *model == comments => {
                let diff: Diff<CommentOpKind> = match postcard::from_bytes(payload) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(?e, "decode comments Catchup payload");
                        continue;
                    }
                };
                for op in &diff.ops {
                    if let Err(e) = client.apply_remote(op) {
                        tracing::warn!(?e, ?op, "comments apply_remote rejected");
                        continue;
                    }
                    remote_events.write(RemoteCommentApplied(op.clone()));
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound — drain pending comments ops + ack
// ---------------------------------------------------------------------------

fn comments_outbound_system(
    mut client: ResMut<CommentsClient>,
    bridge: Option<Res<WsBridge>>,
    status: Res<SyncStatus>,
    mut last_ack: ResMut<CommentsLastAck>,
) {
    if !status.is_connected() {
        return;
    }
    let Some(bridge) = bridge else { return };
    let comments = comments_model();
    let pending = client.drain_pending();
    for op in pending {
        let payload = match postcard::to_allocvec(&op) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(?e, "encode pending comment op");
                continue;
            }
        };
        if !bridge.submit(comments.clone(), payload) {
            return;
        }
    }
    let applied = client.applied_seq();
    if applied > last_ack.0 && bridge.ack(comments.clone(), applied) {
        last_ack.0 = applied;
    }
}
