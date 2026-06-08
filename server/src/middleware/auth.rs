use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::state::AppState;

/// Extract and validate the bearer token for a session-scoped endpoint.
///
/// Logic mirrors `auth.ts → verifyToken`:
/// 1. Derive `session_name` from the URI path (position 2 after "/api/").
/// 2. Try `token` header → `Authorization: Bearer …` header.
///    Token may also be embedded as `session:token` in the session path segment.
/// 3. Revert sanitization: replace `_` → `/` and `-` → `+`.
/// 4. `bcrypt::verify(session + SECRET_KEY, token)` — the bearer token IS the
///    stored bcrypt hash; the server re-verifies rather than doing a DB lookup.
pub async fn verify_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let uri_path = request.uri().path().to_owned();
    let headers = request.headers().clone();

    // --- session name ---
    // Segment layout: "" / "api" / session [/ ...]
    let raw_session = uri_path.split('/').nth(2).unwrap_or("").to_owned();

    // session may be "name:token" — split it.
    let (session_name, embedded_token) = if raw_session.contains(':') {
        let mut parts = raw_session.splitn(2, ':');
        let s = parts.next().unwrap_or("").to_owned();
        let t = parts.next().unwrap_or("").to_owned();
        (s, Some(t))
    } else {
        (raw_session, None)
    };

    if session_name.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Session not informed").into_response();
    }

    // --- raw token from headers or embedded ---
    let raw_token: String = if let Some(t) = embedded_token {
        t
    } else {
        // Prefer `token` header (original wppconnect custom header), fall back to Authorization.
        let from_headers = headers
            .get("token")
            .or_else(|| headers.get("authorization"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        // Strip optional "Bearer " prefix.
        if from_headers.to_ascii_lowercase().starts_with("bearer ") {
            from_headers[7..].to_owned()
        } else {
            from_headers
        }
    };

    if raw_token.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            "Token is not present. Check your header and try again",
        )
            .into_response();
    }

    // Revert generate-token sanitization: _ → / and - → +
    let token_bcrypt = raw_token.replace('_', "/").replace('-', "+");
    let plain = format!("{}{}", session_name, state.config.secret_key);

    // bcrypt::verify is CPU-bound — run it off the async executor.
    let valid = tokio::task::spawn_blocking(move || {
        bcrypt::verify(&plain, &token_bcrypt).unwrap_or(false)
    })
    .await
    .unwrap_or(false);

    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            "Check that the Session and Token are correct",
        )
            .into_response();
    }

    next.run(request).await
}
