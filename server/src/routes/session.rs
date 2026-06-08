use axum::{Router, routing::get};

use crate::state::AppState;

/// Session endpoint router (Phases 5+). Real handlers replace the placeholder
/// once Phases 2–4 are wired up.
///
/// Routes to implement:
///   POST /api/:session/:secretkey/generate-token
///   GET  /api/:secretkey/show-all-sessions
///   POST /api/:session/start-session
///   GET  /api/:session/status-session
///   GET  /api/:session/:token/qrcode-session
pub fn router() -> Router<AppState> {
    Router::new().route("/api/ping", get(ping))
}

async fn ping() -> &'static str {
    "pong"
}
