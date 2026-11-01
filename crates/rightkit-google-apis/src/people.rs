mod contacts;
pub use contacts::*;
mod groups;
pub use groups::*;
mod recipients;
pub use recipients::*;
// Google People API client. Builds on the shared `google` client for auth,
// retry, and backoff — this module only knows People API endpoints and how to
// resolve a sender email address to a ContactCard for the reader sender card,
// and to search across all three People scopes for compose recipient autocomplete.
//
// Backlog item #11 (sender intelligence, Plus tier). READ-ONLY.
//
// Scopes required (already authorized in OAuth flow):
//   contacts.readonly          — user's personal contacts
//   contacts.other.readonly    — "Other contacts" (auto-populated from email history)
//   directory.readonly         — Workspace directory (org members)
//
// API quirk: searchContacts may return empty on the first call with a fresh
// client (cache warm-up). We fall back to otherContacts:search automatically.
// Both endpoints return: { "results": [ { "person": { ...PersonFields } } ] }

use crate::google::{Google, GoogleError};
use serde::Deserialize;
use std::collections::HashMap;

const SEARCH_CONTACTS_URL: &str = "https://people.googleapis.com/v1/people:searchContacts";
const SEARCH_OTHER_CONTACTS_URL: &str = "https://people.googleapis.com/v1/otherContacts:search";
const SEARCH_DIRECTORY_URL: &str = "https://people.googleapis.com/v1/people:searchDirectory";

// ---------------------------------------------------------------------------
// Public output type
// ---------------------------------------------------------------------------

/// Resolved contact card for a sender email. All fields default to empty
/// string when the API omits them — never None, always renderable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContactCard {
    pub display_name: String,
    pub email: String,
    pub photo_url: String,
    pub org_title: String,
}

// ---------------------------------------------------------------------------
// People API response shapes (private, deserialization only)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersonName {
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmailAddress {
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Photo {
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Organization {
    #[serde(default)]
    title: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Person {
    #[serde(default)]
    names: Vec<PersonName>,
    #[serde(default)]
    email_addresses: Vec<EmailAddress>,
    #[serde(default)]
    photos: Vec<Photo>,
    #[serde(default)]
    organizations: Vec<Organization>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(default)]
    person: Option<Person>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResp {
    #[serde(default)]
    results: Vec<SearchResult>,
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

/// Resolve an email address to a ContactCard using the People API.
///
/// Strategy:
///   1. Query `people:searchContacts` (personal contacts).
///   2. If that returns nothing (cache warm-up quirk or not in contacts),
///      fall back to `otherContacts:search` (auto-populated from email history).
///   3. Parse the first result's Person into ContactCard.
///   4. Any 4xx response (except auth errors propagated by the shared client)
///      is treated as `Ok(None)` — sender card is best-effort, never a blocker.
pub async fn lookup_contact(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    email: &str,
) -> Result<Option<ContactCard>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    // 1. Try personal contacts first.
    let contacts_query = [
        ("query", email.to_string()),
        (
            "readMask",
            "names,emailAddresses,photos,organizations".to_string(),
        ),
    ];
    let contacts_resp: Option<SearchResp> =
        match g.get_json(SEARCH_CONTACTS_URL, &contacts_query).await {
            Ok(r) => Some(r),
            Err(GoogleError::Http { status, .. }) if (400..500).contains(&status) => None,
            Err(e) => return Err(String::from(e)),
        };

    if let Some(card) = contacts_resp
        .as_ref()
        .and_then(|r| first_match(&r.results, email))
    {
        return Ok(Some(card));
    }

    // 2. Fall back to other contacts (auto-populated from email history).
    // otherContacts:search does not support `organizations` in readMask.
    let other_query = [
        ("query", email.to_string()),
        ("readMask", "names,emailAddresses,photos".to_string()),
    ];
    let other_resp: Option<SearchResp> =
        match g.get_json(SEARCH_OTHER_CONTACTS_URL, &other_query).await {
            Ok(r) => Some(r),
            Err(GoogleError::Http { status, .. }) if (400..500).contains(&status) => None,
            Err(e) => return Err(String::from(e)),
        };

    if let Some(card) = other_resp
        .as_ref()
        .and_then(|r| first_match(&r.results, email))
    {
        return Ok(Some(card));
    }

    Ok(None)
}

/// Extract a ContactCard from the first usable result in a results slice.
/// `queried_email` is matched case-insensitively; falls back to [0] if absent.
fn first_match(results: &[SearchResult], queried_email: &str) -> Option<ContactCard> {
    let person = results.iter().find_map(|r| r.person.as_ref())?;

    let display_name = person
        .names
        .first()
        .map(|n| n.display_name.clone())
        .unwrap_or_default();

    // Prefer the address that matches the query; fall back to first.
    let email = person
        .email_addresses
        .iter()
        .find(|e| e.value.to_lowercase() == queried_email.to_lowercase())
        .or_else(|| person.email_addresses.first())
        .map(|e| e.value.clone())
        .unwrap_or_default();

    let photo_url = person
        .photos
        .first()
        .map(|p| p.url.clone())
        .unwrap_or_default();

    // org.title preferred; fall back to org.name (Workspace directory often
    // only populates name).
    let org_title = person
        .organizations
        .first()
        .map(|o| {
            if !o.title.is_empty() {
                o.title.clone()
            } else {
                o.name.clone()
            }
        })
        .unwrap_or_default();

    Some(ContactCard {
        display_name,
        email,
        photo_url,
        org_title,
    })
}

// ---------------------------------------------------------------------------
// Tests (offline JSON parse only — no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
// ---------------------------------------------------------------------------
// Recipient autocomplete — compose To/Cc/Bcc search
// ---------------------------------------------------------------------------

#[cfg(test)]
mod recipient_tests;
