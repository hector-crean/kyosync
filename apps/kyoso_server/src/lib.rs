//! `kyoso_server` — stateful WebSocket coordinator for
//! [`kyoso_crdt`]-replicated graphs.
//!
//! Architecture in two sentences: clients open a binary WebSocket to
//! `/ws`, send a `ClientMsg::Hello` naming their room, and from then on
//! every locally-generated op is shipped via `ClientMsg::Submit`. The
//! server assigns a `GlobalSeq`, persists into the room's op log
//! (Postgres or in-memory), folds the op into a server-side mirror used
//! for snapshots, and broadcasts `ServerMsg::Apply` to every connected
//! peer (including the originator).
//!
//! Two background workers run continuously:
//! - **snapshot scheduler** — periodic checkpoint of each room's mirror.
//! - **GC scheduler** — drops ops below
//!   `min(every connected peer's ack, latest snapshot.at_seq)`.

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
pub use services::{OpStore, Room, RoomManager};

#[derive(Clone)]
pub struct AppState {
    pub rooms: Arc<RoomManager>,
}

impl AppState {
    pub fn from_store(store: OpStore) -> Self {
        Self {
            rooms: Arc::new(RoomManager::new(store)),
        }
    }

    /// In-memory state for tests.
    pub fn in_memory() -> Self {
        Self::from_store(OpStore::in_memory())
    }
}

pub fn app(state: AppState) -> Router {
    router::build(state)
}
