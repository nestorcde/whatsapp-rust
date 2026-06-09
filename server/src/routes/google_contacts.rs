use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    google::{auth, people},
    state::AppState,
};

// ── Credentials ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CredentialsBody {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

/// POST /api/google-contacts/credentials/{userKey}
pub async fn store_credentials(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<CredentialsBody>,
) -> Response {
    let creds = auth::Credentials {
        client_id: body.client_id,
        client_secret: body.client_secret,
        redirect_uri: body.redirect_uri,
    };
    match auth::save_credentials(&state.config.google_contacts_token_dir, &user_key, &creds) {
        Ok(_) => Json(json!({ "status": "ok", "userKey": user_key })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/google-contacts/credentials/{userKey}/status
pub async fn credentials_status(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let exists = auth::credentials_path(&state.config.google_contacts_token_dir, &user_key).exists();
    Json(json!({ "userKey": user_key, "hasCredentials": exists })).into_response()
}

/// DELETE /api/google-contacts/credentials/{userKey}
pub async fn delete_credentials(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
) -> Response {
    match auth::delete_credentials_dir(&state.config.google_contacts_token_dir, &user_key) {
        Ok(_) => Json(json!({ "status": "deleted", "userKey": user_key })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── OAuth2 flow ───────────────────────────────────────────────────────────────

/// POST /api/google-contacts/auth/start/{userKey}
/// Returns the Google OAuth2 authorization URL.
pub async fn auth_start(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let token_dir = &state.config.google_contacts_token_dir;
    let creds = match auth::load_credentials(token_dir, &user_key) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let url = auth::build_auth_url(&creds);
    Json(json!({ "authUrl": url })).into_response()
}

#[derive(Deserialize)]
pub struct CallbackBody {
    pub code: String,
}

/// POST /api/google-contacts/auth/callback/{userKey}
/// Exchanges the authorization code for tokens and saves them to disk.
pub async fn auth_callback(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<CallbackBody>,
) -> Response {
    let token_dir = &state.config.google_contacts_token_dir;
    let creds = match auth::load_credentials(token_dir, &user_key) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    match auth::exchange_code(&creds, &body.code).await {
        Ok(token) => match auth::save_token(token_dir, &user_key, &token) {
            Ok(_) => Json(json!({ "status": "authenticated", "userKey": user_key })).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(e) => (StatusCode::BAD_REQUEST, format!("Token exchange failed: {e}")).into_response(),
    }
}

/// GET /api/google-contacts/auth/status/{userKey}
pub async fn auth_status(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let token_dir = &state.config.google_contacts_token_dir;
    let token_exists = auth::token_path(token_dir, &user_key).exists();
    let expired = if token_exists {
        auth::load_token(token_dir, &user_key)
            .map(|t| t.is_expired())
            .unwrap_or(true)
    } else {
        false
    };
    Json(json!({
        "userKey": user_key,
        "authenticated": token_exists,
        "tokenExpired": expired,
    }))
    .into_response()
}

/// DELETE /api/google-contacts/auth/revoke/{userKey}
pub async fn auth_revoke(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
) -> Response {
    match auth::delete_token(&state.config.google_contacts_token_dir, &user_key) {
        Ok(_) => Json(json!({ "status": "revoked", "userKey": user_key })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Users ─────────────────────────────────────────────────────────────────────

/// GET /api/google-contacts/users
pub async fn list_users(State(state): State<AppState>) -> Response {
    let users = auth::list_users(&state.config.google_contacts_token_dir);
    Json(json!({ "users": users })).into_response()
}

// ── Contacts ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchBody {
    pub phone: String,
}

/// POST /api/google-contacts/{userKey}/search
pub async fn search_contact(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<SearchBody>,
) -> Response {
    let token_dir = &state.config.google_contacts_token_dir;
    let token = match auth::get_valid_token(token_dir, &user_key).await {
        Ok(t) => t,
        Err(e) => return (StatusCode::UNAUTHORIZED, e.to_string()).into_response(),
    };
    match people::find_contact_by_phone(&token, &body.phone).await {
        Ok(Some(contact)) => Json(json!({
            "found": true,
            "contact": {
                "resourceName": contact.resource_name,
                "displayName": contact.display_name,
                "phones": contact.phones,
            }
        }))
        .into_response(),
        Ok(None) => Json(json!({ "found": false, "contact": null })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct ContactInput {
    pub name: String,
    pub phone: String,
}

#[derive(Deserialize)]
pub struct UpsertBody {
    pub contacts: Vec<ContactInput>,
}

/// POST /api/google-contacts/{userKey}/upsert
/// Prefetches all existing phone numbers, then creates only missing contacts.
pub async fn upsert_contacts(
    Path(user_key): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<UpsertBody>,
) -> Response {
    let token_dir = &state.config.google_contacts_token_dir;
    let token = match auth::get_valid_token(token_dir, &user_key).await {
        Ok(t) => t,
        Err(e) => return (StatusCode::UNAUTHORIZED, e.to_string()).into_response(),
    };

    // Prefetch all existing normalized phone numbers
    let existing_phones = match people::list_all_phones_normalized(&token).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut created = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;

    for contact in &body.contacts {
        // Normalize input phone
        let normalized = match normalize_e164(&contact.phone) {
            Some(p) => p,
            None => {
                tracing::warn!("upsert: invalid phone '{}'", contact.phone);
                errors += 1;
                continue;
            }
        };

        if existing_phones.contains(&normalized) {
            skipped += 1;
            continue;
        }

        match people::create_contact(&token, &contact.name, &normalized).await {
            Ok(_) => created += 1,
            Err(e) => {
                tracing::error!("create_contact '{}': {e}", contact.phone);
                errors += 1;
            }
        }
    }

    Json(json!({ "created": created, "skipped": skipped, "errors": errors })).into_response()
}

fn normalize_e164(raw: &str) -> Option<String> {
    let stripped = raw.trim().split('@').next()?.trim();
    let cleaned: String = stripped
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let for_parse = if cleaned.starts_with('+') {
        cleaned.clone()
    } else {
        format!("+{cleaned}")
    };
    let phone = phonenumber::parse(None, &for_parse).ok()?;
    if !phonenumber::is_valid(&phone) {
        return None;
    }
    Some(phone.format().mode(phonenumber::Mode::E164).to_string())
}
