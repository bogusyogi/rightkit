//! Named run presets (A207): `host app --preset <name>`.
//!
//! A preset bundles model, effort, toolset, permission mode and prompts.
//! Claude Code has no first-party "preset" object; the closest documented
//! equivalents are the CLI flags a preset sets: `--model`, `--permission-mode`,
//! `--allowedTools`/`--disallowedTools`, `--append-system-prompt`
//! (<https://code.claude.com/docs/en/cli-reference>) and settings files
//! (<https://code.claude.com/docs/en/settings>). Explicit CLI flags always win.
//!
//! Sources (later overrides earlier on name collision):
//! 1. user: `<state root>/presets.toml`
//! 2. project: `<workspace>/<state-dir>/presets.toml`, only when the workspace
//!    is trusted (fail closed); a project preset may not select bypass mode.
//!
//! ```toml
//! [preset.review]
//! model = "anthropic/claude-sonnet-5-5"
//! effort = "high"
//! permission_mode = "ask"
//! tools = ["read_file", "grep"]
//! system_prompt = "Review only; never edit."
//! ```

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub effort: Option<String>,
    /// `ask` | `accept-edits` | `auto` | `bypass`
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub exclude_tools: Vec<String>,
    #[serde(default)]
    pub no_mcp: bool,
    /// Appended to the system prompt for the run.
    pub system_prompt: Option<String>,
    /// Default prompt when none is supplied.
    pub prompt: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PresetFile {
    #[serde(default)]
    preset: BTreeMap<String, Preset>,
}

fn parse_file(text: &str) -> Result<BTreeMap<String, Preset>, String> {
    toml::from_str::<PresetFile>(text)
        .map(|f| f.preset)
        .map_err(|e| format!("invalid presets file: {e}"))
}

fn is_bypass(mode: &str) -> bool {
    matches!(
        mode.trim().to_ascii_lowercase().as_str(),
        "bypass" | "yolo" | "bypasspermissions" | "bypass-permissions"
    )
}

/// Load presets from explicit file paths (testable core). Project presets are
/// only read when `project_trusted`.
pub fn load_presets_from(
    user_file: Option<&Path>,
    project_file: Option<&Path>,
    project_trusted: bool,
) -> Result<BTreeMap<String, Preset>, String> {
    let mut all = BTreeMap::new();
    if let Some(path) = user_file {
        if let Ok(text) = std::fs::read_to_string(path) {
            all.extend(parse_file(&text)?);
        }
    }
    if let (Some(path), true) = (project_file, project_trusted) {
        if let Ok(text) = std::fs::read_to_string(path) {
            for (name, mut preset) in parse_file(&text)? {
                if preset.permission_mode.as_deref().is_some_and(is_bypass) {
                    preset.permission_mode = None;
                }
                all.insert(name, preset);
            }
        }
    }
    Ok(all)
}

/// Load presets for a workspace using the real user dir and trust store.
pub fn load_presets(workspace_root: &Path) -> Result<BTreeMap<String, Preset>, String> {
    let user = crate::state_root().map(|r| r.join("presets.toml"));
    let project = workspace_root.join(".rightkit").join("presets.toml");
    load_presets_from(
        user.as_deref(),
        Some(&project),
        crate::is_workspace_trusted(workspace_root),
    )
}

/// Resolve one preset by name, with an error listing known names.
pub fn resolve_preset(presets: &BTreeMap<String, Preset>, name: &str) -> Result<Preset, String> {
    presets.get(name).cloned().ok_or_else(|| {
        let known: Vec<&str> = presets.keys().map(String::as_str).collect();
        let known = if known.is_empty() {
            "none".to_string()
        } else {
            known.join(", ")
        };
        format!("unknown preset `{name}` (known: {known})")
    })
}
