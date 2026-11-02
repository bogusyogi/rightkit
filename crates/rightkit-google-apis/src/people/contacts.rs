use super::*;

const CONNECTIONS_URL: &str = "https://people.googleapis.com/v1/people/me/connections";
const CREATE_CONTACT_URL: &str = "https://people.googleapis.com/v1/people:createContact";
const CONTACT_FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations,photos";

/// A full personal contact, enough to export to vCard and to dedupe/merge.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContactFull {
    pub resource_name: String,
    pub etag: String,
    pub display_name: String,
    pub given_name: String,
    pub family_name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub org: String,
    /// URL of the contact's primary photo, or empty string if not available.
    pub photo_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FullName {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    given_name: String,
    #[serde(default)]
    family_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoneNumber {
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FullPerson {
    #[serde(default, rename = "resourceName")]
    resource_name: String,
    #[serde(default)]
    etag: String,
    #[serde(default)]
    names: Vec<FullName>,
    #[serde(default)]
    email_addresses: Vec<EmailAddress>,
    #[serde(default)]
    phone_numbers: Vec<PhoneNumber>,
    #[serde(default)]
    organizations: Vec<Organization>,
    #[serde(default)]
    photos: Vec<Photo>,
}

impl From<FullPerson> for ContactFull {
    fn from(p: FullPerson) -> Self {
        let name = p.names.first();
        ContactFull {
            resource_name: p.resource_name,
            etag: p.etag,
            display_name: name.map(|n| n.display_name.clone()).unwrap_or_default(),
            given_name: name.map(|n| n.given_name.clone()).unwrap_or_default(),
            family_name: name.map(|n| n.family_name.clone()).unwrap_or_default(),
            emails: p
                .email_addresses
                .into_iter()
                .map(|e| e.value)
                .filter(|v| !v.is_empty())
                .collect(),
            phones: p
                .phone_numbers
                .into_iter()
                .map(|p| p.value)
                .filter(|v| !v.is_empty())
                .collect(),
            org: p
                .organizations
                .into_iter()
                .next()
                .map(|o| if o.name.is_empty() { o.title } else { o.name })
                .unwrap_or_default(),
            photo_url: p
                .photos
                .into_iter()
                .next()
                .map(|ph| ph.url)
                .unwrap_or_default(),
        }
    }
}

/// Every personal contact (paged `connections.list`). Read-only.
pub async fn list_all_contacts(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<ContactFull>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Resp {
        #[serde(default)]
        connections: Vec<FullPerson>,
        #[serde(default)]
        next_page_token: Option<String>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let mut out: Vec<ContactFull> = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut params = vec![
            ("personFields", CONTACT_FIELDS.to_string()),
            ("pageSize", "1000".to_string()),
        ];
        if let Some(t) = &page_token {
            params.push(("pageToken", t.clone()));
        }
        let resp: Resp = g
            .get_json(CONNECTIONS_URL, &params)
            .await
            .map_err(String::from)?;
        out.extend(resp.connections.into_iter().map(ContactFull::from));
        match resp.next_page_token {
            Some(t) if !t.is_empty() => page_token = Some(t),
            _ => break,
        }
    }
    Ok(out)
}

fn person_body(name: &str, emails: &[String], phones: &[String]) -> serde_json::Value {
    serde_json::json!({
        "names": [{ "unstructuredName": name }],
        "emailAddresses": emails.iter().map(|e| serde_json::json!({ "value": e })).collect::<Vec<_>>(),
        "phoneNumbers": phones.iter().map(|p| serde_json::json!({ "value": p })).collect::<Vec<_>>(),
    })
}

/// Create a personal contact. Needs the writable `contacts` scope.
pub async fn create_contact(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    name: &str,
    emails: &[String],
    phones: &[String],
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let _: serde_json::Value = g
        .post_json(CREATE_CONTACT_URL, &person_body(name, emails, phones))
        .await
        .map_err(String::from)?;
    Ok(())
}

/// Update an existing contact (requires its current `etag`). Writable scope.
#[allow(clippy::too_many_arguments)] // one argument per API field, as the host's call sites pass them
pub async fn update_contact(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    resource_name: &str,
    etag: &str,
    name: &str,
    emails: &[String],
    phones: &[String],
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let mut body = person_body(name, emails, phones);
    body["etag"] = serde_json::json!(etag);
    let url = format!(
        "https://people.googleapis.com/v1/{resource_name}:updateContact?updatePersonFields=names,emailAddresses,phoneNumbers"
    );
    let _: serde_json::Value = g.patch_json(&url, &body).await.map_err(String::from)?;
    Ok(())
}

/// Delete a personal contact by resource name. Writable scope.
pub async fn delete_contact(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    resource_name: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("https://people.googleapis.com/v1/{resource_name}:deleteContact");
    g.delete_no_content(&url).await.map_err(String::from)
}

// ---------------------------------------------------------------------------
// Tests — offline parse + dedup logic (no network)
// ---------------------------------------------------------------------------
