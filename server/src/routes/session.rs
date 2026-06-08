use axum::{Router, routing::get};

/// Placeholder router for session endpoints (Phases 2 & 5).
///
/// Routes wired here:
///   GET  /api/:secretkey/show-all-sessions
///   POST /api/:session/:secretkey/generate-token
///   POST /api/:session/start-session
///   GET  /api/:session/status-session
///   GET  /api/:session/:token/qrcode-session
pub fn router() -> Router {
    Router::new().route("/api/ping", get(ping))
}

/// Temporary smoke-test endpoint removed once real session routes are in place.
async fn ping() -> &'static str {
    "pong"
}
