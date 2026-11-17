//! Typed declaration of an app CLI contract. This validates declarations, not runtime behavior.
use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const APP_CONTRACT_SCHEMA_ID: &str = "rightkit-agent-tools/app-contract/1";
pub const APP_CONTRACT_VERSION: u32 = 2;
pub const APP_CONTRACT_MAX_BYTES: usize = 1024 * 1024;
pub const APP_CONTRACT_SCHEMA: &str = include_str!("app-contract.schema.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppContract {
    pub contract: String,
    pub contract_version: u32,
    pub schema: String,
    pub app: String,
    pub app_version: String,
    pub bundle_id: String,
    pub platforms: Vec<String>,
    pub exit_codes: Value,
    pub conventions: Value,
    pub tools: Vec<AppTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppTool {
    pub name: String,
    pub summary: String,
    pub input_schema: Value,
    pub output_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events_schema: Option<Value>,
    pub effect: AppEffect,
    pub approval: AppApproval,
    pub availability: AppAvailability,
    pub licence: String,
    pub long_running: bool,
    pub idempotent: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEffect { Read, Write, External, Spend, Destroy }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppApproval { None, Yes, InApp }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppAvailability { Engine, Host }

impl AppContract {
    /// Parse a size-bounded declaration and enforce its structural and policy rules.
    pub fn parse(json: &str) -> Result<Self, String> {
        if json.len() > APP_CONTRACT_MAX_BYTES { return Err("contract exceeds size limit".into()); }
        let contract: Self = serde_json::from_str(json).map_err(|_| "invalid contract JSON".to_string())?;
        contract.validate()?;
        Ok(contract)
    }

    /// Validate a declaration; does not certify its schemas or the implementation behind it.
    pub fn validate(&self) -> Result<(), String> {
        if self.contract_version != APP_CONTRACT_VERSION || self.schema != APP_CONTRACT_SCHEMA_ID {
            return Err("unsupported contract version or schema".into());
        }
        if [self.app.as_str(), self.app_version.as_str(), self.bundle_id.as_str()].iter().any(|s| s.trim().is_empty())
            || self.contract != format!("{}-cli", self.app)
            || self.platforms.is_empty() || self.platforms.iter().any(|s| s.trim().is_empty())
            || !self.exit_codes.is_object() || !self.conventions.is_object() {
            return Err("invalid contract metadata".into());
        }
        let mut names = HashSet::new();
        for tool in &self.tools {
            if tool.name.trim().is_empty() || tool.summary.trim().is_empty() || tool.licence.trim().is_empty()
                || !tool.input_schema.is_object() || !tool.output_schema.is_object()
                || tool.events_schema.as_ref().is_some_and(|s| !s.is_object())
                || !names.insert(&tool.name) || crate::exposure::forbidden(&tool.name).is_some() {
                return Err("invalid or forbidden tool declaration".into());
            }
            let allowed = match tool.effect {
                AppEffect::Read | AppEffect::Write => true,
                AppEffect::External | AppEffect::Destroy => tool.approval != AppApproval::None,
                AppEffect::Spend => tool.approval == AppApproval::InApp,
            };
            if !allowed { return Err("approval below effect floor".into()); }
        }
        Ok(())
    }

    /// Serialize a valid declaration for the host or release checker.
    pub fn export(&self) -> Result<String, String> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|_| "cannot encode contract".to_string())?;
        if json.len() > APP_CONTRACT_MAX_BYTES { return Err("contract exceeds size limit".into()); }
        Ok(json)
    }
}
