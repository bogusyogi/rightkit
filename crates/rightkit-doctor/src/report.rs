//! The engine doctor report envelope: stable identity plus live readiness.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::row::{readiness, Row};

/// Who the engine is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Product name (`demo`).
    pub app: String,
    /// Engine binary name (`demo-engine`).
    pub engine: String,
    /// Engine version.
    pub version: String,
    /// Version of the CLI/sidecar contract.
    pub contract_version: u32,
}

impl Identity {
    /// An identity at contract version 1.
    pub fn new(
        app: impl Into<String>,
        engine: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            engine: engine.into(),
            version: version.into(),
            contract_version: 1,
        }
    }

    /// Replace the contract version.
    pub fn with_contract_version(mut self, version: u32) -> Self {
        self.contract_version = version;
        self
    }
}

/// `[{name, version}]` for `(name, version)` pairs.
pub fn capability_list<S: AsRef<str>>(pairs: &[(S, u32)]) -> Vec<Value> {
    pairs
        .iter()
        .map(|(name, version)| json!({ "name": name.as_ref(), "version": version }))
        .collect()
}

/// What a report is built from.
#[derive(Debug, Clone)]
pub struct EngineReport<'a> {
    /// Identity.
    pub identity: &'a Identity,
    /// Capabilities as `(name, version)`.
    pub capabilities: Vec<(String, u32)>,
    /// Checks.
    pub rows: Vec<Row>,
    /// Data folder in use, when the engine has one.
    pub data_dir: Option<&'a Path>,
    /// Resource folder, when it resolves.
    pub resource_dir: Option<&'a Path>,
    /// `--dry-run` was passed.
    pub dry_run: bool,
    /// Extra top-level members (the app's own sections); they never replace the standard ones.
    pub extra: Map<String, Value>,
}

impl EngineReport<'_> {
    /// The report: `app`, `engine`, `app_version`, `contract_version`, `capabilities`,
    /// `readiness` (`status` and `checks`), `paths`, `platform`, `dry_run`, then the extras.
    pub fn build(&self) -> Value {
        let exe = std::env::current_exe().ok();
        let display = |p: &Path| p.display().to_string();
        let mut map = Map::new();
        map.insert("app".into(), json!(self.identity.app));
        map.insert("engine".into(), json!(self.identity.engine));
        map.insert("app_version".into(), json!(self.identity.version));
        map.insert(
            "contract_version".into(),
            json!(self.identity.contract_version),
        );
        map.insert(
            "capabilities".into(),
            Value::Array(capability_list(&self.capabilities)),
        );
        map.insert("readiness".into(), readiness(&self.rows));
        map.insert(
            "paths".into(),
            json!({
                "executable": exe.as_deref().map(display),
                "resource_dir": self.resource_dir.map(display),
                "data_dir": self.data_dir.map(display),
            }),
        );
        map.insert(
            "platform".into(),
            json!({ "os": std::env::consts::OS, "arch": std::env::consts::ARCH }),
        );
        map.insert("dry_run".into(), json!(self.dry_run));
        for (key, value) in &self.extra {
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
        Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_has_the_standard_members_and_extras_do_not_replace_them() {
        let identity = Identity::new("Demo", "demo-engine", "1.2.3");
        let mut extra = Map::new();
        extra.insert("app".into(), json!("hijack"));
        extra.insert("library_dir".into(), json!("lib"));
        let report = EngineReport {
            identity: &identity,
            capabilities: vec![("doctor".into(), 1)],
            rows: vec![Row::ok("data_dir", true, "writable")],
            data_dir: Some(Path::new("data")),
            resource_dir: None,
            dry_run: true,
            extra,
        }
        .build();
        assert_eq!(report["app"], "Demo");
        assert_eq!(report["library_dir"], "lib");
        assert_eq!(report["contract_version"], 1);
        assert_eq!(report["readiness"]["status"], "ready");
        assert_eq!(report["capabilities"][0]["name"], "doctor");
        assert_eq!(report["dry_run"], true);
        assert!(report["paths"]["resource_dir"].is_null());
    }
}
