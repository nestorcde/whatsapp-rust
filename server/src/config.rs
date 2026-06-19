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
    /// OpenAI API key for message generation/rewriting (validate-lexical-similarity)
    pub openai_api_key: String,
    /// OpenAI model name (default: gpt-3.5-turbo)
    pub openai_model: String,
    /// Directory for storing per-userKey Google OAuth2 credentials and tokens
    pub google_contacts_token_dir: String,
    /// Google OAuth2 client ID (from Google Cloud Console)
    pub google_client_id: String,
    /// Google OAuth2 client secret (from Google Cloud Console)
    pub google_client_secret: String,
    /// Google OAuth2 redirect URI (must match Google Cloud Console)
    pub google_redirect_uri: String,
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
        let openai_api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        let openai_model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-3.5-turbo".to_string());
        let google_contacts_token_dir =
            env::var("GOOGLE_CONTACTS_TOKEN_DIR").unwrap_or_else(|_| "./google-tokens".to_string());
        let google_client_id = env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
        let google_client_secret = env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default();
        let google_redirect_uri = env::var("GOOGLE_REDIRECT_URI").unwrap_or_default();

        Self { port, secret_key, sessions_dir, db_user, db_pass, db_url, google_contacts_secret_key, openai_api_key, openai_model, google_contacts_token_dir, google_client_id, google_client_secret, google_redirect_uri }
    }
}
