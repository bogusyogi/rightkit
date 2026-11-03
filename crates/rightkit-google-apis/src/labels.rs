// Gmail user-label CRUD. Builds on the shared `google` client for auth, retry,
// and backoff — this module only knows the Gmail Labels API endpoints and how to
// marshal the create / rename / delete operations.
//
// All three operations touch only label metadata; no messages are affected.
// Nesting is a naming convention on Gmail's side: pass "Parent/Child" as the
// label name and Gmail treats it as a nested label automatically — no special
// API field needed.
//
// rename_label uses PUT (google.rs has no PATCH helper; Gmail accepts a
// full-resource PUT that preserves omitted fields). delete_label uses
// Google::delete_no_content.

use crate::google::Google;
use serde::{Deserialize, Serialize};

const LABELS_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me/labels";

/// A Gmail label as returned by the create or rename API. Mirrors `gmail::LabelInfo`
/// in shape but derives `Serialize + Deserialize` so it can cross the Tauri IPC
/// boundary. `label_type` is always `"user"` for newly created labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelResult {
    pub id: String,
    pub name: String,
    pub label_type: String,
}

// ─── Internal deserialization target for the Gmail Labels API response ────────

#[derive(Deserialize)]
struct ApiLabel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    label_type: String,
}

impl From<ApiLabel> for LabelResult {
    fn from(l: ApiLabel) -> Self {
        LabelResult {
            id: l.id,
            name: l.name,
            label_type: l.label_type,
        }
    }
}

// ─── Throwaway response for rename (we only need success / error) ─────────────

#[derive(Deserialize)]
struct AnyLabel {}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Create a new custom Gmail label.
///
/// POSTs to `GET /gmail/v1/users/me/labels` with the label name and default
/// visibility settings. Returns the new label's id + name on success.
///
/// Nested labels are expressed as `"Parent/Child"` in `name` — Gmail resolves
/// the hierarchy from the slash convention; no extra API field is required.
pub async fn create_label(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    name: &str,
) -> Result<LabelResult, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let body = serde_json::json!({
        "name": name,
        "labelListVisibility": "labelShow",
        "messageListVisibility": "show"
    });

    let label: ApiLabel = g
        .post_json(LABELS_BASE, &body)
        .await
        .map_err(String::from)?;

    Ok(label.into())
}

/// Rename an existing custom Gmail label.
///
/// Uses PUT (the only JSON-body mutator available in google.rs; Gmail accepts a
/// full-resource PUT on the labels endpoint for name updates). Returns `Ok(())`
/// on success.
///
/// Note: Google's documented operation for partial-field updates is PATCH, but
/// Gmail also accepts PUT with only the fields you wish to change — the omitted
/// fields are preserved server-side. If google.rs gains a `patch_json` helper,
/// prefer that here; see INTEGRATION SPEC.
pub async fn rename_label(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    label_id: &str,
    new_name: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let url = format!("{}/{}", LABELS_BASE, label_id);
    let body = serde_json::json!({ "name": new_name });

    // Deserialize into a throwaway struct; we only care about success/error.
    let _: AnyLabel = g.put_json(&url, &body).await.map_err(String::from)?;

    Ok(())
}

/// Delete a custom Gmail label.
///
/// Calls `DELETE /gmail/v1/users/me/labels/{id}`. Only the label is removed;
/// messages that carried the label are NOT deleted (so it's a ReversibleMutation
/// at the command layer — re-creating the label restores it).
pub async fn delete_label(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    label_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{}/{}", LABELS_BASE, label_id);
    g.delete_no_content(&url).await.map_err(String::from)
}

/// Set a custom label's color. Gmail only accepts hex pairs from its fixed
/// palette — the caller passes a known-good `background`/`text` pair. PATCH so
/// the label name is preserved.
pub async fn set_label_color(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    label_id: &str,
    background: &str,
    text: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{}/{}", LABELS_BASE, label_id);
    let body = serde_json::json!({
        "color": { "backgroundColor": background, "textColor": text }
    });
    let _: AnyLabel = g.patch_json(&url, &body).await.map_err(String::from)?;
    Ok(())
}
