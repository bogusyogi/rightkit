use super::*;

const CONTACT_GROUPS_URL: &str = "https://people.googleapis.com/v1/contactGroups";
const PEOPLE_BATCH_GET_URL: &str = "https://people.googleapis.com/v1/people:batchGet";

/// A user-defined Contacts group (label) the user can expand into recipients.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContactGroup {
    pub resource_name: String,
    pub name: String,
    pub member_count: i64,
}

/// List the user's own contact groups (USER_CONTACT_GROUP only — system groups
/// like "starred"/"all" are filtered out). Read-only.
pub async fn list_contact_groups(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<ContactGroup>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawGroup {
        #[serde(default)]
        resource_name: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        formatted_name: String,
        #[serde(default)]
        member_count: i64,
        #[serde(default)]
        group_type: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Resp {
        #[serde(default)]
        contact_groups: Vec<RawGroup>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let resp: Resp = g
        .get_json(
            CONTACT_GROUPS_URL,
            &[
                ("groupFields", "name,memberCount,groupType".to_string()),
                ("pageSize", "1000".to_string()),
            ],
        )
        .await
        .map_err(String::from)?;
    Ok(resp
        .contact_groups
        .into_iter()
        .filter(|gr| gr.group_type == "USER_CONTACT_GROUP" && gr.member_count > 0)
        .map(|gr| ContactGroup {
            resource_name: gr.resource_name,
            name: if gr.name.is_empty() {
                gr.formatted_name
            } else {
                gr.name
            },
            member_count: gr.member_count,
        })
        .collect())
}

/// Expand a contact group into its members' email addresses (up to 200).
pub async fn expand_contact_group(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    resource_name: &str,
) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GroupGet {
        #[serde(default)]
        member_resource_names: Vec<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Person {
        #[serde(default)]
        email_addresses: Vec<EmailAddress>,
    }
    #[derive(Deserialize)]
    struct BatchResponse {
        #[serde(default)]
        person: Option<Person>,
    }
    #[derive(Deserialize)]
    struct BatchResp {
        #[serde(default)]
        responses: Vec<BatchResponse>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let get_url = format!("https://people.googleapis.com/v1/{resource_name}");
    let group: GroupGet = g
        .get_json(&get_url, &[("maxMembers", "200".to_string())])
        .await
        .map_err(String::from)?;
    if group.member_resource_names.is_empty() {
        return Ok(vec![]);
    }
    let mut params: Vec<(&str, String)> = group
        .member_resource_names
        .iter()
        .take(200)
        .map(|rn| ("resourceNames", rn.clone()))
        .collect();
    params.push(("personFields", "emailAddresses".to_string()));
    let resp: BatchResp = g
        .get_json(PEOPLE_BATCH_GET_URL, &params)
        .await
        .map_err(String::from)?;
    let mut emails = Vec::new();
    for r in resp.responses {
        if let Some(p) = r.person {
            if let Some(e) = p.email_addresses.into_iter().find(|e| !e.value.is_empty()) {
                emails.push(e.value);
            }
        }
    }
    Ok(emails)
}

// ---------------------------------------------------------------------------
// Contacts management — list / create / update / delete (export, import, merge)
// ---------------------------------------------------------------------------
//
// list_all is read-only (contacts.readonly). create/update/delete need the
// writable `https://www.googleapis.com/auth/contacts` scope; an account that
// has not re-consented to it gets a 403 surfaced as a normal error.
