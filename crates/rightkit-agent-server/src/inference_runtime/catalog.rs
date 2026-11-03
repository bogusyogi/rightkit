use super::{resolved_direct_addresses, CatalogModel, DirectConnection, InferenceProtocol};
use std::collections::HashSet;

const DIRECT_CATALOG_LIMIT: usize = 4 * 1024 * 1024;

pub async fn discover_direct_models(
    connection: &DirectConnection,
    auth_header: Option<(String, String)>,
) -> Result<Vec<CatalogModel>, &'static str> {
    let resolved = resolved_direct_addresses(connection)
        .await
        .map_err(|error| error.code())?;
    // Header names and values were validated when the connection was saved; this
    // re-check only refuses what could not be sent at all.
    let legal = |name: &str, value: &str| {
        !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_graphic() && b != b':')
            && !value.chars().any(char::is_control)
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in &connection.public_headers {
        if !legal(name, value) {
            return Err("invalid_header");
        }
        headers.push((name.clone(), value.clone()));
    }
    if let Some((name, value)) = auth_header {
        if !legal(&name, &value) {
            return Err("secret_required");
        }
        headers.push((name, value));
    }
    if connection.protocol == InferenceProtocol::AnthropicCompatible
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
    }
    let url = format!(
        "{}/models",
        connection.normalized_base_url.trim_end_matches('/')
    );
    // No proxy, no redirects; the host resolves only to the addresses validated above.
    let client = rightkit_http::http_client::pinned_client(
        &url,
        &resolved,
        std::time::Duration::from_secs(3),
        std::time::Duration::from_secs(10),
    )
    .map_err(|_| "authority_unavailable")?;
    let mut request = rightkit_http::Request::get(url).max_response_bytes(DIRECT_CATALOG_LIMIT);
    for (name, value) in headers {
        // Credentials and operator headers never appear in error text.
        request = request.secret_header(name, value);
    }
    let discovered = match client.send_any(&request).await {
        Ok(response) if rightkit_http::http_client::is_success(response.status) => {
            let value: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| "model_unavailable")?;
            Some(stable_catalog_models(&value).map_err(|_| "model_unavailable")?)
        }
        Err(rightkit_http::HttpError::BodyTooLarge { .. }) => return Err("model_unavailable"),
        _ => None,
    };
    apply_direct_model_policy(discovered, &connection.manual_model_ids)
}

fn apply_direct_model_policy(
    discovered: Option<Vec<CatalogModel>>,
    manual_model_ids: &[String],
) -> Result<Vec<CatalogModel>, &'static str> {
    let discovered_succeeded = discovered.is_some();
    let mut models = match discovered {
        Some(models) => models,
        None if !manual_model_ids.is_empty() => manual_model_ids
            .iter()
            .map(|id| CatalogModel {
                id: id.clone(),
                object: None,
                created: None,
                owned_by: None,
                root: None,
                parent: None,
                permission: serde_json::Value::Null,
                capabilities: Vec::new(),
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                selectable: true,
                unavailable_reason: None,
            })
            .collect(),
        None => return Err("model_unavailable"),
    };
    if !manual_model_ids.is_empty() && discovered_succeeded {
        let allowed = manual_model_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        models.retain(|model| allowed.contains(model.id.as_str()));
    }
    (!models.is_empty())
        .then_some(models)
        .ok_or("model_unavailable")
}

/// Maps only approved upstream fields; unknown response fields never
/// become provider bindings or UI-visible metadata.
pub fn stable_catalog_models(value: &serde_json::Value) -> Result<Vec<CatalogModel>, &'static str> {
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("invalid_catalog")?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for raw in data {
        let Some(id) = raw
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let strings = |name| {
            raw.get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let number = |name| raw.get(name).and_then(serde_json::Value::as_u64);
        let capabilities = raw
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .map(|xs| {
                xs.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        models.push(CatalogModel {
            id: id.to_owned(),
            object: strings("object"),
            created: raw.get("created").and_then(serde_json::Value::as_i64),
            owned_by: strings("owned_by"),
            root: strings("root"),
            parent: strings("parent"),
            permission: raw
                .get("permission")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            capabilities,
            context_length: number("context_length"),
            max_input_tokens: number("max_input_tokens"),
            max_output_tokens: number("max_output_tokens"),
            selectable: true,
            unavailable_reason: None,
        });
    }
    if models.is_empty() {
        return Err("empty_catalog");
    }
    Ok(models)
}

pub fn catalog_entry_hash(model: &CatalogModel) -> String {
    let value = serde_json::to_vec(model).unwrap_or_default();
    rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&value))
}
