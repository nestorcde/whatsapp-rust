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
    config::Config,
    google::{auth as gauth, people},
    oracle::{self, MsgRow, OracleConfig},
    session_manager::{SessionRef, SessionStatus},
    state::AppState,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

// Genexus sends whacrecod as a bare integer (no quotes). Accept both.
fn deser_string_or_int<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Err(D::Error::custom("expected string or number for whacrecod")),
    }
}

// ── Request bodies ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SendGxBody {
    #[serde(deserialize_with = "deser_string_or_int")]
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
    #[serde(deserialize_with = "deser_string_or_int")]
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
    let config = state.config.clone();
    let seg_ret_dsd = body.seg_ret_dsd;
    let seg_ret_hst = body.seg_ret_hst;
    let cantidad = body.cantidad;

    tokio::spawn(async move {
        tracing::info!("WhaCreCod: {} - WhaCreOfi: {} - Cantidad: {} - segRetDsd: {} - segRetHst: {}",
                       whacrecod, sender, cantidad, seg_ret_dsd, seg_ret_hst);
        match oracle::get_pending_messages(oracle.clone(), whacrecod.clone(), sender.clone(), cantidad).await {
            Ok(rows) => {
                process_rows(rows, session_ref, oracle, config, whacrecod, sender, seg_ret_dsd, seg_ret_hst, false).await;
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
    let config = state.config.clone();
    let seg_ret_dsd = body.seg_ret_dsd;
    let seg_ret_hst = body.seg_ret_hst;
    let cantidad = body.cantidad;
    let veces = body.veces;

    tokio::spawn(async move {
        tracing::info!("WhaCreCod: {} - WhaCreOfi: {} - Cantidad: {} - segRetDsd: {} - segRetHst: {} - Veces: {}",
                       whacrecod, sender, cantidad, seg_ret_dsd, seg_ret_hst, veces);
        match oracle::get_resend_messages(oracle.clone(), whacrecod.clone(), sender.clone(), cantidad, veces).await {
            Ok(rows) => {
                process_rows(rows, session_ref, oracle, config, whacrecod, sender, seg_ret_dsd, seg_ret_hst, true).await;
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
    config: Arc<Config>,
    whacrecod: String,
    sender: String,
    seg_ret_dsd: u64,
    seg_ret_hst: u64,
    _is_resend: bool,
) {
    let mut turno: u8 = 0;
    for row in rows {
        tracing::info!("MSG: {} - NUMERO: {} - INSTANCIA: {}", row.msg, row.numero, sender);

        let phone = match format_phone(&row.numero) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Invalid phone '{}': {e}", row.numero);
                mark_status(&oracle, &whacrecod, row.secuencia, "FAL").await;
                continue;
            }
        };

        // Auto-register contact in Google Contacts (fire-and-forget).
        {
            let cfg = config.clone();
            let e164 = format!("+{}", phone.trim_end_matches("@c.us"));
            let name = match row.socnro {
                Some(v) if v > 0.0 => format!("{} - {}", row.nombre, format_socnro(v)),
                _ => row.nombre.clone(),
            };
            tokio::spawn(async move {
                let creds = gauth::credentials_from_config(&cfg);
                match gauth::get_valid_token_with_creds(&cfg.google_contacts_token_dir, "default", &creds).await {
                    Ok(token) => {
                        if let Err(e) = people::upsert_contact(&token, &name, &e164).await {
                            tracing::debug!("google upsert '{}': {e}", e164);
                        }
                    }
                    Err(e) => tracing::debug!("google token unavailable: {e}"),
                }
            });
        }

        let jid: Jid = match phone.parse() {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("JID parse failed for '{phone}': {e}");
                mark_status(&oracle, &whacrecod, row.secuencia, "FAL").await;
                continue;
            }
        };

        let (text, img_opt) = pick_content(&row, &mut turno);
        let con_imagen = img_opt.is_some();
        tracing::info!("[DEBUG] Justo antes de enviar - phonenumber: \"{phone}\", con_imagen: {con_imagen}, secuencia: {}", row.secuencia);
        let result = if let Some(img) = img_opt {
            send_image_with_retry(&session_ref.client, &jid, img, &text, 2).await
        } else {
            send_text_with_retry(&session_ref.client, &jid, &text, 2).await
        };

        let estado = if result.is_ok() {
            tracing::info!("✓ Mensaje enviado exitosamente a {phone}");
            "ENV"
        } else {
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
                tracing::info!("El siguiente mensaje se enviará después de {delay} segundos");
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
        }
    }
}

// ── Content selection (turno logic) ──────────────────────────────────────────

/// Mirrors the original turno rotation: `turno` persists across the whole batch
/// so each successive contact gets the next message variant (msg1→msg2→msg3→msg1…).
/// If neither MSG2CHK nor MSG3CHK is set, msg1 is always used regardless of turno.
fn pick_content(row: &MsgRow, turno: &mut u8) -> (String, Option<Vec<u8>>) {
    if row.msg2_chk != 1 && row.msg3_chk != 1 {
        let img = if row.img1_chk == 1 { row.img1.clone() } else { None };
        return (row.msg.clone(), img);
    }

    match *turno {
        0 => {
            let img = if row.img1_chk == 1 { row.img1.clone() } else { None };
            *turno = 1;
            (row.msg.clone(), img)
        }
        1 => {
            if row.msg2_chk == 1 {
                let img = if row.img2_chk == 1 { row.img2.clone() } else { None };
                *turno = if row.msg3_chk == 1 { 2 } else { 0 };
                (row.msg2.clone().unwrap_or_else(|| row.msg.clone()), img)
            } else {
                // msg2 not enabled, skip to msg3
                let img = if row.img3_chk == 1 { row.img3.clone() } else { None };
                *turno = 0;
                (row.msg3.clone().unwrap_or_else(|| row.msg.clone()), img)
            }
        }
        _ => {
            // turno == 2
            let img = if row.img3_chk == 1 { row.img3.clone() } else { None };
            *turno = 0;
            (row.msg3.clone().unwrap_or_else(|| row.msg.clone()), img)
        }
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

// ── Socnro formatting ─────────────────────────────────────────────────────────

/// Formats WHACRESOCNRO (e.g. 322620) as "32-262/0".
/// Rule: integer DD_DDD_D → "DD-DDD/D".
fn format_socnro(val: f64) -> String {
    let n = val.round() as u64;
    format!("{:02}-{:03}/{}", n / 10000, (n % 10000) / 10, n % 10)
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
