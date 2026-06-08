use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::state::AppState;

/// Simple token check for Google Contacts endpoints.
/// Mirrors `googleAuth.middleware.ts → verifyGoogleContactsAuth`:
/// compare the `token` or `Authorization` header directly against
/// `GOOGLE_CONTACTS_SECRET_KEY` (stored in `AppState::config`).
pub async fn verify_google_contacts_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers().clone();

    let raw_token = headers
        .get("token")
        .or_else(|| headers.get("authorization"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    if raw_token.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Token not provided").into_response();
    }

    let token_value = if raw_token.to_ascii_lowercase().starts_with("bearer ") {
        raw_token[7..].to_owned()
    } else {
        raw_token
    };

    if token_value == state.config.google_contacts_secret_key {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
    }
}
