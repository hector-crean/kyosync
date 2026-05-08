use axum::Json;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub rooms: usize,
}

pub async fn health(axum::extract::State(state): axum::extract::State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        rooms: state.rooms.count(),
    })
}
