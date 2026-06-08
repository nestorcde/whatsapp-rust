use axum::{Router, http::StatusCode, routing::get};

pub mod session;

pub fn router() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(session::router())
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}
