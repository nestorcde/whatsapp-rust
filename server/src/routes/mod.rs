use axum::{Router, http::StatusCode, routing::get};

use crate::state::AppState;

pub mod session;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(session::router())
        .with_state(state)
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}
