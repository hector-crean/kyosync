//! Built-in [`RoomModelHandler`](super::handler::RoomModelHandler)
//! implementations.
//!
//! Out-of-tree models live in their own crates and follow the same
//! pattern: implement `RoomModelHandler` for the per-room state,
//! `HandlerFactory` for the startup-time constructor.

pub mod graph;

pub use graph::{GraphHandlerFactory, GraphRoomHandler};
