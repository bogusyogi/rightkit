use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::PluginError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub entry: String,
    #[serde(default, alias = "providesTools")]
    pub provides_tools: Vec<String>,
    #[serde(default, alias = "providesCommands")]
    pub provides_commands: Vec<String>,
    #[serde(default)]
    pub tools: BTreeMap<String, PluginToolMetadata>,
    #[serde(default)]
    pub commands: BTreeMap<String, PluginCommandMetadata>,
    #[serde(default)]
    pub prompts: Vec<PluginPromptContribution>,
    #[serde(default)]
    pub middleware: Vec<PluginMiddlewareContribution>,
    /// TUI slots this plugin contributes to. Declared, not trusted: the host
    /// renders only declared slots (see [`PluginUiSlot`]).
    #[serde(default, alias = "uiSlots")]
    pub ui_slots: Vec<PluginUiContribution>,
    #[serde(default)]
    pub sandbox: PluginSandboxPolicy,
    pub enabled: bool,
}

/// The finite set of TUI surfaces a plugin may contribute to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum PluginUiSlot {
    Pane,
    Band,
    Status,
    Toast,
}

impl PluginUiSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginUiSlot::Pane => "pane",
            PluginUiSlot::Band => "band",
            PluginUiSlot::Status => "status",
            PluginUiSlot::Toast => "toast",
        }
    }
}

/// One declared contribution to a UI slot. Unknown slot names fail manifest
/// parsing instead of silently granting a surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginUiContribution {
    pub id: String,
    pub slot: PluginUiSlot,
    #[serde(default)]
    pub title: String,
    /// Plugin command that produces the slot's text when the host refreshes it.
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum PluginIsolationMode {
    TrustedSubprocess,
    #[default]
    RestrictedSubprocess,
    MetadataOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginToolMetadata {
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_input_schema")]
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub read_only: bool,
    /// Whether the tool may initiate network traffic. Permission policy gates
    /// this separately from ordinary subprocess execution.
    #[serde(default)]
    pub network_access: bool,
    /// Declared data classes the subprocess may access. Unknown values fail
    /// manifest parsing instead of silently expanding authority.
    #[serde(default)]
    pub data_access: Vec<PluginDataAccess>,
    #[serde(default = "default_output_media_type")]
    pub output_media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PluginDataAccess {
    WorkspaceRead,
    WorkspaceWrite,
    Secrets,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandMetadata {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub usage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginPromptContribution {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub applies_to_roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginMiddlewareContribution {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub phase: String,
    pub effect: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub requires_toolset: Option<String>,
    #[serde(default)]
    pub condition: Option<PluginMiddlewareCondition>,
    #[serde(default)]
    pub mutate_args: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub order: Option<u32>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginMiddlewareCondition {
    #[serde(default)]
    pub argument: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub equals: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSandboxPolicy {
    #[serde(default)]
    pub isolation: PluginIsolationMode,
    #[serde(default)]
    pub allow_absolute_entry: bool,
    #[serde(default = "default_plugin_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_inherit_env")]
    pub inherit_env: bool,
    #[serde(default)]
    pub allowed_env: Vec<String>,
    #[serde(default)]
    pub denied_env: Vec<String>,
    #[serde(default)]
    pub strip_network_env: bool,
}

impl Default for PluginSandboxPolicy {
    fn default() -> Self {
        Self {
            isolation: PluginIsolationMode::RestrictedSubprocess,
            allow_absolute_entry: false,
            timeout_ms: default_plugin_timeout_ms(),
            inherit_env: default_inherit_env(),
            allowed_env: Vec::new(),
            denied_env: Vec::new(),
            strip_network_env: false,
        }
    }
}

fn default_plugin_timeout_ms() -> u64 {
    30_000
}

fn default_inherit_env() -> bool {
    true
}

fn default_input_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": true
    })
}

fn default_output_media_type() -> String {
    "text/plain; charset=utf-8".to_string()
}

impl PluginManifest {
    pub fn from_json(s: &str) -> Result<Self, PluginError> {
        serde_json::from_str(s).map_err(|e| PluginError::Parse(e.to_string()))
    }

    /// Contributions declared for `slot`, in manifest order. A disabled
    /// plugin contributes nothing.
    pub fn ui_contributions(&self, slot: PluginUiSlot) -> Vec<&PluginUiContribution> {
        if !self.enabled {
            return Vec::new();
        }
        self.ui_slots.iter().filter(|c| c.slot == slot).collect()
    }

    /// Reject empty or duplicate slot ids and commands the manifest does not
    /// declare, so a slot cannot reach an undeclared capability.
    pub fn validate_ui_slots(&self) -> Result<(), PluginError> {
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.ui_slots {
            if c.id.trim().is_empty() {
                return Err(PluginError::Parse("ui slot id is empty".into()));
            }
            if !seen.insert(c.id.as_str()) {
                return Err(PluginError::Parse(format!("duplicate ui slot id {}", c.id)));
            }
            if let Some(cmd) = &c.command {
                if !self.provides_commands.contains(cmd) && !self.commands.contains_key(cmd) {
                    return Err(PluginError::Parse(format!(
                        "ui slot {} references undeclared command {cmd}",
                        c.id
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, PluginError> {
        serde_json::to_string_pretty(self).map_err(|e| PluginError::Parse(e.to_string()))
    }
}
