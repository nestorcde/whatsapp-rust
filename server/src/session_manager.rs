use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};
use wacore::types::events::{Event, EventKind};
use waproto::whatsapp as wa;
use whatsapp_rust::{
    Client, Server, TokioRuntime,
    bot::{Bot, BotHandle},
    store::{Backend, SqliteStore},
};
use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
use whatsapp_rust_ureq_http_client::UreqHttpClient;

use crate::oracle::{self, OracleConfig, Whatn002Row};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Closed,
    Starting,
    WaitingQr,
    Connected,
}

struct SessionEntry {
    client: Arc<Client>,
    status: Arc<RwLock<SessionStatus>>,
    qr_string: Arc<RwLock<Option<String>>>,
    _bot_handle: BotHandle,
}

/// Cloneable handle to a session's shared live state.
#[derive(Clone)]
pub struct SessionRef {
    pub client: Arc<Client>,
    pub status: Arc<RwLock<SessionStatus>>,
    pub qr_string: Arc<RwLock<Option<String>>>,
}

pub struct SessionManager {
    sessions: Mutex<HashMap<String, SessionEntry>>,
    sessions_dir: PathBuf,
    oracle: Arc<OracleConfig>,
}

impl SessionManager {
    pub fn new(sessions_dir: impl Into<PathBuf>, oracle: Arc<OracleConfig>) -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            sessions_dir: sessions_dir.into(),
            oracle,
        })
    }

    /// Returns the session's live state if it exists (regardless of status).
    pub async fn get(&self, name: &str) -> Option<SessionRef> {
        let guard = self.sessions.lock().await;
        guard.get(name).map(|e| SessionRef {
            client: Arc::clone(&e.client),
            status: Arc::clone(&e.status),
            qr_string: Arc::clone(&e.qr_string),
        })
    }

    pub async fn list_all(&self) -> Vec<String> {
        let guard = self.sessions.lock().await;
        guard.keys().cloned().collect()
    }

    /// Start a new session or return the existing one if already running.
    ///
    /// If a `Closed` entry exists it is replaced. Holding the mutex across
    /// `bot.build()` is intentional: SQLite migration is sub-second and
    /// `start` is an infrequent operation.
    pub async fn start(&self, name: &str) -> anyhow::Result<SessionRef> {
        let mut guard = self.sessions.lock().await;

        if let Some(entry) = guard.get(name) {
            let st = entry.status.read().await.clone();
            if !matches!(st, SessionStatus::Closed) {
                return Ok(SessionRef {
                    client: Arc::clone(&entry.client),
                    status: Arc::clone(&entry.status),
                    qr_string: Arc::clone(&entry.qr_string),
                });
            }
            guard.remove(name);
        }

        // Pre-allocate shared state so the event closure can capture it by
        // cloning before the Bot is built.
        let status = Arc::new(RwLock::new(SessionStatus::Starting));
        let qr_string: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

        let status_ev = Arc::clone(&status);
        let qr_ev = Arc::clone(&qr_string);
        let oracle_ev = Arc::clone(&self.oracle);
        let ev_name = name.to_owned();

        std::fs::create_dir_all(&self.sessions_dir)?;
        let db_path = self
            .sessions_dir
            .join(format!("{name}.db"))
            .to_string_lossy()
            .into_owned();

        let backend: Arc<dyn Backend> = Arc::new(
            SqliteStore::new(&db_path)
                .await
                .map_err(|e| anyhow::anyhow!("SQLite init failed for '{name}': {e}"))?,
        );

        let mut bot = Bot::builder()
            .with_backend(backend)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .skip_history_sync()
            .on_event_for(
                &[
                    EventKind::PairingQrCode,
                    EventKind::Connected,
                    EventKind::Disconnected,
                    EventKind::LoggedOut,
                    EventKind::Message,
                ],
                move |event, _client| {
                    let status = Arc::clone(&status_ev);
                    let qr = Arc::clone(&qr_ev);
                    let oracle = Arc::clone(&oracle_ev);
                    let ev_name = ev_name.clone();
                    async move {
                        match &*event {
                            Event::PairingQrCode { code, .. } => {
                                info!(session = %ev_name, "QR code received");
                                *status.write().await = SessionStatus::WaitingQr;
                                *qr.write().await = Some(code.clone());
                            }
                            Event::Connected(_) => {
                                info!(session = %ev_name, "connected");
                                *status.write().await = SessionStatus::Connected;
                                *qr.write().await = None;
                                let ofi = ev_name.clone();
                                let ts = format_oracle_now();
                                tokio::spawn(async move {
                                    let row = Whatn002Row {
                                        ofi,
                                        nro_origen: String::new(),
                                        nro_dest: String::new(),
                                        mensaje: String::new(),
                                        fecha_hora: ts,
                                        propio: 0,
                                        tipo: "INI".to_string(),
                                        has_media: 0,
                                        mimetype: String::new(),
                                        mediadata: String::new(),
                                    };
                                    if let Err(e) = oracle::insert_whatn002(oracle, row).await {
                                        tracing::warn!("WHATN002 INI: {e}");
                                    }
                                });
                            }
                            Event::Disconnected(_) => {
                                // Client may auto-reconnect; mark as Starting rather than Closed.
                                warn!(session = %ev_name, "disconnected (may reconnect)");
                                *status.write().await = SessionStatus::Starting;
                            }
                            Event::LoggedOut(_) => {
                                warn!(session = %ev_name, "logged out — session must be re-paired");
                                *status.write().await = SessionStatus::Closed;
                                *qr.write().await = None;
                                let ofi = ev_name.clone();
                                let ts = format_oracle_now();
                                tokio::spawn(async move {
                                    let row = Whatn002Row {
                                        ofi,
                                        nro_origen: String::new(),
                                        nro_dest: String::new(),
                                        mensaje: "SESSION CERRADA POR EL OFICIAL!!".to_string(),
                                        fecha_hora: ts,
                                        propio: 0,
                                        tipo: "DES".to_string(),
                                        has_media: 0,
                                        mimetype: String::new(),
                                        mediadata: String::new(),
                                    };
                                    if let Err(e) = oracle::insert_whatn002(oracle, row).await {
                                        tracing::warn!("WHATN002 DES: {e}");
                                    }
                                });
                            }
                            Event::Message(msg, info) => {
                                // Skip group and broadcast (status@broadcast) messages.
                                if info.source.is_group
                                    || info.source.chat.server == Server::Broadcast
                                {
                                    return;
                                }
                                let ofi = ev_name.clone();
                                let nro_origen =
                                    format!("+{}", info.source.sender.user.as_str());
                                let nro_dest =
                                    format!("+{}", info.source.chat.user.as_str());
                                let propio: i32 = if info.source.is_from_me { 1 } else { 0 };
                                let fecha_hora = info
                                    .timestamp
                                    .format("%Y-%m-%d %H:%M:%S")
                                    .to_string();
                                let (body, has_media, mimetype) =
                                    extract_msg_content(msg);
                                let nro_origen2 = nro_origen.clone();
                                let nro_dest2 = nro_dest.clone();
                                tokio::spawn(async move {
                                    let row = Whatn002Row {
                                        ofi: ofi.clone(),
                                        nro_origen: nro_origen.clone(),
                                        nro_dest: nro_dest.clone(),
                                        mensaje: body,
                                        fecha_hora,
                                        propio,
                                        tipo: "MSG".to_string(),
                                        has_media: if has_media { 1 } else { 0 },
                                        mimetype,
                                        mediadata: String::new(),
                                    };
                                    if let Err(e) =
                                        oracle::insert_whatn002(oracle.clone(), row).await
                                    {
                                        tracing::warn!("WHATN002 MSG: {e}");
                                        return;
                                    }
                                    // Increment resend counter for the matching ENV row.
                                    let numero_cliente =
                                        if propio == 1 { nro_dest2 } else { nro_origen2 };
                                    if let Err(e) = oracle::increment_whacrecntint(
                                        oracle,
                                        ofi,
                                        numero_cliente,
                                    )
                                    .await
                                    {
                                        tracing::debug!("WHACRECNTINT: {e}");
                                    }
                                });
                            }
                            _ => {}
                        }
                    }
                },
            )
            .build()
            .await
            .map_err(|e| anyhow::anyhow!("Bot build failed for '{name}': {e}"))?;

        let client = bot.client();
        let bot_handle = bot
            .run()
            .await
            .map_err(|e| anyhow::anyhow!("Bot run failed for '{name}': {e}"))?;

        guard.insert(
            name.to_owned(),
            SessionEntry {
                client: Arc::clone(&client),
                status: Arc::clone(&status),
                qr_string: Arc::clone(&qr_string),
                _bot_handle: bot_handle,
            },
        );

        Ok(SessionRef { client, status, qr_string })
    }
}

fn format_oracle_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Returns (body, has_media, mimetype) from a WhatsApp message.
fn extract_msg_content(msg: &wa::Message) -> (String, bool, String) {
    if let Some(img) = msg.image_message.as_deref() {
        let caption = img.caption.as_deref().unwrap_or("").to_string();
        let mime = img.mimetype.as_deref().unwrap_or("image/jpeg").to_string();
        return (caption, true, mime);
    }
    if let Some(vid) = msg.video_message.as_deref() {
        let caption = vid.caption.as_deref().unwrap_or("").to_string();
        let mime = vid.mimetype.as_deref().unwrap_or("video/mp4").to_string();
        return (caption, true, mime);
    }
    if let Some(aud) = msg.audio_message.as_deref() {
        let mime = aud.mimetype.as_deref().unwrap_or("audio/mpeg").to_string();
        return (String::new(), true, mime);
    }
    if let Some(doc) = msg.document_message.as_deref() {
        let mime = doc.mimetype.as_deref().unwrap_or("application/octet-stream").to_string();
        return (String::new(), true, mime);
    }
    if msg.sticker_message.is_some() {
        return (String::new(), true, "image/webp".to_string());
    }
    let body = msg
        .conversation
        .as_deref()
        .or_else(|| {
            msg.extended_text_message
                .as_ref()
                .and_then(|e| e.text.as_deref())
        })
        .unwrap_or("")
        .chars()
        .take(3999)
        .collect();
    (body, false, String::new())
}
