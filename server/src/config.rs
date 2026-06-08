use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP port to bind (default: 21465, matching the wppconnect default)
    pub port: u16,
    /// Master secret used to verify/generate session tokens
    pub secret_key: String,
    /// Base directory where per-session SQLite databases are stored
    pub sessions_dir: String,
    /// Oracle DB username
    pub db_user: String,
    /// Oracle DB password
    pub db_pass: String,
    /// Oracle DB connect string (e.g. `//host:1521/service`)
    pub db_url: String,
    /// Secret key for Google Contacts API endpoints (plain comparison)
    pub google_contacts_secret_key: String,
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

        let db_user = env::var("DB_USER").unwrap_or_default();
        let db_pass = env::var("DB_PASS").unwrap_or_default();
        let db_url = env::var("DB_URL").unwrap_or_default();
        let google_contacts_secret_key =
            env::var("GOOGLE_CONTACTS_SECRET_KEY").unwrap_or_default();

        Self { port, secret_key, sessions_dir, db_user, db_pass, db_url, google_contacts_secret_key }
    }
}
