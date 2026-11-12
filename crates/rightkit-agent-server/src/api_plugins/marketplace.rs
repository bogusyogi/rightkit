pub(crate) fn load_plugin_marketplace(path: &FsPath) -> Result<PluginMarketplaceCatalog, String> {
    if !path.exists() {
        return Ok(PluginMarketplaceCatalog {
            plugins: Vec::new(),
            synced_from: String::new(),
            synced_at: String::new(),
            signed_by: String::new(),
            signature_sha256: String::new(),
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

pub(crate) fn write_plugin_marketplace(
    path: &FsPath,
    catalog: &PluginMarketplaceCatalog,
) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(catalog).map_err(|e| e.to_string())?;
    rightkit_fs::atomic_write_with(
        path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |f| std::io::Write::write_all(f, raw.as_bytes()),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn validate_synced_marketplace_catalog(
    catalog: &PluginMarketplaceCatalog,
    require_signed: bool,
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for entry in &catalog.plugins {
        if !validate_plugin_id(&entry.manifest.id) {
            return Err(format!("invalid plugin id '{}'", entry.manifest.id));
        }
        if !seen.insert(entry.manifest.id.clone()) {
            return Err(format!(
                "duplicate marketplace plugin '{}'",
                entry.manifest.id
            ));
        }
        if entry.checksum_sha256.trim().is_empty() {
            return Err(format!(
                "marketplace plugin '{}' is missing checksumSha256",
                entry.manifest.id
            ));
        }
        if require_signed && entry.signed_by.trim().is_empty() {
            return Err(format!(
                "marketplace plugin '{}' is missing signedBy",
                entry.manifest.id
            ));
        }
        if require_signed {
            verify_marketplace_entry_signature(entry)?;
        }
        if entry.source_path.as_os_str().is_empty() && entry.source_url.trim().is_empty() {
            return Err(format!(
                "marketplace plugin '{}' is missing sourcePath/sourceUrl",
                entry.manifest.id
            ));
        }
        if entry.source_path.as_os_str().is_empty() && entry.package_sha256.trim().is_empty() {
            return Err(format!(
                "marketplace plugin '{}' is missing packageSha256",
                entry.manifest.id
            ));
        }
    }
    Ok(())
}

fn decode_base64_field(value: &str, field: &str) -> Result<Vec<u8>, String> {
    let trimmed = value.trim();
    // Padded standard base64 is RightKit's; the unpadded spelling older entries used is accepted by
    // re-padding it (the decoder itself stays the owner's).
    // Only a value with no `=` at all is re-padded: a value that already carries padding is handed to the decoder
    // as it is, so a wrong amount of padding ("AA=") is refused rather than completed into valid input.
    let padded = match trimmed.len() % 4 {
        2 if !trimmed.contains('=') => format!("{trimmed}=="),
        3 if !trimmed.contains('=') => format!("{trimmed}="),
        _ => trimmed.to_string(),
    };
    rightkit_crypto::base64_decode(&padded).map_err(|_| format!("invalid base64 {field}"))
}

fn marketplace_entry_signature_payload(entry: &PluginMarketplaceEntry) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "scheme": crate::host_identity::prefixed("plugin-marketplace-entry-v1"),
        "id": entry.manifest.id,
        "version": entry.manifest.version,
        "checksumSha256": entry.checksum_sha256,
        "packageSha256": entry.package_sha256,
        "sourceUrl": entry.source_url,
        "signedBy": entry.signed_by,
    }))
    .expect("static signature payload is serializable")
}

pub(crate) fn verify_marketplace_entry_signature(
    entry: &PluginMarketplaceEntry,
) -> Result<(), String> {
    if entry.signed_by.trim().is_empty() {
        return Err(format!(
            "marketplace plugin '{}' is missing signedBy",
            entry.manifest.id
        ));
    }
    if entry.signing_public_key.trim().is_empty() {
        return Err(format!(
            "marketplace plugin '{}' is missing signingPublicKey",
            entry.manifest.id
        ));
    }
    if entry.signature.trim().is_empty() {
        return Err(format!(
            "marketplace plugin '{}' is missing signature",
            entry.manifest.id
        ));
    }

    let public_key = decode_base64_field(&entry.signing_public_key, "signingPublicKey")?;
    let public_key: [u8; 32] = public_key
        .as_slice()
        .try_into()
        .map_err(|_| "invalid ed25519 public key length".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)
        .map_err(|_| "invalid ed25519 public key".to_string())?;
    let signature = decode_base64_field(&entry.signature, "signature")?;
    let signature = Signature::from_slice(&signature)
        .map_err(|_| "invalid ed25519 signature length".to_string())?;
    verifying_key
        .verify(&marketplace_entry_signature_payload(entry), &signature)
        .map_err(|_| {
            format!(
                "marketplace plugin '{}' signature verification failed",
                entry.manifest.id
            )
        })
}

pub(crate) async fn fetch_marketplace_url_bytes(
    url: &str,
    allow_file_urls: bool,
    label: &str,
) -> Result<Vec<u8>, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} URL is required"));
    }
    if let Some(path) = trimmed.strip_prefix("file://") {
        if !allow_file_urls {
            return Err(format!("file:// {label} URLs require allowFileUrls"));
        }
        let path =
            if cfg!(windows) && path.starts_with('/') && path.as_bytes().get(2) == Some(&b':') {
                &path[1..]
            } else {
                path
            };
        return std::fs::read(PathBuf::from(path)).map_err(|e| e.to_string());
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(format!("{label} URL must be http(s)"));
    }
    let client = rightkit_http::http_client::endpoint_client_following(
        trimmed,
        rightkit_http::http_client::DEFAULT_TIMEOUT,
    )?;
    let response =
        rightkit_http::http_client::send_any(&client, &rightkit_http::Request::get(trimmed))
            .await?;
    if !rightkit_http::http_client::is_success(response.status) {
        return Err(format!(
            "{label} fetch failed with HTTP {}",
            response.status
        ));
    }
    Ok(response.body)
}

pub(crate) fn plugin_marketplace_entry_json(entry: &PluginMarketplaceEntry) -> serde_json::Value {
    serde_json::json!({
        "id": entry.manifest.id,
        "name": entry.manifest.name,
        "version": entry.manifest.version,
        "description": entry.manifest.description,
        "entry": entry.manifest.entry,
        "providesTools": entry.manifest.provides_tools,
        "providesCommands": entry.manifest.provides_commands,
        "tools": entry.manifest.tools,
        "commands": entry.manifest.commands,
        "prompts": entry.manifest.prompts,
        "middleware": entry.manifest.middleware,
        "sandbox": entry.manifest.sandbox,
        "enabled": entry.manifest.enabled,
        "downloads": entry.downloads,
        "rating": entry.rating,
        "sourceUrl": entry.source_url,
        "sourcePath": entry.source_path,
        "checksumSha256": entry.checksum_sha256,
        "packageSha256": entry.package_sha256,
        "signedBy": entry.signed_by,
        "signingPublicKey": entry.signing_public_key,
        "signature": entry.signature,
        "category": entry.category,
        "tags": entry.tags,
    })
}
