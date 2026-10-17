use axum::extract::State;
use axum::{Json, Router, routing::get};
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_secs: u64,
}

async fn health_check(State(start_time): State<Arc<Instant>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy",
        uptime_secs: start_time.elapsed().as_secs(),
    })
}

pub fn health_router(start_time: Arc<Instant>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/ready", get(|| async { "ok" }))
        .with_state(start_time)
}
