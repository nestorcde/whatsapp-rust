use std::env;

/// Runtime configuration loaded from environment variables (and optional .env file).
///
/// All fields are validated at startup so the server fails fast instead of
/// panicking on the first request that needs a missing value.
#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP port to bind (default: 21465, matching the wppconnect default)
    pub port: u16,
    /// Master secret used to verify/generate session tokens
    pub secret_key: String,
    /// Base directory where per-session SQLite databases are stored
    pub sessions_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();

        let port = env::var("PORT")
            .unwrap_or_else(|_| "21465".to_string())
            .parse::<u16>()
            .expect("PORT must be a valid port number (0-65535)");

        let secret_key =
            env::var("SECRET_KEY").expect("SECRET_KEY environment variable is required");

        let sessions_dir = env::var("SESSIONS_DIR").unwrap_or_else(|_| "./sessions".to_string());

        Self {
            port,
            secret_key,
            sessions_dir,
        }
    }
}
