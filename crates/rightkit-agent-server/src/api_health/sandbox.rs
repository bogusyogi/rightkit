fn workspace_root_or_current(value: Option<&str>) -> PathBuf {
    optional_workspace_root(value)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn optional_workspace_root(value: Option<&str>) -> Option<PathBuf> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn sandbox_settings_path(workspace_root: &FsPath) -> PathBuf {
    workspace_root
        .join(".coderight")
        .join("sandbox-settings.json")
}

const ALLOWED_SANDBOX_TIERS: &[&str] = &[
    rightkit_agent_policy::SandboxTier::Strict.persisted_value(),
    rightkit_agent_policy::SandboxTier::WorkspaceWrite.persisted_value(),
    rightkit_agent_policy::SandboxTier::Disabled.persisted_value(),
];

fn allowed_sandbox_tiers() -> &'static [&'static str] {
    ALLOWED_SANDBOX_TIERS
}

fn normalize_sandbox_tier(value: &str) -> Option<String> {
    rightkit_agent_policy::SandboxTier::parse_alias(value).map(|tier| tier.persisted_value().to_string())
}

fn load_sandbox_settings(
    workspace_root: &FsPath,
) -> Result<Option<SandboxSettingsRecord>, std::io::Error> {
    let path = sandbox_settings_path(workspace_root);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    let mut record: SandboxSettingsRecord = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    record.tier = record.tier.trim().to_string();
    Ok(Some(record))
}

fn resolve_sandbox_settings(
    saved: Option<&SandboxSettingsRecord>,
) -> rightkit_agent_policy::SandboxPolicyResolution {
    let operator = std::env::var("RIGHTKIT_SANDBOX_TIER").ok();
    rightkit_agent_policy::resolve_sandbox_policy(
        operator.as_deref(),
        saved.map(|record| record.tier.as_str()),
    )
}

fn sandbox_policy_source(source: rightkit_agent_policy::SandboxPolicySource) -> &'static str {
    match source {
        rightkit_agent_policy::SandboxPolicySource::Default => "default",
        rightkit_agent_policy::SandboxPolicySource::Operator => "environment",
        rightkit_agent_policy::SandboxPolicySource::Workspace => "workspace",
        rightkit_agent_policy::SandboxPolicySource::OperatorAndWorkspace => "environment+workspace",
    }
}

fn save_sandbox_settings_record(
    workspace_root: &FsPath,
    record: &SandboxSettingsRecord,
) -> Result<(), std::io::Error> {
    let path = sandbox_settings_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(record)?;
    crate::store_write::write_store_file(&path, raw.as_bytes())
}

// ── Readiness snapshot ─────────────────────────────────────────────────────
