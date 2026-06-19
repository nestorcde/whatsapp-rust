use axum::{
    Router,
    http::StatusCode,
    middleware,
    routing::{delete, get, post},
};

use crate::{
    middleware::{auth, google_auth},
    state::AppState,
};

pub mod google_contacts;
pub mod messages_gx;
pub mod session;
pub mod similarity;

pub fn router(state: AppState) -> Router {
    // Routes protected by WhatsApp session Bearer token.
    let wa_protected = Router::new()
        .route("/api/{session}/start-session", post(session::start_session))
        .route("/api/{session}/status-session", get(session::status_session))
        .route("/api/{session}/send-message-gx", post(messages_gx::send_message_gx))
        .route("/api/{session}/resend-message-gx", post(messages_gx::resend_message_gx))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::verify_token,
        ));

    // Routes protected by GOOGLE_CONTACTS_SECRET_KEY.
    let google_protected = Router::new()
        // Credentials management
        .route(
            "/api/google-contacts/credentials/{userKey}",
            post(google_contacts::store_credentials)
                .delete(google_contacts::delete_credentials),
        )
        .route(
            "/api/google-contacts/credentials/{userKey}/status",
            get(google_contacts::credentials_status),
        )
        // OAuth2 flow
        .route(
            "/api/google-contacts/auth/start/{userKey}",
            post(google_contacts::auth_start),
        )
        .route(
            "/api/google-contacts/auth/callback/{userKey}",
            post(google_contacts::auth_callback),
        )
        .route(
            "/api/google-contacts/auth/status/{userKey}",
            get(google_contacts::auth_status),
        )
        .route(
            "/api/google-contacts/auth/revoke/{userKey}",
            delete(google_contacts::auth_revoke),
        )
        // Users + contacts
        .route("/api/google-contacts/users", get(google_contacts::list_users))
        .route(
            "/api/google-contacts/{userKey}/search",
            post(google_contacts::search_contact),
        )
        .route(
            "/api/google-contacts/{userKey}/upsert",
            post(google_contacts::upsert_contacts),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            google_auth::verify_google_contacts_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/api/validate-lexical-similarity",
            post(similarity::validate_lexical_similarity),
        )
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
        .merge(wa_protected)
        .merge(google_protected)
        .with_state(state)
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}
