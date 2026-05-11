//! Multi-model transport for the [`kyoso_crdt`] envelope protocol.
//!
//! This crate is **model-agnostic**. It owns the WebSocket connection
//! ([`WsClient`], [`SyncTransportPlugin`]), exposes the per-peer
//! [`PeerIdGen`] handle so all CRDT models on a peer share one
//! `LocalSeq` namespace, and drains inbound envelopes into [`WsInbound`]
//! Bevy events for per-model plugins to consume.
//!
//! Per-model Bevy plugins live in their own crates:
//!
//! - [`kyoso_graph_sync`](https://docs.rs/kyoso_graph_sync) — graph
//!   model (`GraphSyncPlugin`, detection systems, projection, typed
//!   schema sync, edge category dispatch).
//! - [`kyoso_comments_sync`](https://docs.rs/kyoso_comments_sync) —
//!   comments / threads / annotations.
//!
//! ## Wiring an app
//!
//! ```ignore
//! use bevy::prelude::*;
//! use kyoso_sync::SyncTransportPlugin;
//! use kyoso_graph_sync::GraphSyncPlugin;
//! use kyoso_comments_sync::CommentsSyncPlugin;
//!
//! App::new()
//!     .add_plugins(SyncTransportPlugin::new("ws://localhost:7878/ws", "demo"))
//!     .add_plugins(GraphSyncPlugin::<MyNode, MyEdge>::default())
//!     .add_plugins(CommentsSyncPlugin::default())
//!     .run();
//! ```

pub mod client;
pub mod sequence_diff;
pub mod transport;

pub use client::{ConnectError, Inbound, WsClient};
pub use sequence_diff::sequence_diff;
pub use transport::{
    ClearLocalPresence, ModelRegistry, PeerIdGen, RawPresence, RawPresenceEvent,
    SetLocalPresence, SyncStatus, SyncTransportPlugin, WsBridge, WsInbound,
};
