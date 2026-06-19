use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_in: u64,
    pub obtained_at: u64,
}

impl Token {
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now >= self.obtained_at + self.expires_in.saturating_sub(60)
    }
}

// ── File path helpers ─────────────────────────────────────────────────────────

fn user_dir(token_dir: &str, user_key: &str) -> PathBuf {
    PathBuf::from(token_dir).join(user_key)
}

pub fn credentials_path(token_dir: &str, user_key: &str) -> PathBuf {
    user_dir(token_dir, user_key).join("credentials.json")
}

pub fn token_path(token_dir: &str, user_key: &str) -> PathBuf {
    user_dir(token_dir, user_key).join("token.json")
}

// ── File I/O ──────────────────────────────────────────────────────────────────

pub fn load_credentials(token_dir: &str, user_key: &str) -> Result<Credentials> {
    let content = std::fs::read_to_string(credentials_path(token_dir, user_key))
        .with_context(|| format!("Credentials not found for '{user_key}'"))?;
    serde_json::from_str(&content).context("Invalid credentials.json")
}

pub fn save_credentials(token_dir: &str, user_key: &str, creds: &Credentials) -> Result<()> {
    std::fs::create_dir_all(user_dir(token_dir, user_key))?;
    let content = serde_json::to_string_pretty(creds)?;
    std::fs::write(credentials_path(token_dir, user_key), content)?;
    Ok(())
}

pub fn delete_credentials_dir(token_dir: &str, user_key: &str) -> Result<()> {
    let dir = user_dir(token_dir, user_key);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

pub fn load_token(token_dir: &str, user_key: &str) -> Result<Token> {
    let content = std::fs::read_to_string(token_path(token_dir, user_key))
        .with_context(|| format!("Token not found for '{user_key}'"))?;
    serde_json::from_str(&content).context("Invalid token.json")
}

pub fn save_token(token_dir: &str, user_key: &str, token: &Token) -> Result<()> {
    std::fs::create_dir_all(user_dir(token_dir, user_key))?;
    let content = serde_json::to_string_pretty(token)?;
    std::fs::write(token_path(token_dir, user_key), content)?;
    Ok(())
}

pub fn delete_token(token_dir: &str, user_key: &str) -> Result<()> {
    let path = token_path(token_dir, user_key);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// List all userKeys that have a credentials.json under token_dir.
pub fn list_users(token_dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(token_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| e.path().join("credentials.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

// ── OAuth2 URL + token exchange ───────────────────────────────────────────────

pub fn build_auth_url(creds: &Credentials) -> String {
    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .expect("static URL is valid");
    url.query_pairs_mut()
        .append_pair("client_id", &creds.client_id)
        .append_pair("redirect_uri", &creds.redirect_uri)
        .append_pair("scope", "https://www.googleapis.com/auth/contacts")
        .append_pair("response_type", "code")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    url.to_string()
}

pub async fn exchange_code(creds: &Credentials, code: &str) -> Result<Token> {
    let client = reqwest::Client::new();
    let params = [
        ("code", code),
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("redirect_uri", creds.redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];
    let value: serde_json::Value = client
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    token_from_value(value, None)
}

pub async fn refresh_access_token(creds: &Credentials, token: &Token) -> Result<Token> {
    let refresh = token
        .refresh_token
        .as_deref()
        .context("No refresh token available")?;
    let client = reqwest::Client::new();
    let params = [
        ("refresh_token", refresh),
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("grant_type", "refresh_token"),
    ];
    let value: serde_json::Value = client
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    token_from_value(value, token.refresh_token.clone())
}

fn token_from_value(v: serde_json::Value, existing_refresh: Option<String>) -> Result<Token> {
    let obtained_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let refresh_token = v["refresh_token"]
        .as_str()
        .map(|s| s.to_string())
        .or(existing_refresh);
    Ok(Token {
        access_token: v["access_token"]
            .as_str()
            .context("missing access_token")?
            .to_string(),
        refresh_token,
        token_type: v["token_type"].as_str().unwrap_or("Bearer").to_string(),
        expires_in: v["expires_in"].as_u64().unwrap_or(3600),
        obtained_at,
    })
}

/// Load token for userKey, refreshing it if expired. Persists the refreshed token.
pub async fn get_valid_token(token_dir: &str, user_key: &str) -> Result<Token> {
    let creds = load_credentials(token_dir, user_key)?;
    let mut token = load_token(token_dir, user_key)?;
    if token.is_expired() {
        token = refresh_access_token(&creds, &token).await?;
        save_token(token_dir, user_key, &token)?;
    }
    Ok(token)
}

/// Build Credentials from environment config (no file needed).
pub fn credentials_from_config(config: &Config) -> Credentials {
    Credentials {
        client_id: config.google_client_id.clone(),
        client_secret: config.google_client_secret.clone(),
        redirect_uri: config.google_redirect_uri.clone(),
    }
}

/// Like get_valid_token but uses credentials from config instead of from file.
pub async fn get_valid_token_with_creds(token_dir: &str, user_key: &str, creds: &Credentials) -> Result<Token> {
    let mut token = load_token(token_dir, user_key)?;
    if token.is_expired() {
        token = refresh_access_token(creds, &token).await?;
        save_token(token_dir, user_key, &token)?;
    }
    Ok(token)
}
