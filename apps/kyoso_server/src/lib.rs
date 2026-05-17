//! `kyoso_server` — stateful WebSocket coordinator for the multi-model
//! [`kyoso_crdt`] envelope protocol.
//!
//! Architecture: clients open a binary WebSocket to `/ws`, send an
//! [`EnvelopeClientMsg::Hello`](kyoso_crdt::EnvelopeClientMsg::Hello)
//! listing the models they want to subscribe to, then ship per-model
//! ops as `Submit` envelopes. The server routes by [`ModelId`] to the
//! matching [`RoomModelHandler`] for the room, which owns its model's
//! storage, mirror, and append-lock. The handler returns the encoded
//! stamped op, [`Room`] wraps it in
//! [`EnvelopeServerMsg::Apply`](kyoso_crdt::EnvelopeServerMsg::Apply)
//! and broadcasts.
//!
//! Built-in handlers:
//! - [`GraphHandlerFactory`] — graph CRDT, persistent ([`OpStore`]).
//! - [`CommentsHandlerFactory`] — comments CRDT, in-memory.
//!
//! Custom models are added by implementing [`RoomModelHandler`] +
//! [`HandlerFactory`] and registering the factory in
//! [`AppState::with_factories`].
//!
//! Two background workers run continuously:
//! - **snapshot scheduler** — calls [`Room::take_snapshot_all`] on
//!   every live room (handlers that don't snapshot are no-ops).
//! - **GC scheduler** — calls [`Room::run_gc_all`].

use std::sync::Arc;

use axum::Router;

pub mod config;
pub mod error;
pub mod handlers;
pub mod model;
pub mod router;
pub mod services;
pub mod shutdown;

pub use config::Config;
pub use error::{AppError, Result};
pub use services::scheduler::{self, SchedulerConfig};
pub use services::{
    GraphHandlerFactory, GraphRoomHandler, HandlerFactory, OpStore, Room, RoomManager,
    RoomModelHandler,
};

#[derive(Clone)]
pub struct AppState {
    pub rooms: Arc<RoomManager>,
}

impl AppState {
    /// Build a server with a custom set of handler factories. Each
    /// factory's `model_id()` becomes one of the models the server
    /// will accept on every room.
    pub fn with_factories(factories: Vec<Box<dyn HandlerFactory>>) -> Self {
        Self {
            rooms: Arc::new(RoomManager::new(Arc::new(factories))),
        }
    }

    /// Build the default in-memory app: graph (in-memory `OpStore`)
    /// + comments (in-memory log). Used by every test in the
    /// workspace; production deployments build their own factory list
    /// via [`Self::with_factories`].
    pub fn in_memory() -> Self {
        Self::from_store(OpStore::in_memory())
    }

    /// Build with a specific (typically Postgres) [`OpStore`] for the
    /// graph model + the in-memory comments handler.
    pub fn from_store(store: OpStore) -> Self {
        Self::with_factories(vec![Box::new(GraphHandlerFactory::new(store))])
    }
}

pub fn app(state: AppState) -> Router {
    router::build(state)
}
