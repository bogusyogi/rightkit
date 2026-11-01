use super::*;

/// Map a persisted `GoalRow` (SQLite) back to the engine's live `GoalState` on
/// session resume. `cost_usd` resets to 0 — the daemon recomputes it from the
/// SessionUsageRollup; everything else round-trips from the goals table.
pub(crate) fn goal_state_from_row(
    row: rightkit_agent_engine_storage::GoalRow,
) -> rightkit_agent_engine_core::GoalState {
    rightkit_agent_engine_core::GoalState {
        id: row.id,
        text: row.text,
        status: row.status.as_str().to_string(),
        budget_usd: row.budget_usd,
        budget_tokens: row.budget_tokens,
        tokens_used: row.tokens_used,
        cost_usd: 0.0,
        continuation_count: row.continuation_count,
        no_progress_count: row.no_progress_count,
        gds_score: 0.0,
        verification_present: false,
        verification_approved: false,
        started_at: row.started_at,
        closed_at: row.closed_at,
        dismissed_at: row.dismissed_at,
        decomposition_state: row.decomposition_state,
        plan_revision: row.plan_revision,
        now: row.now,
        next: row.next,
        steps: row.steps,
        verification_state: row.verification_state,
    }
}

pub(crate) fn now_iso() -> String {
    rightkit_agent_engine_protocol::now_iso()
}

pub(crate) fn now_millis() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub(crate) fn trust_envelope(
    trust: ContentTrust,
    source: impl Into<String>,
    content: &str,
) -> TrustEnvelope {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(content.as_bytes());
    let instructions_are_data = matches!(
        trust,
        ContentTrust::ToolUntrusted | ContentTrust::RemoteAgentUntrusted
    );
    TrustEnvelope {
        trust,
        source: source.into(),
        instructions_are_data,
        content_hash: format!("sha256:{}", hasher.finalize()),
    }
}

pub(crate) fn push_session_artifact(
    session: &LiveSession,
    mut artifact: rightkit_agent_engine_protocol::ArtifactRow,
) {
    stamp_spec_artifact_metadata(session, &mut artifact);
    lock_recover(&session.artifacts, "session.artifacts").push(artifact.clone());
    if let Some(storage) = session.storage.clone() {
        let session_id = session.id.clone();
        tokio::spawn(async move {
            let store = storage.lock().await;
            if let Err(e) = store.upsert_artifact(&session_id, &artifact) {
                tracing::warn!(session_id = %session_id, error = %e, "artifact persist failed");
            }
        });
    }
}

pub(crate) async fn persist_artifacts_snapshot(
    state: &DaemonState,
    session_id: &str,
    artifacts: &[rightkit_agent_engine_protocol::ArtifactRow],
) {
    let Some(storage) = &state.storage else {
        return;
    };
    let store = storage.lock().await;
    for artifact in artifacts {
        if let Err(e) = store.upsert_artifact(session_id, artifact) {
            tracing::warn!(session_id = %session_id, error = %e, "artifact persist failed");
        }
    }
}

pub(crate) async fn load_stored_artifacts(
    state: &DaemonState,
    session_id: &str,
) -> Option<Vec<rightkit_agent_engine_protocol::ArtifactRow>> {
    let storage = state.storage.as_ref()?;
    let store = storage.lock().await;
    store.list_artifacts(session_id).ok()
}

pub(crate) fn stamp_spec_artifact_metadata(
    session: &LiveSession,
    artifact: &mut rightkit_agent_engine_protocol::ArtifactRow,
) {
    let Some(spec_id) = lock_recover(&session.spec_id, "session.spec_id").clone() else {
        return;
    };
    if spec_id.trim().is_empty() {
        return;
    }
    let mut meta = serde_json::from_str::<serde_json::Value>(&artifact.meta)
        .unwrap_or_else(|_| serde_json::json!({}));
    if !meta.is_object() {
        meta = serde_json::json!({});
    }
    if meta
        .get("specId")
        .and_then(|value| value.as_str())
        .is_none()
    {
        meta["specId"] = serde_json::Value::String(spec_id);
    }
    if meta
        .get("evidenceSource")
        .and_then(|value| value.as_str())
        .is_none()
    {
        meta["evidenceSource"] = serde_json::Value::String("bound-session-artifact".to_string());
    }
    artifact.meta = meta.to_string();
}

/// Load layered project instructions for daemon-created sessions.
///
/// This preserves legacy `right.md` support while also honoring common coding
/// agent files such as `CLAUDE.md`, `AGENTS.md`, `.claude/rules/*.md`, and
/// `.claude/instructions/*.md`.
pub(crate) fn load_instruction_context(workspace_root: &str) -> Option<String> {
    let (loader, files) = discover_instruction_files(workspace_root);
    if files.is_empty() {
        return None;
    }

    let composed = loader.compose(&files);
    let trimmed = composed.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn discover_instruction_files(
    workspace_root: &str,
) -> (
    rightkit_app_settings::agent_config::InstructionLoader,
    Vec<rightkit_app_settings::agent_config::InstructionFile>,
) {
    let mut loader = rightkit_app_settings::agent_config::InstructionLoader::new();

    if let Some(home) = rightkit_app_settings::agent_config::os_home_dir() {
        loader.add_root(
            rightkit_app_settings::agent_config::InstructionScope::User,
            home.join(".config/coderight"),
        );
    }

    if !workspace_root.is_empty() {
        let workspace = std::path::Path::new(workspace_root);
        loader.add_root(
            rightkit_app_settings::agent_config::InstructionScope::Project,
            workspace.to_path_buf(),
        );
        loader.add_root(
            rightkit_app_settings::agent_config::InstructionScope::Local,
            workspace.join(".coderight"),
        );
    }

    // Only `.claude/rules` is trust-gated; root files stay ungated (#131).
    let files = loader.discover_gated(|_| {
        !workspace_root.is_empty()
            && rightkit_app_settings::agent_config::is_workspace_trusted(std::path::Path::new(
                workspace_root,
            ))
    });
    (loader, files)
}

pub(crate) async fn get_instructions_handler(
    Query(q): Query<WorkspaceQuery>,
) -> Json<serde_json::Value> {
    let workspace_root = q.workspace_root.trim().to_string();
    let (loader, files) = discover_instruction_files(&workspace_root);
    let composed = if files.is_empty() {
        String::new()
    } else {
        loader.compose(&files)
    };
    let rows = files
        .into_iter()
        .map(|file| {
            serde_json::json!({
                "scope": file.scope.to_string(),
                "path": file.path,
                "bodyBytes": file.body.len(),
                "bodyLines": file.body.lines().count(),
                "globs": file.frontmatter.globs,
                "paths": file.frontmatter.paths,
                "includes": file.frontmatter.includes,
                "hookRefs": file.frontmatter.hook_refs,
            })
        })
        .collect::<Vec<_>>();

    Json(serde_json::json!({
        "workspaceRoot": workspace_root,
        "fileCount": rows.len(),
        "composedBytes": composed.trim().len(),
        "files": rows,
    }))
}

const MAX_DISCOVERY_FILES: usize = 256;
const MAX_DISCOVERY_FILE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapabilityIndexEntry {
    kind: &'static str,
    name: String,
    description: String,
    source: String,
    precedence: u8,
    effect_class: &'static str,
    trust: &'static str,
    risks: Vec<&'static str>,
    context_bytes: usize,
    excluded: bool,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct CapabilityDiscoveryQuery {
    #[serde(default, rename = "workspaceRoot", alias = "workspace_root")]
    workspace_root: String,
    /// Comma-separated `kind:name` identifiers. Exclusion is request-scoped:
    /// inspection never mutates trust or creates a second capability store.
    #[serde(default)]
    exclude: Option<String>,
}

pub(crate) async fn get_capability_discovery_handler(
    Query(query): Query<CapabilityDiscoveryQuery>,
) -> Json<serde_json::Value> {
    let excluded = query
        .exclude
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<std::collections::HashSet<_>>();
    let entries = discover_capability_index(std::path::Path::new(&query.workspace_root), &excluded);
    let context_bytes = entries
        .iter()
        .filter(|entry| !entry.excluded)
        .map(|entry| entry.context_bytes)
        .sum::<usize>();
    let compact_index = entries
        .iter()
        .filter(|entry| !entry.excluded)
        .map(|entry| {
            serde_json::json!({
                "id": format!("{}:{}", entry.kind, entry.name),
                "description": entry.description,
                "effectClass": entry.effect_class,
                "trust": entry.trust,
                "contextBytes": entry.context_bytes,
            })
        })
        .collect::<Vec<_>>();

    Json(serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": query.workspace_root,
        "entryCount": entries.len(),
        "contextBytes": context_bytes,
        "compactIndex": compact_index,
        "entries": entries,
        "discoveryGrantsAuthority": false,
    }))
}

fn discover_capability_index(
    workspace: &std::path::Path,
    excluded: &std::collections::HashSet<&str>,
) -> Vec<CapabilityIndexEntry> {
    let mut entries = std::collections::BTreeMap::<String, CapabilityIndexEntry>::new();

    let (loader, instructions) = discover_instruction_files(workspace.to_str().unwrap_or(""));
    for instruction in instructions {
        let source = instruction.path.clone();
        insert_capability(
            &mut entries,
            excluded,
            CapabilityIndexEntry {
                kind: "instruction",
                name: std::path::Path::new(&instruction.path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("instruction")
                    .to_string(),
                description: format!("{} scoped workspace instructions", instruction.scope),
                source,
                precedence: instruction.scope as u8,
                effect_class: "guidance",
                trust: "policy_gated",
                risks: Vec::new(),
                context_bytes: instruction.body.len(),
                excluded: false,
            },
        );
    }
    drop(loader);

    let mut skill_roots = Vec::new();
    if let Some(state) = state_dir_for_discovery() {
        skill_roots.push((
            rightkit_agent_host_traits::skill_host::manifest::SkillSourceScope::User,
            state.join("skills"),
        ));
    }
    skill_roots.push((
        rightkit_agent_host_traits::skill_host::manifest::SkillSourceScope::WorkspaceCompatibility,
        workspace.join(".claude/skills"),
    ));
    skill_roots.push((
        rightkit_agent_host_traits::skill_host::manifest::SkillSourceScope::Workspace,
        workspace.join(".coderight/skills"),
    ));
    for skill in
        rightkit_agent_host_traits::skill_host::manifest::discover_skill_roots(&skill_roots)
    {
        let mut risks = scan_declaration_risks(&skill.manifest.body);
        if !skill.manifest.effects.allowed_tools.is_empty()
            || !skill.manifest.effects.disallowed_tools.is_empty()
        {
            risks.push("tool_permissions");
        }
        let effect = if skill.manifest.effects.allowed_tools.is_empty()
            && skill.manifest.effects.disallowed_tools.is_empty()
        {
            "guidance"
        } else {
            "tool_policy"
        };
        insert_capability(
            &mut entries,
            excluded,
            CapabilityIndexEntry {
                kind: "skill",
                name: skill.manifest.name,
                description: skill.manifest.description,
                source: skill.path.to_string_lossy().to_string(),
                precedence: skill.scope as u8,
                effect_class: effect,
                trust: "policy_gated",
                risks,
                context_bytes: skill.manifest.body.len(),
                excluded: false,
            },
        );
    }

    discover_hook_and_command_files(workspace, &mut entries, excluded);
    discover_mcp_entries(workspace, &mut entries, excluded);
    discover_plugin_entries(workspace, &mut entries, excluded);

    entries.into_values().collect()
}

fn insert_capability(
    entries: &mut std::collections::BTreeMap<String, CapabilityIndexEntry>,
    excluded: &std::collections::HashSet<&str>,
    mut entry: CapabilityIndexEntry,
) {
    let key = format!("{}:{}", entry.kind, entry.name);
    entry.excluded = excluded.contains(key.as_str());
    let replace = entries
        .get(&key)
        .is_none_or(|current| entry.precedence >= current.precedence);
    if replace {
        entries.insert(key, entry);
    }
}

fn discover_hook_and_command_files(
    workspace: &std::path::Path,
    entries: &mut std::collections::BTreeMap<String, CapabilityIndexEntry>,
    excluded: &std::collections::HashSet<&str>,
) {
    for (kind, root, effect) in [
        ("hook", workspace.join(".coderight/hooks"), "execute"),
        ("command", workspace.join(".coderight/commands"), "guidance"),
    ] {
        for path in bounded_declaration_files(&root) {
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(kind)
                .to_string();
            let mut risks = scan_declaration_risks(&body);
            if kind == "hook" {
                risks.push("executable");
                if !body.contains("timeout") {
                    risks.push("missing_timeout");
                }
            }
            insert_capability(
                entries,
                excluded,
                CapabilityIndexEntry {
                    kind,
                    name,
                    description: body
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("workspace declaration")
                        .trim_start_matches('#')
                        .trim()
                        .chars()
                        .take(160)
                        .collect(),
                    source: path.to_string_lossy().to_string(),
                    precedence: 3,
                    effect_class: effect,
                    trust: "needs_approval",
                    risks,
                    context_bytes: body.len(),
                    excluded: false,
                },
            );
        }
    }
}

fn discover_mcp_entries(
    workspace: &std::path::Path,
    entries: &mut std::collections::BTreeMap<String, CapabilityIndexEntry>,
    excluded: &std::collections::HashSet<&str>,
) {
    let paths = rightkit_mcp::agent_mcp::config::config_paths(Some(workspace));
    for (precedence, path) in paths.into_iter().enumerate() {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<rightkit_mcp::agent_mcp::McpConfigFile>(&raw)
        else {
            continue;
        };
        for (name, server) in config.mcp_servers {
            let (description, effect, mut risks) = match server {
                rightkit_mcp::agent_mcp::McpServerConfig::Stdio(config) => {
                    let mut risks = vec!["executable"];
                    if config.command == "npx"
                        && config
                            .args
                            .iter()
                            .any(|arg| !arg.starts_with('-') && !npm_package_spec_is_pinned(arg))
                    {
                        risks.push("unpinned_package");
                    }
                    if config.env.iter().any(|(key, value)| {
                        let key = key.to_ascii_lowercase();
                        !value.starts_with("${")
                            && !value.is_empty()
                            && (key.contains("token")
                                || key.contains("secret")
                                || key.contains("password")
                                || key.contains("api_key"))
                    }) {
                        risks.push("secret_exposure");
                    }
                    (
                        format!("stdio MCP via {}", config.command),
                        "execute",
                        risks,
                    )
                }
                rightkit_mcp::agent_mcp::McpServerConfig::Http(config) => (
                    format!("HTTP MCP at {}", config.url),
                    "network",
                    vec!["network_access"],
                ),
                rightkit_mcp::agent_mcp::McpServerConfig::Sse(config) => (
                    format!("SSE MCP at {}", config.url),
                    "network",
                    vec!["network_access"],
                ),
            };
            if raw.contains("\"*\"") {
                risks.push("wildcard_permissions");
            }
            insert_capability(
                entries,
                excluded,
                CapabilityIndexEntry {
                    kind: "mcp",
                    name,
                    description,
                    source: path.to_string_lossy().to_string(),
                    precedence: precedence.min(u8::MAX as usize) as u8,
                    effect_class: effect,
                    trust: "needs_approval",
                    risks,
                    context_bytes: 96,
                    excluded: false,
                },
            );
        }
    }
}

fn npm_package_spec_is_pinned(spec: &str) -> bool {
    if let Some(stripped) = spec.strip_prefix('@') {
        stripped.contains('@')
    } else {
        spec.contains('@')
    }
}

fn discover_plugin_entries(
    workspace: &std::path::Path,
    entries: &mut std::collections::BTreeMap<String, CapabilityIndexEntry>,
    excluded: &std::collections::HashSet<&str>,
) {
    for (plugin, root) in plugin_manifests_with_roots(&workspace.join(".coderight/plugins")) {
        let mut risks = vec!["executable"];
        if plugin.sandbox.inherit_env {
            risks.push("secret_exposure");
        }
        if plugin.sandbox.timeout_ms == 0 {
            risks.push("missing_timeout");
        }
        if plugin.tools.values().any(|tool| tool.network_access) {
            risks.push("network_access");
        }
        insert_capability(
            entries,
            excluded,
            CapabilityIndexEntry {
                kind: "plugin",
                name: plugin.id,
                description: plugin.description,
                source: root.to_string_lossy().to_string(),
                precedence: 3,
                effect_class: "execute",
                trust: "needs_approval",
                risks,
                context_bytes: 96,
                excluded: false,
            },
        );
    }
}

fn bounded_declaration_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(canonical_root) = root.canonicalize() else {
        return Vec::new();
    };
    let mut pending = vec![(canonical_root.clone(), 0usize)];
    let mut out = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        if depth > 3 || out.len() >= MAX_DISCOVERY_FILES {
            continue;
        }
        let Ok(read_dir) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in read_dir.flatten() {
            if out.len() >= MAX_DISCOVERY_FILES {
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            if !canonical.starts_with(&canonical_root) {
                continue;
            }
            if file_type.is_dir() {
                pending.push((canonical, depth + 1));
            } else if file_type.is_file()
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() <= MAX_DISCOVERY_FILE_BYTES as u64)
            {
                out.push(canonical);
            }
        }
    }
    out.sort();
    out
}

fn scan_declaration_risks(body: &str) -> Vec<&'static str> {
    let lower = body.to_ascii_lowercase();
    let mut risks = Vec::new();
    if lower.contains("http://") || lower.contains("https://") || lower.contains("network") {
        risks.push("network_access");
    }
    if lower.contains("allowed_tools: [\"*\"]") || lower.contains("permissions = [\"*\"]") {
        risks.push("wildcard_permissions");
    }
    if lower.contains("api_key=") || lower.contains("token=") || lower.contains("password=") {
        risks.push("secret_exposure");
    }
    if lower.contains("../") || lower.contains("..\\") {
        risks.push("path_escape");
    }
    risks.sort_unstable();
    risks.dedup();
    risks
}

fn state_dir_for_discovery() -> Option<std::path::PathBuf> {
    rightkit_app_settings::agent_config::state_root()
}

#[cfg(test)]
mod capability_discovery_tests {
    use super::*;

    #[test]
    fn unified_index_dedupes_scans_risk_and_applies_request_exclusion() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".coderight/skills/review")).unwrap();
        std::fs::create_dir_all(root.path().join(".claude/skills/review")).unwrap();
        std::fs::create_dir_all(root.path().join(".coderight/hooks")).unwrap();
        std::fs::create_dir_all(root.path().join(".coderight/commands")).unwrap();
        std::fs::write(
            root.path().join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: compatibility\n---\nread only",
        )
        .unwrap();
        std::fs::write(
            root.path().join(".coderight/skills/review/SKILL.md"),
            "---\nname: review\ndescription: workspace\n---\nread only",
        )
        .unwrap();
        std::fs::write(
            root.path().join(".coderight/hooks/deploy.json"),
            r#"{"command":"deploy","permissions":["*"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join(".coderight/commands/inspect.md"),
            "# Inspect\nRead the workspace",
        )
        .unwrap();
        std::fs::write(
            root.path().join(".coderight/mcp.json"),
            r#"{"mcpServers":{"local":{"type":"stdio","command":"npx","args":["server"]}}}"#,
        )
        .unwrap();

        let excluded = std::collections::HashSet::from(["command:inspect"]);
        let entries = discover_capability_index(root.path(), &excluded);
        let skills = entries
            .iter()
            .filter(|entry| entry.kind == "skill" && entry.name == "review")
            .collect::<Vec<_>>();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "workspace");
        assert!(entries.iter().any(|entry| {
            entry.kind == "hook"
                && entry.risks.contains(&"executable")
                && entry.risks.contains(&"missing_timeout")
        }));
        assert!(entries.iter().any(|entry| {
            entry.kind == "mcp"
                && entry.name == "local"
                && entry.risks.contains(&"unpinned_package")
        }));
        assert!(entries
            .iter()
            .any(|entry| entry.kind == "command" && entry.excluded));
    }

    #[test]
    fn scoped_npm_package_requires_a_second_at_sign_to_be_pinned() {
        assert!(!npm_package_spec_is_pinned("@scope/server"));
        assert!(npm_package_spec_is_pinned("@scope/server@1.2.3"));
        assert!(npm_package_spec_is_pinned("server@1.2.3"));
    }
}
