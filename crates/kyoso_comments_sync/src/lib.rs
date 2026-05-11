//! Comments-model Bevy plugin.
//!
//! Mounts on top of [`kyoso_sync::SyncTransportPlugin`] (the same
//! transport [`kyoso_graph_sync::GraphSyncPlugin`] uses) so the
//! comments and graph models multiplex onto one WebSocket connection
//! through the [`kyoso_crdt::EnvelopeServerMsg`] envelope.
//!
//! The plugin owns a [`CommentsClient`] Bevy resource that wraps a
//! [`CommentsBackend`](kyoso_comments_crdt::CommentsBackend), sharing
//! its [`IdGen`](kyoso_crdt::IdGen) handle with the
//! [`PeerIdGen`](kyoso_sync::PeerIdGen) — so every comment's `CrdtId`
//! comes from the same per-peer `LocalSeq` namespace as the graph's
//! node/edge IDs. That's what makes a comment's anchor (a graph
//! `CrdtId`) safe to store as a plain `CrdtId` value.
//!
//! ## Wiring
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_sync::SyncTransportPlugin;
//! use kyoso_graph_sync::GraphSyncPlugin;
//! use kyoso_comments_sync::CommentsSyncPlugin;
//!
//! App::new()
//!     .add_plugins((
//!         SyncTransportPlugin::new("ws://localhost:7878/ws", "demo"),
//!         GraphSyncPlugin::<MyNode, MyEdge>::default(),
//!         CommentsSyncPlugin::default(),
//!     ))
//!     .run();
//! ```
//!
//! For comments-only apps a single-model convenience constructor is
//! available: `CommentsSyncPlugin::new(url, room)` bundles in
//! `SyncTransportPlugin` automatically.
//!
//! ## Inbound + outbound
//!
//! - **Inbound**: [`CommentsSyncPlugin`] reads
//!   [`WsInbound`](kyoso_sync::WsInbound) events, filters for
//!   [`comments_model()`](kyoso_comments_crdt::comments_model), decodes
//!   per-model payloads, applies to [`CommentsClient`], and emits
//!   [`RemoteCommentApplied`] for downstream consumers.
//! - **Outbound**: drains [`CommentsClient::drain_pending`], encodes
//!   each op, and submits through
//!   [`WsBridge::submit`](kyoso_sync::WsBridge::submit).

pub mod plugin;
pub mod resource;

pub use plugin::{CommentsSyncPlugin, RemoteCommentApplied};
pub use resource::CommentsClient;
