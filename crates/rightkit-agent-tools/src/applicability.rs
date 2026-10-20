//! The M22 applicability record: which role an app plays for the agent surface, tied to the real
//! entrypoints that prove it.
//!
//! An app declares the record under the key `M22` in its `rightkit-mandatory.json`:
//!
//! ```json
//! { "M22": { "role": "client", "entrypoints": ["src-tauri/src/agent.rs#register_tools"] } }
//! ```
//!
//! * `client`: the app drives other apps through a [`crate::Toolbox`] (a caller).
//! * `host`: the app serves agent calls (a hosted surface behind daemon approvals and service
//!   registration).
//! * `both`: it does both.
//! * `absent`: the app has no agent surface; it lists no entrypoints and declares the row not
//!   applicable with a reason.
//!
//! `right-release ownership-mandatory` reads the same record from Node and reports it. This module
//! is the Rust side: the same schema and the same rules, so an app (or its tests) can parse and
//! check its own record without Node. The record is report-only: it never grants or removes an
//! agent capability, and nothing in the toolbox gate reads it.
//!
//! Entrypoint syntax is `<relative file>`, optionally `#<symbol>` or `::<symbol>`. A bare token
//! with no path separator and no extension is a symbol or command name and cannot be checked
//! against the file system ([`ApplicabilityRecord::unverified_entrypoints`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};

/// The key of the record in `rightkit-mandatory.json`.
pub const APPLICABILITY_KEY: &str = "M22";

/// The role an app plays for the agent surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    /// Calls tools: the app owns a toolbox client.
    Client,
    /// Serves agent calls: a hosted surface.
    Host,
    /// Both a client and a host.
    Both,
    /// No agent surface at all.
    Absent,
}

impl AgentRole {
    /// The role as written in the record.
    pub fn label(self) -> &'static str {
        match self {
            AgentRole::Client => "client",
            AgentRole::Host => "host",
            AgentRole::Both => "both",
            AgentRole::Absent => "absent",
        }
    }
}

/// One app's M22 record: `{ "role": ..., "entrypoints": [...] }`. Unknown keys are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicabilityRecord {
    pub role: AgentRole,
    pub entrypoints: Vec<String>,
}

/// Every problem found in a record, in the order found. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicabilityError {
    pub problems: Vec<String>,
}

impl fmt::Display for ApplicabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid M22 applicability record: {}",
            self.problems.join("; ")
        )
    }
}

impl std::error::Error for ApplicabilityError {}

fn invalid(problem: impl Into<String>) -> ApplicabilityError {
    ApplicabilityError {
        problems: vec![problem.into()],
    }
}

/// The file part of an entrypoint: everything before the first `#` or `::`.
fn entrypoint_file(entry: &str) -> &str {
    let end = [entry.find('#'), entry.find("::")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(entry.len());
    &entry[..end]
}

/// A file part that looks like a path (has a separator or an extension) can be checked on disk;
/// a bare token is a symbol or command name.
fn looks_like_path(text: &str) -> bool {
    if text.contains(['/', '\\']) {
        return true;
    }
    match text.rsplit_once('.') {
        Some((_, ext)) => !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()),
        None => false,
    }
}

/// An absolute path, a drive-letter path, or any `..` component.
fn escapes_app_root(file: &str) -> bool {
    let normalized = file.replace('\\', "/");
    let drive = {
        let mut chars = file.chars();
        matches!((chars.next(), chars.next()), (Some(a), Some(':')) if a.is_ascii_alphabetic())
    };
    Path::new(file).is_absolute()
        || normalized.starts_with('/')
        || drive
        || Path::new(&normalized)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}

impl ApplicabilityRecord {
    /// Parse one record value. Fails with every shape and consistency problem found, never
    /// panics: a non-object, an unknown key, a role outside `client|host|both|absent`, a missing
    /// or non-array `entrypoints`, an empty or duplicate entrypoint, an entrypoint that names no
    /// file or symbol, a path that is absolute or leaves the app, `absent` with entrypoints, or
    /// `client`/`host`/`both` with none.
    pub fn from_value(value: &Value) -> Result<Self, ApplicabilityError> {
        let Some(obj) = value.as_object() else {
            return Err(invalid(
                "applicability record must be an object { role, entrypoints }",
            ));
        };
        let mut problems = Vec::new();
        let mut extra: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|k| *k != "role" && *k != "entrypoints")
            .collect();
        extra.sort_unstable();
        if !extra.is_empty() {
            problems.push(format!(
                "unknown applicability record key(s): {}",
                extra.join(", ")
            ));
        }
        let role = match obj.get("role").and_then(Value::as_str) {
            Some("client") => Some(AgentRole::Client),
            Some("host") => Some(AgentRole::Host),
            Some("both") => Some(AgentRole::Both),
            Some("absent") => Some(AgentRole::Absent),
            _ => {
                problems.push(format!(
                    "role must be one of client|host|both|absent, got {}",
                    obj.get("role")
                        .map_or("nothing".to_string(), Value::to_string)
                ));
                None
            }
        };
        let mut entrypoints = Vec::new();
        match obj.get("entrypoints").and_then(Value::as_array) {
            None => problems.push("entrypoints must be an array".into()),
            Some(list) => {
                let mut seen = BTreeSet::new();
                for item in list {
                    let Some(text) = item.as_str().map(str::trim).filter(|t| !t.is_empty()) else {
                        problems.push("every entrypoint must be a non-empty string".into());
                        continue;
                    };
                    if !seen.insert(text.to_string()) {
                        problems.push(format!("entrypoint {text:?} is listed twice"));
                        continue;
                    }
                    entrypoints.push(text.to_string());
                    let file = entrypoint_file(text);
                    if file.is_empty() {
                        problems.push(format!("entrypoint {text:?} names no file or symbol"));
                    } else if looks_like_path(file) && escapes_app_root(file) {
                        problems.push(format!(
                            "entrypoint {text:?} must be a relative path inside the app"
                        ));
                    }
                }
                match role {
                    Some(AgentRole::Absent) if !list.is_empty() => {
                        problems.push("role absent must list no entrypoints".into())
                    }
                    Some(r) if r != AgentRole::Absent && list.is_empty() => problems.push(format!(
                        "role {} needs at least one real entrypoint",
                        r.label()
                    )),
                    _ => {}
                }
            }
        }
        match (problems.is_empty(), role) {
            (true, Some(role)) => Ok(Self { role, entrypoints }),
            _ => Err(ApplicabilityError { problems }),
        }
    }

    /// The `M22` record of a `rightkit-mandatory.json` text: `Ok(None)` when the file has no
    /// `M22` key (the record is optional), `Err` when the text is not JSON, is not an object, or
    /// the record is invalid.
    pub fn from_manifest(text: &str) -> Result<Option<Self>, ApplicabilityError> {
        let doc: Value = serde_json::from_str(text)
            .map_err(|e| invalid(format!("rightkit-mandatory.json is not valid JSON: {e}")))?;
        let Some(obj) = doc.as_object() else {
            return Err(invalid("rightkit-mandatory.json must be a JSON object"));
        };
        obj.get(APPLICABILITY_KEY).map(Self::from_value).transpose()
    }

    /// Entrypoints that name only a symbol or command (no path separator, no extension): they
    /// are recorded but cannot be verified on disk.
    pub fn unverified_entrypoints(&self) -> usize {
        self.entrypoints
            .iter()
            .filter(|e| !looks_like_path(entrypoint_file(e)))
            .count()
    }

    /// Check that every path entrypoint names an existing file under `app_root`. Returns one
    /// problem per missing file (empty when all exist). Symbols are not checked.
    pub fn missing_entrypoint_files(&self, app_root: &Path) -> Vec<String> {
        self.entrypoints
            .iter()
            .filter_map(|entry| {
                let file = entrypoint_file(entry);
                if !looks_like_path(file) {
                    return None;
                }
                let on_disk = app_root.join(file.replace('\\', "/"));
                (!on_disk.is_file())
                    .then(|| format!("entrypoint {entry:?}: file not found under the app root"))
            })
            .collect()
    }

    /// Whether the app declares an agent surface at all (any role but `absent`).
    pub fn has_agent_surface(&self) -> bool {
        self.role != AgentRole::Absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn problems(value: Value) -> Vec<String> {
        ApplicabilityRecord::from_value(&value)
            .expect_err("record must be invalid")
            .problems
    }

    #[test]
    fn valid_records_parse_for_every_role() {
        for (role, entrypoints) in [
            ("client", json!(["src-tauri/src/agent.rs#register_tools"])),
            ("host", json!(["src/host.rs::serve", "register_service"])),
            ("both", json!(["a/b.rs", "c.mjs#d"])),
            ("absent", json!([])),
        ] {
            let r =
                ApplicabilityRecord::from_value(&json!({"role": role, "entrypoints": entrypoints}))
                    .unwrap_or_else(|e| panic!("{role}: {e}"));
            assert_eq!(r.role.label(), role);
            assert_eq!(r.has_agent_surface(), role != "absent");
        }
        let both = ApplicabilityRecord::from_value(
            &json!({"role": "host", "entrypoints": ["src/host.rs::serve", " register_service "]}),
        )
        .unwrap();
        assert_eq!(both.entrypoints, ["src/host.rs::serve", "register_service"]);
        assert_eq!(both.unverified_entrypoints(), 1);
    }

    #[test]
    fn bad_shapes_roles_and_entrypoints_are_reported_not_thrown() {
        assert!(problems(json!("client"))[0].contains("must be an object"));
        assert!(problems(json!(null))[0].contains("must be an object"));
        assert!(
            problems(json!({"role": "client", "entrypoints": ["x.rs"], "extra": 1}))
                .iter()
                .any(|p| p.contains("unknown applicability record key(s): extra"))
        );
        assert!(problems(json!({"role": "server", "entrypoints": ["x.rs"]}))
            .iter()
            .any(|p| p.contains("role must be one of client|host|both|absent")));
        assert!(problems(json!({"entrypoints": ["x.rs"]}))
            .iter()
            .any(|p| p.contains("role must be one of")));
        assert!(problems(json!({"role": "client"}))
            .iter()
            .any(|p| p.contains("entrypoints must be an array")));
        assert!(problems(json!({"role": "client", "entrypoints": "x.rs"}))
            .iter()
            .any(|p| p.contains("entrypoints must be an array")));
        for bad in [json!(""), json!("  "), json!(3), json!(null)] {
            assert!(problems(json!({"role": "client", "entrypoints": [bad]}))
                .iter()
                .any(|p| p.contains("non-empty string")));
        }
        assert!(
            problems(json!({"role": "client", "entrypoints": ["x.rs", "x.rs"]}))
                .iter()
                .any(|p| p.contains("listed twice"))
        );
        assert!(
            problems(json!({"role": "client", "entrypoints": ["#only_symbol"]}))
                .iter()
                .any(|p| p.contains("names no file or symbol"))
        );
        assert!(
            problems(json!({"role": "client", "entrypoints": ["::sym"]}))
                .iter()
                .any(|p| p.contains("names no file or symbol"))
        );
    }

    #[test]
    fn entrypoints_cannot_leave_the_app() {
        for escape in [
            "../other/agent.rs",
            "a/../../b.rs",
            "a\\..\\..\\b.rs",
            "/etc/agent.rs",
            "C:\\app\\agent.rs",
            "C:/app/agent.rs#f",
        ] {
            assert!(
                problems(json!({"role": "client", "entrypoints": [escape]}))
                    .iter()
                    .any(|p| p.contains("relative path inside the app")),
                "{escape}"
            );
        }
        // A dotted name with no separator is a path-like file, not an escape.
        assert!(ApplicabilityRecord::from_value(
            &json!({"role": "client", "entrypoints": ["agent.rs"]})
        )
        .is_ok());
    }

    #[test]
    fn role_and_entrypoints_must_agree() {
        assert!(problems(json!({"role": "absent", "entrypoints": ["x.rs"]}))
            .iter()
            .any(|p| p.contains("role absent must list no entrypoints")));
        for role in ["client", "host", "both"] {
            assert!(
                problems(json!({"role": role, "entrypoints": []}))
                    .iter()
                    .any(|p| p.contains("needs at least one real entrypoint")),
                "{role}"
            );
        }
    }

    #[test]
    fn a_manifest_without_the_key_has_no_record_and_a_bad_one_is_an_error() {
        assert_eq!(
            ApplicabilityRecord::from_manifest(r#"{"profile": "desktop"}"#).unwrap(),
            None
        );
        let r = ApplicabilityRecord::from_manifest(
            r#"{"profile": "desktop", "M22": {"role": "client", "entrypoints": ["src/a.rs"]}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.role, AgentRole::Client);
        assert!(ApplicabilityRecord::from_manifest("not json").is_err());
        assert!(ApplicabilityRecord::from_manifest("[]").is_err());
        assert!(ApplicabilityRecord::from_manifest(r#"{"M22": {"role": "x"}}"#).is_err());
        // The serde form refuses unknown keys too.
        assert!(serde_json::from_str::<ApplicabilityRecord>(
            r#"{"role":"client","entrypoints":["a.rs"],"x":1}"#
        )
        .is_err());
    }

    #[test]
    fn path_entrypoints_are_checked_on_disk_and_symbols_are_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/agent.rs"), "").unwrap();
        let r = ApplicabilityRecord::from_value(&json!({
            "role": "both",
            "entrypoints": ["src/agent.rs#register", "src\\agent.rs::other", "src/gone.rs", "register_service"]
        }))
        .unwrap();
        let missing = r.missing_entrypoint_files(dir.path());
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("src/gone.rs"));
        assert_eq!(r.unverified_entrypoints(), 1);
    }
}
