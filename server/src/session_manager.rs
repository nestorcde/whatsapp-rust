use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};
use wacore::types::events::{Event, EventKind};
use whatsapp_rust::{
    Client, TokioRuntime,
    bot::{Bot, BotHandle},
    store::{Backend, SqliteStore},
};
use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
use whatsapp_rust_ureq_http_client::UreqHttpClient;

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
}

impl SessionManager {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            sessions_dir: sessions_dir.into(),
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
                ],
                move |event, _client| {
                    let status = Arc::clone(&status_ev);
                    let qr = Arc::clone(&qr_ev);
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
