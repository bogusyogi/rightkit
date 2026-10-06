//! Per-server approval before spawn/connect. Ported from CodeRight's `trust_gate`, generalised to HTTP servers.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client::ServerSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSource {
    /// Configured by the user directly: exempt from the gate.
    User,
    /// Declared by a workspace file (id/path of the workspace): gated.
    Workspace(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    Approved,
    Denied,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub workspace: String,
    pub server: String,
    pub fingerprint: String,
    pub command: String,
    pub args: Vec<String>,
    /// Environment variable NAMES only; values are never surfaced or persisted.
    pub env_keys: Vec<String>,
}

/// Stable identity of what would run/connect: kind + command + args + env key names, or the URL.
pub fn fingerprint(spec: &ServerSpec) -> String {
    let canonical = match spec {
        ServerSpec::Stdio { command, args, env, cwd } => json_canon(&serde_json::json!({
            "kind": "stdio", "command": command, "args": args,
            "envKeys": env.keys().collect::<Vec<_>>(),
            "cwd": cwd.as_ref().map(|c| c.to_string_lossy().to_string()),
        })),
        ServerSpec::Http { url } => json_canon(&serde_json::json!({"kind": "http", "url": url})),
    };
    let d = Sha256::digest(canonical.as_bytes());
    format!("sha256:{}", d.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn json_canon(v: &serde_json::Value) -> String {
    v.to_string() // serde_json maps are sorted (BTreeMap) => deterministic
}

pub(crate) fn pending_for(workspace: &str, server: &str, spec: &ServerSpec) -> PendingApproval {
    let (command, args, env_keys) = match spec {
        ServerSpec::Stdio { command, args, env, .. } => {
            (command.clone(), args.clone(), env.keys().cloned().collect())
        }
        ServerSpec::Http { url } => (url.clone(), vec![], vec![]),
    };
    PendingApproval { workspace: workspace.into(), server: server.into(), fingerprint: fingerprint(spec), command, args, env_keys }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    workspace: String,
    server: String,
    fingerprint: String,
    approved: bool,
    #[serde(default)]
    decided_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    records: Vec<Record>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Missing or corrupt file => empty store (everything still gated).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into()))
    }

    pub fn decide(&self, workspace: &str, server: &str, spec: &ServerSpec) -> TrustDecision {
        let fp = fingerprint(spec);
        match self.records.iter().find(|r| r.workspace == workspace && r.server == server) {
            Some(r) if r.fingerprint == fp => {
                if r.approved {
                    TrustDecision::Approved
                } else {
                    TrustDecision::Denied
                }
            }
            _ => TrustDecision::Pending,
        }
    }

    fn set(&mut self, workspace: &str, server: &str, spec: &ServerSpec, approved: bool) {
        let rec = Record {
            workspace: workspace.into(),
            server: server.into(),
            fingerprint: fingerprint(spec),
            approved,
            decided_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        match self.records.iter_mut().find(|r| r.workspace == workspace && r.server == server) {
            Some(r) => *r = rec,
            None => self.records.push(rec),
        }
    }

    pub fn approve(&mut self, workspace: &str, server: &str, spec: &ServerSpec) {
        self.set(workspace, server, spec, true)
    }
    pub fn deny(&mut self, workspace: &str, server: &str, spec: &ServerSpec) {
        self.set(workspace, server, spec, false)
    }
}
