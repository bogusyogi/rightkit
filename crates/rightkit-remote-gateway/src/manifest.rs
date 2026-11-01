//! The tool manifest, `core/remote-tools.json`, included at build time. Settings and the CLI
//! read the same list; a tool that is not in it is refused, and so is one marked not built.

use super::{Refusal, Result};
use serde_json::Value;

const MANIFEST: &str = include_str!("../data/remote-tools.json");

/// One manifest row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub tier: u64,
    pub built: bool,
    /// The remote switch, approved folders and caps control it.
    pub gated: bool,
    /// The tool's name through the ChatGPT gateway (`the gateway`); `None` when the gateway
    /// does not offer it.
    pub gateway: Option<String>,
}

/// Every tool in the manifest. An unreadable manifest has none, so everything is refused.
pub fn tools() -> Vec<Tool> {
    let Ok(doc) = serde_json::from_str::<Value>(MANIFEST) else {
        return Vec::new();
    };
    doc["tools"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(Tool {
                        name: row["tool"].as_str()?.to_string(),
                        tier: row["tier"].as_u64()?,
                        built: row["built"].as_bool() == Some(true),
                        gated: row["gated"].as_bool() == Some(true),
                        gateway: row["gateway"].as_str().map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tool, when it is in the manifest and built; the refusal (`unknown_tool` or `not_built`)
/// otherwise.
pub fn require(name: &str) -> Result<Tool> {
    match tools().into_iter().find(|tool| tool.name == name) {
        None => Err(Refusal::new(
            "unknown_tool",
            format!("{name} is not in the app's remote tool manifest"),
        )),
        Some(tool) if !tool.built || !tool.gated => Err(Refusal::new(
            "not_built",
            format!("{name} is not built into this the app yet"),
        )),
        Some(tool) => Ok(tool),
    }
}
