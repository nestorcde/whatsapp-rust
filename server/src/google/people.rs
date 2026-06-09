use anyhow::{Context, Result};
use serde_json::Value;

use super::auth::Token;

const PEOPLE_API: &str = "https://people.googleapis.com/v1";

// ── Contact types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ContactInfo {
    pub resource_name: String,
    pub display_name: String,
    pub phones: Vec<String>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn normalize_phone(raw: &str) -> Option<String> {
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
    Some(
        phone
            .format()
            .mode(phonenumber::Mode::E164)
            .to_string(),
    )
}

fn contact_from_value(conn: &Value) -> Option<ContactInfo> {
    let resource_name = conn["resourceName"].as_str()?.to_string();
    let display_name = conn["names"]
        .as_array()
        .and_then(|n| n.first())
        .and_then(|n| n["displayName"].as_str())
        .unwrap_or("")
        .to_string();
    let phones = conn["phoneNumbers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    p["canonicalForm"]
                        .as_str()
                        .or_else(|| p["value"].as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ContactInfo { resource_name, display_name, phones })
}

// ── API calls ─────────────────────────────────────────────────────────────────

/// Fetch all connections, paginating as needed. Returns a flat list of ContactInfo.
pub async fn list_all_contacts(token: &Token) -> Result<Vec<ContactInfo>> {
    let client = reqwest::Client::new();
    let mut contacts = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut url = reqwest::Url::parse(&format!(
            "{PEOPLE_API}/people/me/connections?personFields=phoneNumbers,names&pageSize=1000"
        ))
        .unwrap();
        if let Some(ref pt) = page_token {
            url.query_pairs_mut().append_pair("pageToken", pt);
        }

        let data: Value = client
            .get(url)
            .bearer_auth(&token.access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if let Some(connections) = data["connections"].as_array() {
            for conn in connections {
                if let Some(info) = contact_from_value(conn) {
                    contacts.push(info);
                }
            }
        }

        match data["nextPageToken"].as_str() {
            Some(pt) => page_token = Some(pt.to_string()),
            None => break,
        }
    }

    Ok(contacts)
}

/// Return all E.164 phone numbers present in Google Contacts (normalized).
pub async fn list_all_phones_normalized(token: &Token) -> Result<Vec<String>> {
    let contacts = list_all_contacts(token).await?;
    Ok(contacts
        .into_iter()
        .flat_map(|c| c.phones)
        .filter_map(|p| normalize_phone(&p))
        .collect())
}

/// Search for a contact by phone number. Returns the first match or None.
pub async fn find_contact_by_phone(token: &Token, phone: &str) -> Result<Option<ContactInfo>> {
    let normalized = normalize_phone(phone).context("Invalid phone number")?;
    let contacts = list_all_contacts(token).await?;
    let found = contacts.into_iter().find(|c| {
        c.phones
            .iter()
            .filter_map(|p| normalize_phone(p))
            .any(|p| p == normalized)
    });
    Ok(found)
}

/// Create contact only if no existing contact has the same E.164 phone number.
pub async fn upsert_contact(token: &Token, name: &str, phone: &str) -> Result<()> {
    let normalized = normalize_phone(phone).context("Invalid phone")?;
    let contacts = list_all_contacts(token).await?;
    let exists = contacts.iter().any(|c| {
        c.phones
            .iter()
            .filter_map(|p| normalize_phone(p))
            .any(|p| p == normalized)
    });
    if !exists {
        create_contact(token, name, &normalized).await?;
    }
    Ok(())
}

/// Create a new Google Contact.
pub async fn create_contact(token: &Token, name: &str, phone: &str) -> Result<Value> {
    let client = reqwest::Client::new();
    let mut body = serde_json::json!({ "phoneNumbers": [{ "value": phone }] });
    if !name.is_empty() {
        body["names"] = serde_json::json!([{ "displayName": name }]);
    }
    let data: Value = client
        .post(format!("{PEOPLE_API}/people:createContact"))
        .bearer_auth(&token.access_token)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(data)
}
