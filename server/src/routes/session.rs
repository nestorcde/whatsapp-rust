use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{oracle, session_manager::SessionStatus, state::AppState};

// --- shared helpers ---

fn session_xml(status_str: &str, urlcode: &str) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<response><status>{status_str}</status><urlcode>{urlcode}</urlcode><version>1.0.0</version></response>"
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        xml,
    )
        .into_response()
}

fn status_label(st: &SessionStatus) -> &'static str {
    match st {
        SessionStatus::Closed => "CLOSED",
        SessionStatus::Starting => "STARTING",
        SessionStatus::WaitingQr => "WAITING_QR",
        SessionStatus::Connected => "CONNECTED",
    }
}

// bcrypt verification for the inline qrcode-session auth (token in URL path).
async fn verify_path_token(session: &str, raw_token: &str, secret_key: &str) -> bool {
    let token = raw_token.replace('_', "/").replace('-', "+");
    let plain = format!("{session}{secret_key}");
    tokio::task::spawn_blocking(move || bcrypt::verify(&plain, &token).unwrap_or(false))
        .await
        .unwrap_or(false)
}

// --- generate-token ---
// POST /api/:session/:secretkey/generate-token
// No Bearer auth — secretkey is validated against SECRET_KEY in the path.
pub async fn generate_token(
    Path((session, secretkey)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if secretkey != state.config.secret_key {
        return (StatusCode::UNAUTHORIZED, "Invalid secret key").into_response();
    }

    let plain = format!("{}{}", session, state.config.secret_key);
    let hash =
        match tokio::task::spawn_blocking(move || bcrypt::hash(&plain, 10)).await {
            Ok(Ok(h)) => h,
            _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Hash failed").into_response(),
        };

    // Sanitize for URL safety: / → _ and + → -
    let token = hash.replace('/', "_").replace('+', "-");

    if let Err(e) =
        oracle::upsert_oficial_token(state.oracle.clone(), session.clone(), token.clone()).await
    {
        tracing::warn!("upsert_oficial_token failed for '{session}': {e}");
    }

    let full = format!("{session}:{token}");
    Json(json!({ "status": "Success", "session": session, "token": token, "full": full }))
        .into_response()
}

// --- show-all-sessions ---
// GET /api/:secretkey/show-all-sessions
// No Bearer auth — secretkey validated in path.
pub async fn show_all_sessions(
    Path(secretkey): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if secretkey != state.config.secret_key {
        return (StatusCode::UNAUTHORIZED, "Invalid secret key").into_response();
    }
    let sessions = state.sessions.list_all().await;
    Json(json!({ "response": sessions })).into_response()
}

// --- start-session ---
// POST /api/:session/start-session  [verify_token middleware]
pub async fn start_session(
    Path(session): Path<String>,
    State(state): State<AppState>,
) -> Response {
    match state.sessions.start(&session).await {
        Ok(session_ref) => {
            let st = session_ref.status.read().await.clone();
            let qr = session_ref.qr_string.read().await.clone();
            session_xml(status_label(&st), qr.as_deref().unwrap_or(""))
        }
        Err(e) => {
            tracing::error!("start_session '{session}': {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to start session").into_response()
        }
    }
}

// --- status-session ---
// GET /api/:session/status-session  [verify_token middleware]
// If QR is available and `whacrecod` header is present, writes QR hex to Oracle.
pub async fn status_session(
    Path(session): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(session_ref) = state.sessions.get(&session).await else {
        return Json(json!({ "status": "CLOSED", "qrcode": null })).into_response();
    };

    let st = session_ref.status.read().await.clone();
    let qr = session_ref.qr_string.read().await.clone();

    if st == SessionStatus::Closed {
        return Json(json!({ "status": "CLOSED", "qrcode": null })).into_response();
    }

    if let Some(ref qr_str) = qr {
        let whacrecod = headers
            .get("whacrecod")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        if !whacrecod.is_empty() {
            let qr_hex: String = qr_str.bytes().map(|b| format!("{b:02X}")).collect();
            if let Err(e) =
                oracle::update_qr_code(state.oracle.clone(), whacrecod, qr_hex).await
            {
                tracing::warn!("update_qr_code failed: {e}");
            }
        }
    }

    session_xml(status_label(&st), qr.as_deref().unwrap_or(""))
}

// --- qrcode-session ---
// GET /api/:session/:token/qrcode-session
// Token is in the URL path (for use in <img src> tags). Auth is verified inline.
pub async fn qrcode_session(
    Path((session, token)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if !verify_path_token(&session, &token, &state.config.secret_key).await {
        return (StatusCode::UNAUTHORIZED, "Invalid token").into_response();
    }

    let Some(session_ref) = state.sessions.get(&session).await else {
        return Json(json!({ "status": "CLOSED", "qrcode": null })).into_response();
    };

    let qr = session_ref.qr_string.read().await.clone();
    let st = session_ref.status.read().await.clone();

    let Some(qr_str) = qr else {
        return Json(json!({ "status": status_label(&st), "qrcode": null })).into_response();
    };

    match tokio::task::spawn_blocking(move || render_qr_png(qr_str)).await {
        Ok(Ok(png)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "image/png")],
            png,
        )
            .into_response(),
        Ok(Err(e)) => {
            tracing::error!("QR render failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "QR render failed").into_response()
        }
        Err(e) => {
            tracing::error!("spawn_blocking panicked: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

fn render_qr_png(data: String) -> anyhow::Result<Vec<u8>> {
    let code = qrcode::QrCode::new(data.as_bytes())
        .map_err(|e| anyhow::anyhow!("QR encode: {e}"))?;
    let img = code.render::<image::Luma<u8>>().build();
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)?;
    Ok(cursor.into_inner())
}
