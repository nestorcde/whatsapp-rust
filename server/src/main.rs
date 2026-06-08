mod config;
mod oracle;
mod routes;
mod session_manager;
mod state;

use std::{net::SocketAddr, sync::Arc};

use tracing::info;

use crate::{
    oracle::OracleConfig,
    session_manager::SessionManager,
    state::AppState,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "whatsapp_rust_server=info,tower_http=info".into()),
        )
        .init();

    let cfg = config::Config::from_env();

    let oracle = Arc::new(OracleConfig {
        user: cfg.db_user.clone(),
        password: cfg.db_pass.clone(),
        url: cfg.db_url.clone(),
    });

    let sessions = SessionManager::new(&cfg.sessions_dir);

    let state = AppState {
        sessions,
        oracle,
        config: Arc::new(cfg.clone()),
    };

    let app = routes::router(state);

    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port)
        .parse()
        .expect("invalid bind address");

    info!("whatsapp-rust-server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");

    axum::serve(listener, app).await.expect("server error");
}
