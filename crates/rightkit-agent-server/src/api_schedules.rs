use serde::{Deserialize, Serialize};

/// Supported bot schedule trigger kinds exposed through schedule APIs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    #[default]
    Cron,
    Once,
    Interval,
    Monitor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    #[default]
    Skip,
    QueueOne,
    BoundedParallel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    #[default]
    Skip,
    Coalesce,
    CatchUp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotProfileV1 {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub glyph: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_profile_version")]
    pub version: u32,
    pub role_pack: String,
    pub model: String,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub memory_policy: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub default_project: Option<String>,
    #[serde(default)]
    pub task_templates: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub tool_grants: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub browser: BrowserPolicy,
    #[serde(default)]
    pub budget: BotBudget,
    #[serde(default)]
    pub verifier: VerifierBinding,
    #[serde(default)]
    pub triggers: Vec<BotTriggerBinding>,
    #[serde(default)]
    pub concurrency: String,
    /// Hard upper bound for concurrent runs; policy string is retained as a
    /// wire-compatible label for older clients.
    #[serde(default = "default_concurrency_cap")]
    pub concurrency_cap: u32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_profile_version() -> u32 {
    1
}

fn default_enabled() -> bool {
    true
}

fn default_concurrency_cap() -> u32 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPolicy {
    pub mode: String,
    pub profile: Option<String>,
    pub allow_network: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotBudget {
    pub max_tokens: Option<u64>,
    pub max_seconds: Option<u64>,
    pub max_cost_micros: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifierBinding {
    pub role_pack: Option<String>,
    pub model: Option<String>,
    pub required: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerBinding {
    pub id: String,
    pub schedule: ScheduleKind,
    pub expression: Option<String>,
    pub paused: bool,
    #[serde(default)]
    pub misfire: MisfirePolicy,
    #[serde(default)]
    pub retry_limit: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleLifecycleState {
    pub paused: bool,
    pub overlap: String,
    pub misfire: String,
    pub idempotency_key: Option<String>,
    pub retry_limit: u32,
}

impl ScheduleLifecycleState {
    pub fn conservative() -> Self {
        Self {
            overlap: "skip".into(),
            misfire: "skip".into(),
            ..Self::default()
        }
    }
}

impl BotProfileV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported bot profile schema {}",
                self.schema_version
            ));
        }
        for (field, value) in [
            ("id", &self.id),
            ("name", &self.name),
            ("rolePack", &self.role_pack),
            ("model", &self.model),
        ] {
            if value.trim().is_empty() {
                return Err(format!("bot profile {field} is empty"));
            }
        }
        if self.id.len() > 128 || self.name.len() > 160 {
            return Err("bot profile identity is too long".into());
        }
        if self.version == 0 {
            return Err("bot profile version must be positive".into());
        }
        if self.browser.mode.trim().is_empty() {
            return Err("bot profile browser mode is empty".into());
        }
        if !matches!(
            self.concurrency.trim(),
            "skip" | "queue-one" | "bounded-parallel"
        ) {
            return Err(
                "bot profile concurrency must be skip, queue-one, or bounded-parallel".into(),
            );
        }
        if self.concurrency_cap == 0 || self.concurrency_cap > 64 {
            return Err("bot profile concurrency cap must be between 1 and 64".into());
        }
        let mut trigger_ids = std::collections::HashSet::new();
        for trigger in &self.triggers {
            if trigger.id.trim().is_empty() || !trigger_ids.insert(trigger.id.trim()) {
                return Err("bot trigger ids must be nonempty and unique".into());
            }
            if trigger
                .expression
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err("bot triggers require a schedule expression".into());
            }
        }
        Ok(())
    }
}

mod implementation;

pub use implementation::*;
