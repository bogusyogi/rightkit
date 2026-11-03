pub(crate) fn install_local_plugin(
    plugin_dir: &FsPath,
    source_path: &FsPath,
    overwrite: bool,
) -> Result<(rightkit_agent_plugins::PluginManifest, PathBuf, bool), StatusCode> {
    if !source_path.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let manifest_path = source_path.join("plugin.json");
    let raw = std::fs::read_to_string(&manifest_path).map_err(|_| StatusCode::BAD_REQUEST)?;
    let manifest = rightkit_agent_plugins::PluginManifest::from_json(&raw)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !validate_plugin_id(&manifest.id) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let source_canonical = source_path
        .canonicalize()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    std::fs::create_dir_all(plugin_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let destination = plugin_dir.join(&manifest.id);

    if destination.exists() {
        let destination_canonical = destination
            .canonicalize()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if destination_canonical == source_canonical {
            return Err(StatusCode::BAD_REQUEST);
        }
        if !overwrite {
            return Err(StatusCode::CONFLICT);
        }
        std::fs::remove_dir_all(&destination).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    copy_plugin_tree(&source_canonical, &destination)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((manifest, destination, overwrite))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginMarketplaceCatalog {
    #[serde(default)]
    pub(crate) plugins: Vec<PluginMarketplaceEntry>,
    #[serde(default)]
    pub(crate) synced_from: String,
    #[serde(default)]
    pub(crate) synced_at: String,
    #[serde(default)]
    pub(crate) signed_by: String,
    #[serde(default)]
    pub(crate) signature_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginMarketplaceEntry {
    pub(crate) manifest: rightkit_agent_plugins::PluginManifest,
    #[serde(default)]
    pub(crate) downloads: u64,
    #[serde(default)]
    pub(crate) rating: f64,
    #[serde(default)]
    pub(crate) source_url: String,
    #[serde(default)]
    pub(crate) source_path: PathBuf,
    pub(crate) checksum_sha256: String,
    #[serde(default)]
    pub(crate) package_sha256: String,
    #[serde(default)]
    pub(crate) signed_by: String,
    #[serde(default)]
    pub(crate) signing_public_key: String,
    #[serde(default)]
    pub(crate) signature: String,
    #[serde(default)]
    pub(crate) category: String,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
}
