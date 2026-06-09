use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use wacore::download::MediaType;
use waproto::whatsapp as wa;
use whatsapp_rust::{Client, Jid, UploadOptions};

use crate::{
    oracle::{self, MsgRow, OracleConfig},
    session_manager::{SessionRef, SessionStatus},
    state::AppState,
};

// ── Request bodies ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SendGxBody {
    pub whacrecod: String,
    pub sender: String,
    #[serde(default)]
    pub cantidad: i32,
    #[serde(rename = "segRetDsd", default)]
    pub seg_ret_dsd: u64,
    #[serde(rename = "segRetHst", default)]
    pub seg_ret_hst: u64,
}

#[derive(Deserialize)]
pub struct ResendGxBody {
    pub whacrecod: String,
    pub sender: String,
    #[serde(default)]
    pub cantidad: i32,
    #[serde(rename = "segRetDsd", default)]
    pub seg_ret_dsd: u64,
    #[serde(rename = "segRetHst", default)]
    pub seg_ret_hst: u64,
    #[serde(default)]
    pub veces: i32,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/{session}/send-message-gx  [verify_token]
/// Fetches 'PEN' rows from Oracle and sends them in background.
pub async fn send_message_gx(
    Path(session): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<SendGxBody>,
) -> Response {
    let Some(session_ref) = state.sessions.get(&session).await else {
        return (StatusCode::BAD_REQUEST, "Session not found").into_response();
    };
    if *session_ref.status.read().await != SessionStatus::Connected {
        return (StatusCode::BAD_REQUEST, "Session not connected").into_response();
    }

    let whacrecod = body.whacrecod.clone();
    let sender = body.sender.clone();
    let oracle = state.oracle.clone();
    let seg_ret_dsd = body.seg_ret_dsd;
    let seg_ret_hst = body.seg_ret_hst;
    let cantidad = body.cantidad;

    tokio::spawn(async move {
        match oracle::get_pending_messages(oracle.clone(), whacrecod.clone(), sender, cantidad).await {
            Ok(rows) => {
                process_rows(rows, session_ref, oracle, whacrecod, seg_ret_dsd, seg_ret_hst, false).await;
            }
            Err(e) => tracing::error!("get_pending_messages: {e}"),
        }
    });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Los mensajes se enviarán en breve...",
    )
        .into_response()
}

/// POST /api/{session}/resend-message-gx  [verify_token]
/// Fetches 'ENV' rows filtered by `veces` and re-sends in background.
pub async fn resend_message_gx(
    Path(session): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<ResendGxBody>,
) -> Response {
    let Some(session_ref) = state.sessions.get(&session).await else {
        return (StatusCode::BAD_REQUEST, "Session not found").into_response();
    };
    if *session_ref.status.read().await != SessionStatus::Connected {
        return (StatusCode::BAD_REQUEST, "Session not connected").into_response();
    }

    let whacrecod = body.whacrecod.clone();
    let sender = body.sender.clone();
    let oracle = state.oracle.clone();
    let seg_ret_dsd = body.seg_ret_dsd;
    let seg_ret_hst = body.seg_ret_hst;
    let cantidad = body.cantidad;
    let veces = body.veces;

    tokio::spawn(async move {
        match oracle::get_resend_messages(oracle.clone(), whacrecod.clone(), sender, cantidad, veces).await {
            Ok(rows) => {
                process_rows(rows, session_ref, oracle, whacrecod, seg_ret_dsd, seg_ret_hst, true).await;
            }
            Err(e) => tracing::error!("get_resend_messages: {e}"),
        }
    });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Los mensajes se enviarán en breve...",
    )
        .into_response()
}

// ── Background processing ─────────────────────────────────────────────────────

async fn process_rows(
    rows: Vec<MsgRow>,
    session_ref: SessionRef,
    oracle: Arc<OracleConfig>,
    whacrecod: String,
    seg_ret_dsd: u64,
    seg_ret_hst: u64,
    is_resend: bool,
) {
    for row in rows {
        let phone = match format_phone(&row.numero) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Invalid phone '{}': {e}", row.numero);
                mark_status(&oracle, &whacrecod, row.secuencia, "FAL").await;
                continue;
            }
        };

        let jid: Jid = match phone.parse() {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("JID parse failed for '{phone}': {e}");
                mark_status(&oracle, &whacrecod, row.secuencia, "FAL").await;
                continue;
            }
        };

        let (text, img_opt) = pick_content(&row, is_resend);
        let result = if let Some(img) = img_opt {
            send_image_with_retry(&session_ref.client, &jid, img, &text, 2).await
        } else {
            send_text_with_retry(&session_ref.client, &jid, &text, 2).await
        };

        let estado = if result.is_ok() { "ENV" } else {
            if let Err(ref e) = result {
                tracing::error!("send failed (sec={}): {e}", row.secuencia);
            }
            "FAL"
        };
        mark_status(&oracle, &whacrecod, row.secuencia, estado).await;

        // Random inter-message delay
        if seg_ret_hst > 0 {
            let hi = seg_ret_hst.max(seg_ret_dsd);
            let delay = if hi > seg_ret_dsd {
                use rand::RngExt;
                rand::rng().random_range(seg_ret_dsd..=hi)
            } else {
                seg_ret_dsd
            };
            if delay > 0 {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
        }
    }
}

// ── Content selection (turno logic) ──────────────────────────────────────────

/// pick_content selects message text and optional image blob.
/// send: turno 0 → msg + img1
/// resend: turno 1 → msg2/img2 if flag set, else turno 2 → msg3/img3, else turno 0
fn pick_content(row: &MsgRow, is_resend: bool) -> (String, Option<Vec<u8>>) {
    if is_resend && row.msg2_chk == 1 {
        let img = if row.img2_chk == 1 { row.img2.clone() } else { None };
        (row.msg2.clone().unwrap_or_else(|| row.msg.clone()), img)
    } else if is_resend && row.msg3_chk == 1 {
        let img = if row.img3_chk == 1 { row.img3.clone() } else { None };
        (row.msg3.clone().unwrap_or_else(|| row.msg.clone()), img)
    } else {
        let img = if row.img1_chk == 1 { row.img1.clone() } else { None };
        (row.msg.clone(), img)
    }
}

// ── Phone formatting ──────────────────────────────────────────────────────────

fn format_phone(raw: &str) -> anyhow::Result<String> {
    // Strip @c.us or other suffixes
    let stripped = raw.trim().split('@').next().unwrap_or("").trim();
    let cleaned: String = stripped
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();

    if cleaned.is_empty() {
        return Err(anyhow::anyhow!("Empty phone after cleaning: '{raw}'"));
    }

    let for_parse = if cleaned.starts_with('+') {
        cleaned.clone()
    } else {
        format!("+{cleaned}")
    };

    let phone = phonenumber::parse(None, &for_parse)
        .map_err(|e| anyhow::anyhow!("Cannot parse '{raw}': {e}"))?;

    if !phonenumber::is_valid(&phone) {
        return Err(anyhow::anyhow!("Phone '{raw}' failed E.164 validation"));
    }

    let e164 = phone
        .format()
        .mode(phonenumber::Mode::E164)
        .to_string();
    let number = e164.trim_start_matches('+');

    Ok(format!("{number}@c.us"))
}

// ── Send helpers with retry ───────────────────────────────────────────────────

async fn send_text_with_retry(
    client: &Arc<Client>,
    jid: &Jid,
    text: &str,
    max_retries: u32,
) -> anyhow::Result<()> {
    let msg = wa::Message {
        conversation: Some(text.to_string()),
        ..Default::default()
    };
    for attempt in 0..=max_retries {
        match client.send_message(jid.clone(), msg.clone()).await {
            Ok(_) => return Ok(()),
            Err(e) if attempt < max_retries => {
                tracing::warn!("send_text attempt {attempt}: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

async fn send_image_with_retry(
    client: &Arc<Client>,
    jid: &Jid,
    blob: Vec<u8>,
    caption: &str,
    max_retries: u32,
) -> anyhow::Result<()> {
    let uploaded = client
        .upload(blob, MediaType::Image, UploadOptions::new())
        .await?;

    let msg = wa::Message {
        image_message: Some(Box::new(wa::message::ImageMessage {
            url: Some(uploaded.url.clone()),
            direct_path: Some(uploaded.direct_path.clone()),
            media_key: Some(uploaded.media_key_vec()),
            file_sha256: Some(uploaded.file_sha256_vec()),
            file_enc_sha256: Some(uploaded.file_enc_sha256_vec()),
            file_length: Some(uploaded.file_length),
            media_key_timestamp: Some(uploaded.media_key_timestamp),
            mimetype: Some("image/jpeg".to_string()),
            caption: if caption.is_empty() { None } else { Some(caption.to_string()) },
            ..Default::default()
        })),
        ..Default::default()
    };

    for attempt in 0..=max_retries {
        match client.send_message(jid.clone(), msg.clone()).await {
            Ok(_) => return Ok(()),
            Err(e) if attempt < max_retries => {
                tracing::warn!("send_image attempt {attempt}: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ── Oracle status update helper ───────────────────────────────────────────────

async fn mark_status(oracle: &Arc<OracleConfig>, whacrecod: &str, secuencia: i32, estado: &str) {
    if let Err(e) = oracle::update_msg_status(
        oracle.clone(),
        whacrecod.to_owned(),
        secuencia,
        estado.to_owned(),
    )
    .await
    {
        tracing::error!("update_msg_status '{estado}' sec={secuencia}: {e}");
    }
}
