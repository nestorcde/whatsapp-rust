use axum::{
    Router,
    http::StatusCode,
    middleware,
    routing::{get, post},
};

use crate::{middleware::auth, state::AppState};

pub mod messages_gx;
pub mod session;

pub fn router(state: AppState) -> Router {
    // Routes that require Bearer token auth.
    let protected = Router::new()
        .route("/api/{session}/start-session", post(session::start_session))
        .route("/api/{session}/status-session", get(session::status_session))
        .route("/api/{session}/send-message-gx", post(messages_gx::send_message_gx))
        .route("/api/{session}/resend-message-gx", post(messages_gx::resend_message_gx))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::verify_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        // No-auth session routes (secretkey validated inline).
        .route(
            "/api/{session}/{secretkey}/generate-token",
            post(session::generate_token),
        )
        .route(
            "/api/{secretkey}/show-all-sessions",
            get(session::show_all_sessions),
        )
        // Token is in path for <img src> use; verified inline.
        .route(
            "/api/{session}/{token}/qrcode-session",
            get(session::qrcode_session),
        )
        .merge(protected)
        .with_state(state)
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}
