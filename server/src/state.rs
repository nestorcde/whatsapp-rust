use std::sync::Arc;

use crate::{config::Config, oracle::OracleConfig, session_manager::SessionManager};

/// Shared application state injected into every Axum handler via `State<AppState>`.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<SessionManager>,
    pub oracle: Arc<OracleConfig>,
    pub config: Arc<Config>,
}
