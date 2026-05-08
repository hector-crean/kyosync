use axum::Router;
use axum::routing::{any, get};
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::handlers::{health, room_ws};

pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/ws", any(room_ws::upgrade))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
