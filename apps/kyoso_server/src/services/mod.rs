//! Server-side stateful services. Each service owns one slice of
//! application state and is referenced from [`crate::AppState`].

pub mod handler;
pub mod handlers;
pub mod room;
pub mod scheduler;
pub mod store;

pub use handler::{HandlerFactory, RoomModelHandler};
pub use handlers::{CommentsHandlerFactory, CommentsRoomHandler, GraphHandlerFactory, GraphRoomHandler};
pub use room::{Room, RoomManager};
pub use store::OpStore;
