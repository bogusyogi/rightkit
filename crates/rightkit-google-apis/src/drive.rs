mod write;
pub use write::*;
// Google Drive REST client — v1 full browser.
// Builds on the shared `google` client for auth, retry, and backoff.
// Covers: list_files (browse + search + pagination), list_recent_files
// (backward-compat alias), and download_file (binary + Google-native export).
//
// download_file requires `Google::get_bytes` in google.rs — see integration spec
// at the bottom of this file (in a doc comment) for the exact helper to add.
use crate::google::Google;
use serde::Deserialize;
const FILES_ENDPOINT: &str = "https://www.googleapis.com/drive/v3/files";
// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────
/// A Google Drive file or folder returned by the files.list endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub modified_time: String,
    pub web_view_link: String,
    // v1 additions
    pub size: String,           // empty string when the API omits it (folders, Docs)
    pub icon_link: String,      // empty string when the API omits it
    pub thumbnail_link: String, // preview image URL (empty when none)
    pub is_folder: bool,        // true when mimeType == application/vnd.google-apps.folder
    pub parents: Vec<String>,   // parent folder id(s); empty for root items
}
/// One page of Drive results plus a continuation token.
#[derive(Debug, serde::Serialize)]
pub struct DriveListing {
    pub files: Vec<DriveFile>,
    pub next_page_token: String, // empty string when there is no next page
}
// ─────────────────────────────────────────────────────────────────────────────
// Internal: wire format from the Drive API
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "mimeType", default)]
    mime_type: String,
    #[serde(rename = "modifiedTime", default)]
    modified_time: String,
    #[serde(rename = "webViewLink", default)]
    web_view_link: String,
    #[serde(default)]
    size: String,
    #[serde(rename = "iconLink", default)]
    icon_link: String,
    #[serde(rename = "thumbnailLink", default)]
    thumbnail_link: String,
    #[serde(default)]
    parents: Vec<String>,
}
#[derive(Deserialize)]
struct ListResp {
    #[serde(rename = "nextPageToken", default)]
    next_page_token: String,
    #[serde(default)]
    files: Vec<RawFile>,
}
// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Escape a single-quote in a Drive q-param value so it doesn't break the
/// query string. Drive uses `\'` as the escape sequence.
fn escape_q(s: &str) -> String {
    s.replace('\'', "\\'")
}
/// Build the Drive `q` filter for files.list.
///
/// Priority:
/// 1. `query` is Some   → name-contains search, trashed=false
/// 2. `shelf` is a known shelf → that shelf's filter (recent/shared/starred)
/// 3. `folder_id` is Some → parent filter, trashed=false
/// 4. none of the above → root children, trashed=false
///
/// A non-empty `mime_filter` (e.g. "image/", "application/pdf",
/// "application/vnd.google-apps.folder") is ANDed on as `mimeType contains '…'`.
fn build_q(
    folder_id: Option<&str>,
    query: Option<&str>,
    shelf: Option<&str>,
    mime_filter: Option<&str>,
) -> String {
    let mut base = if let Some(term) = query {
        format!("name contains '{}' and trashed=false", escape_q(term))
    } else {
        match shelf {
            Some("recent") => "trashed=false".to_string(),
            Some("shared") => "sharedWithMe=true and trashed=false".to_string(),
            Some("starred") => "starred=true and trashed=false".to_string(),
            Some("trash") => "trashed=true".to_string(),
            _ => {
                if let Some(fid) = folder_id {
                    format!("'{}' in parents and trashed=false", escape_q(fid))
                } else {
                    "'root' in parents and trashed=false".to_string()
                }
            }
        }
    };
    if let Some(m) = mime_filter {
        if !m.is_empty() {
            base.push_str(&format!(" and mimeType contains '{}'", escape_q(m)));
        }
    }
    base
}
/// Drive sort order for a shelf: flat recency for the cross-Drive shelves,
/// folders-first for folder/root browsing.
fn order_for(shelf: Option<&str>, query: Option<&str>) -> &'static str {
    if query.is_some() {
        return "modifiedTime desc";
    }
    match shelf {
        Some("recent") | Some("shared") | Some("starred") | Some("trash") => "modifiedTime desc",
        _ => "folder,modifiedTime desc",
    }
}
fn raw_to_drive_file(f: RawFile) -> DriveFile {
    let is_folder = f.mime_type == FOLDER_MIME;
    DriveFile {
        id: f.id,
        name: f.name,
        mime_type: f.mime_type,
        modified_time: f.modified_time,
        web_view_link: f.web_view_link,
        size: f.size,
        icon_link: f.icon_link,
        thumbnail_link: f.thumbnail_link,
        is_folder,
        parents: f.parents,
    }
}
/// Map a Google-native MIME type to an export MIME type for files/{id}/export.
/// Returns None for non-Google-native types (those use alt=media instead).
fn export_mime(doc_mime: &str) -> Option<&'static str> {
    match doc_mime {
        "application/vnd.google-apps.document" => Some("application/pdf"),
        "application/vnd.google-apps.spreadsheet" => {
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        }
        "application/vnd.google-apps.presentation" => Some("application/pdf"),
        "application/vnd.google-apps.drawing" => Some("application/pdf"),
        "application/vnd.google-apps.script" => Some("application/vnd.google-apps.script+json"),
        "application/vnd.google-apps.form" => Some("application/pdf"),
        // Catch-all for any other google-apps type we don't specifically know:
        // export as PDF so something useful comes back.
        _ => Some("application/pdf"),
    }
}
pub fn attachment_name_and_mime(name: &str, mime_type: &str) -> (String, String) {
    if !mime_type.starts_with("application/vnd.google-apps") {
        let mime = if mime_type.trim().is_empty() {
            "application/octet-stream"
        } else {
            mime_type.trim()
        };
        return (name.to_string(), mime.to_string());
    }
    let Some(export_type) = export_mime(mime_type) else {
        return (name.to_string(), "application/octet-stream".to_string());
    };
    let ext = match export_type {
        "application/pdf" => "pdf",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => "pdf",
    };
    let filename = if name.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
        name.to_string()
    } else if let Some((stem, old_ext)) = name.rsplit_once('.') {
        if !stem.is_empty() && (2..=5).contains(&old_ext.len()) {
            format!("{stem}.{ext}")
        } else {
            format!("{name}.{ext}")
        }
    } else {
        format!("{name}.{ext}")
    };
    (filename, export_type.to_string())
}
// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────
/// List Drive files — browse, navigate, or search — one page at a time.
///
/// * `folder_id`  — None → root, Some(id) → that folder's children.
/// * `query`      — if present, overrides folder_id; searches by name.
/// * `page_token` — pass the `next_page_token` from the previous call to page.
#[allow(clippy::too_many_arguments)] // one argument per API field, as the host's call sites pass them
pub async fn list_files(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    folder_id: Option<&str>,
    query: Option<&str>,
    page_token: Option<&str>,
    shelf: Option<&str>,
    mime_filter: Option<&str>,
) -> Result<DriveListing, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let q = build_q(folder_id, query, shelf, mime_filter);
    let fields =
        "nextPageToken,files(id,name,mimeType,modifiedTime,size,iconLink,thumbnailLink,webViewLink,parents)";
    let mut params = vec![
        ("q", q.clone()),
        ("pageSize", "100".to_string()),
        ("orderBy", order_for(shelf, query).to_string()),
        ("fields", fields.to_string()),
        // sharedWithMe lives in the user corpus; allDrives keeps shared items visible.
        ("supportsAllDrives", "true".to_string()),
        ("includeItemsFromAllDrives", "true".to_string()),
    ];
    if let Some(tok) = page_token {
        if !tok.is_empty() {
            params.push(("pageToken", tok.to_string()));
        }
    }
    // get_json wants &[(&str, String)] — convert from vec of owned pairs
    let params_ref: Vec<(&str, String)> = params;
    let resp: ListResp = g
        .get_json(FILES_ENDPOINT, &params_ref)
        .await
        .map_err(String::from)?;
    Ok(DriveListing {
        files: resp.files.into_iter().map(raw_to_drive_file).collect(),
        next_page_token: resp.next_page_token,
    })
}
/// Drive storage quota for the account (bytes, as strings — values can exceed
/// i64 on large pooled quotas, and the API itself returns them as strings).
#[derive(Debug, serde::Serialize)]
pub struct StorageQuota {
    pub limit: String, // empty string for unlimited (Workspace pooled)
    pub usage: String, // total usage across Drive + Gmail + Photos
    pub usage_in_drive: String,
}
#[derive(Deserialize)]
struct AboutResp {
    #[serde(rename = "storageQuota", default)]
    storage_quota: RawQuota,
}
#[derive(Deserialize, Default)]
struct RawQuota {
    #[serde(default)]
    limit: String,
    #[serde(default)]
    usage: String,
    #[serde(rename = "usageInDrive", default)]
    usage_in_drive: String,
}
/// Fetch the account's Drive storage quota via about.get.
pub async fn storage_quota(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<StorageQuota, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let resp: AboutResp = g
        .get_json(
            "https://www.googleapis.com/drive/v3/about",
            &[("fields", "storageQuota".to_string())],
        )
        .await
        .map_err(String::from)?;
    Ok(StorageQuota {
        limit: resp.storage_quota.limit,
        usage: resp.storage_quota.usage,
        usage_in_drive: resp.storage_quota.usage_in_drive,
    })
}
/// Upload a file to Drive (simple multipart upload) under `parent` (or My Drive
/// root when None). Returns the created file.
pub async fn upload_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    filename: &str,
    mime: &str,
    bytes: Vec<u8>,
    parent: Option<&str>,
) -> Result<DriveFile, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let mut meta = serde_json::json!({ "name": filename });
    if let Some(p) = parent {
        meta["parents"] = serde_json::json!([p]);
    }
    let url = "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart\
               &supportsAllDrives=true\
               &fields=id,name,mimeType,modifiedTime,size,iconLink,thumbnailLink,webViewLink,parents";
    let raw: RawFile = g
        .upload_multipart(url, &meta, mime, bytes)
        .await
        .map_err(String::from)?;
    Ok(raw_to_drive_file(raw))
}
/// Download a Drive file to bytes.
///
/// * Google-native types (Docs, Sheets, Slides, …) → exported via
///   `files/{id}/export?mimeType=…` with a sensible target format.
/// * All other types → downloaded raw via `files/{id}?alt=media`.
///
/// Requires `Google::get_bytes` in google.rs (see integration spec below).
pub async fn download_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
    mime_type: &str,
) -> Result<Vec<u8>, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    if mime_type.starts_with("application/vnd.google-apps") {
        // Google-native: use the export endpoint.
        let export_type = export_mime(mime_type).unwrap_or("application/pdf");
        let file_id = rightkit_http::percent_encode_component(file_id);
        let url = format!("{}/{}/export", FILES_ENDPOINT, file_id);
        g.get_bytes(&url, &[("mimeType", export_type.to_string())])
            .await
            .map_err(String::from)
    } else {
        // Binary blob: fetch with alt=media.
        let file_id = rightkit_http::percent_encode_component(file_id);
        let url = format!("{}/{}", FILES_ENDPOINT, file_id);
        g.get_bytes(&url, &[("alt", "media".to_string())])
            .await
            .map_err(String::from)
    }
}
// Tests (offline — no network)
// ─────────────────────────────────────────────────────────────────────────────
