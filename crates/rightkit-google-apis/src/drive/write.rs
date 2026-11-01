use super::*;

const WRITE_FIELDS: &str =
    "id,name,mimeType,modifiedTime,size,iconLink,thumbnailLink,webViewLink,parents";

fn drive(client_id: &str, client_secret: &str, refresh_token: &str) -> Google {
    Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    )
}

/// Create a new folder, optionally under `parent_id`. Returns the created folder.
pub async fn create_folder(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    name: &str,
    parent_id: Option<&str>,
) -> Result<DriveFile, String> {
    let g = drive(client_id, client_secret, refresh_token);
    let mut body = serde_json::json!({ "name": name, "mimeType": FOLDER_MIME });
    if let Some(p) = parent_id {
        body["parents"] = serde_json::json!([p]);
    }
    let url = format!("{FILES_ENDPOINT}?fields={WRITE_FIELDS}&supportsAllDrives=true");
    let raw: RawFile = g.post_json(&url, &body).await.map_err(String::from)?;
    Ok(raw_to_drive_file(raw))
}

/// Rename a file or folder. Returns the updated resource.
pub async fn rename_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
    new_name: &str,
) -> Result<DriveFile, String> {
    let g = drive(client_id, client_secret, refresh_token);
    let file_id = rightkit_http::percent_encode_component(file_id);
    let url = format!("{FILES_ENDPOINT}/{file_id}?fields={WRITE_FIELDS}&supportsAllDrives=true");
    let body = serde_json::json!({ "name": new_name });
    let raw: RawFile = g.patch_json(&url, &body).await.map_err(String::from)?;
    Ok(raw_to_drive_file(raw))
}

/// Move a file by re-parenting it (remove `old_parent`, add `new_parent`).
pub async fn move_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
    new_parent: &str,
    old_parent: Option<&str>,
) -> Result<DriveFile, String> {
    let g = drive(client_id, client_secret, refresh_token);
    let file_id = rightkit_http::percent_encode_component(file_id);
    let new_parent = rightkit_http::percent_encode_component(new_parent);
    let mut url = format!(
        "{FILES_ENDPOINT}/{file_id}?addParents={new_parent}&fields={WRITE_FIELDS}&supportsAllDrives=true"
    );
    if let Some(old) = old_parent {
        if !old.is_empty() {
            let old = rightkit_http::percent_encode_component(old);
            url.push_str(&format!("&removeParents={old}"));
        }
    }
    // No body fields change; re-parenting is driven entirely by the query params.
    let raw: RawFile = g
        .patch_json(&url, &serde_json::json!({}))
        .await
        .map_err(String::from)?;
    Ok(raw_to_drive_file(raw))
}

/// Move a file to the Drive trash (recoverable — never a hard delete).
pub async fn trash_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
) -> Result<(), String> {
    let g = drive(client_id, client_secret, refresh_token);
    let file_id = rightkit_http::percent_encode_component(file_id);
    let url = format!("{FILES_ENDPOINT}/{file_id}?supportsAllDrives=true");
    let body = serde_json::json!({ "trashed": true });
    let _: RawFile = g.patch_json(&url, &body).await.map_err(String::from)?;
    Ok(())
}

/// Share a file with a person by email at a given role
/// ("reader" | "commenter" | "writer"). Returns the new permission id.
pub async fn share_file(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
    email: &str,
    role: &str,
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct PermResp {
        #[serde(default)]
        id: String,
    }
    let role = match role {
        "reader" | "commenter" | "writer" => role,
        _ => return Err("share role must be reader, commenter or writer".into()),
    };
    let g = drive(client_id, client_secret, refresh_token);
    // sendNotificationEmail=true so the recipient is told (Drive default behavior).
    let file_id = rightkit_http::percent_encode_component(file_id);
    let url = format!("{FILES_ENDPOINT}/{file_id}/permissions?supportsAllDrives=true");
    let body = serde_json::json!({ "type": "user", "role": role, "emailAddress": email });
    let resp: PermResp = g.post_json(&url, &body).await.map_err(String::from)?;
    Ok(resp.id)
}

/// A sharing permission on a Drive file (who has access + at what role).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DrivePermission {
    pub id: String,
    pub role: String,
    pub kind: String,
    pub email: String,
    pub display_name: String,
}

/// List the sharing permissions on a file (for the share manager UI).
pub async fn list_permissions(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
) -> Result<Vec<DrivePermission>, String> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        permissions: Vec<RawPerm>,
    }
    #[derive(Deserialize)]
    struct RawPerm {
        #[serde(default)]
        id: String,
        #[serde(default)]
        role: String,
        #[serde(default, rename = "type")]
        kind: String,
        #[serde(default, rename = "emailAddress")]
        email: String,
        #[serde(default, rename = "displayName")]
        display_name: String,
    }
    let g = drive(client_id, client_secret, refresh_token);
    let file_id = rightkit_http::percent_encode_component(file_id);
    let url = format!(
        "{FILES_ENDPOINT}/{file_id}/permissions?fields=permissions(id,role,type,emailAddress,displayName)&supportsAllDrives=true"
    );
    let raw: Raw = g.get_json(&url, &[]).await.map_err(String::from)?;
    Ok(raw
        .permissions
        .into_iter()
        .map(|p| DrivePermission {
            id: p.id,
            role: p.role,
            kind: p.kind,
            email: p.email,
            display_name: p.display_name,
        })
        .collect())
}

/// Revoke a sharing permission (remove someone's access). The owner permission
/// cannot be deleted by the API and is hidden from the revoke UI.
pub async fn revoke_permission(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    file_id: &str,
    permission_id: &str,
) -> Result<(), String> {
    let g = drive(client_id, client_secret, refresh_token);
    let file_id = rightkit_http::percent_encode_component(file_id);
    let permission_id = rightkit_http::percent_encode_component(permission_id);
    let url =
        format!("{FILES_ENDPOINT}/{file_id}/permissions/{permission_id}?supportsAllDrives=true");
    g.delete_no_content(&url).await.map_err(String::from)
}

// ─────────────────────────────────────────────────────────────────────────────
