//! Per-server approval before spawn/connect. Ported from CodeRight's `trust_gate`, generalised to HTTP servers.

use std::path::Path;

use serde::{Deserialize, Serialize};

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

/// Format version bound into every fingerprint. Version 2 binds environment VALUES
/// as well as names; fingerprints from version 1 never match again, so existing
/// approvals are Pending and must be re-approved.
const FINGERPRINT_VERSION: u32 = 2;

/// Stable, value-sensitive identity of what would run/connect.
///
/// For stdio servers the canonical form covers command, args, cwd and every
/// environment variable name AND value (e.g. a changed `PATH` or `NODE_OPTIONS`
/// invalidates an approval). Only this SHA-256 digest is ever persisted or
/// surfaced; environment values themselves never leave the in-memory canonical
/// string, which is dropped after hashing.
pub fn fingerprint(spec: &ServerSpec) -> String {
    let canonical = match spec {
        ServerSpec::Stdio {
            command,
            args,
            env,
            cwd,
        } => json_canon(&serde_json::json!({
            "v": FINGERPRINT_VERSION,
            "kind": "stdio", "command": command, "args": args,
            "env": env,
            "cwd": cwd.as_ref().map(|c| c.to_string_lossy().to_string()),
        })),
        ServerSpec::Http { url } => json_canon(&serde_json::json!({
            "v": FINGERPRINT_VERSION, "kind": "http", "url": url
        })),
    };
    format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(canonical.as_bytes())
    )
}

fn json_canon(v: &serde_json::Value) -> String {
    v.to_string() // serde_json maps are sorted (BTreeMap) => deterministic
}

pub(crate) fn pending_for(workspace: &str, server: &str, spec: &ServerSpec) -> PendingApproval {
    let (command, args, env_keys) = match spec {
        ServerSpec::Stdio {
            command, args, env, ..
        } => (command.clone(), args.clone(), env.keys().cloned().collect()),
        ServerSpec::Http { url } => (url.clone(), vec![], vec![]),
    };
    PendingApproval {
        workspace: workspace.into(),
        server: server.into(),
        fingerprint: fingerprint(spec),
        command,
        args,
        env_keys,
    }
}

/// One decision brought in from a foreign store by [`TrustStore::try_load_migrating`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedTrust {
    pub workspace: String,
    pub server: String,
    /// The foreign fingerprint, kept verbatim. It matches only through
    /// [`TrustStore::decide_with_fingerprint`]; [`TrustStore::decide`] compares the RightKit
    /// fingerprint, so an imported record reads as `Pending` there (fails closed).
    pub fingerprint: String,
    pub approved: bool,
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

    /// The store at `path`. Only a missing file is an empty store (everything still gated).
    ///
    /// A file that exists and cannot be read, or does not parse, is an error
    /// (`InvalidData` for a parse failure). An empty store returned for such a file would be
    /// saved over the recorded decisions the next time one is made, and on Windows an open
    /// can be refused for a moment while another process replaces the file.
    pub fn try_load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// As [`TrustStore::try_load`], for a file that may hold another store's records.
    ///
    /// A file with a `records` array loads as a `TrustStore` (`migrate` is not called; a record
    /// that does not parse is `InvalidData`). Any other JSON document is passed whole to
    /// `migrate`: `Some(records)` becomes
    /// the store (timestamped now), `None` means the format is not recognised and the result is
    /// `InvalidData`, so the caller never saves an empty store over decisions it could not read.
    /// A missing file is an empty store, an unreadable one an `io::Error`, as in `try_load`.
    /// Nothing is written; call [`TrustStore::save`] to persist the migrated store.
    pub fn try_load_migrating(
        path: &Path,
        migrate: impl FnOnce(&serde_json::Value) -> Option<Vec<ImportedTrust>>,
    ) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error),
        };
        let invalid =
            |error: serde_json::Error| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
        let document: serde_json::Value = serde_json::from_str(&text).map_err(invalid)?;
        if document
            .get("records")
            .is_some_and(serde_json::Value::is_array)
        {
            return serde_json::from_value(document).map_err(invalid);
        }
        let imported = migrate(&document).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "trust store format not recognised",
            )
        })?;
        let mut store = Self::default();
        for record in imported {
            store.set_fingerprint(
                &record.workspace,
                &record.server,
                record.fingerprint,
                record.approved,
            );
        }
        Ok(store)
    }

    /// As [`TrustStore::try_load`], but every failure is an empty store.
    #[deprecated(
        note = "an unreadable or corrupt file reads as an empty store, which a later save writes over the recorded decisions; use `try_load`"
    )]
    pub fn load(path: &Path) -> Self {
        Self::try_load(path).unwrap_or_default()
    }

    /// Write the store to `path`: a temp file beside it, flushed, then renamed over it, so a
    /// reader or a crash never sees a partly written store.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        rightkit_fs::atomic_write(path, text.as_bytes()).map_err(std::io::Error::from)
    }

    pub fn decide(&self, workspace: &str, server: &str, spec: &ServerSpec) -> TrustDecision {
        self.decide_with_fingerprint(workspace, server, &fingerprint(spec))
    }

    /// As [`TrustStore::decide`] against a caller-computed fingerprint, for a host that keeps
    /// its own content hash (records written by [`TrustStore::approve_with_fingerprint`]). An
    /// empty fingerprint is never approved or denied: it is `Pending`.
    pub fn decide_with_fingerprint(
        &self,
        workspace: &str,
        server: &str,
        fingerprint: &str,
    ) -> TrustDecision {
        match self
            .records
            .iter()
            .find(|r| r.workspace == workspace && r.server == server)
        {
            Some(r) if !fingerprint.is_empty() && r.fingerprint == fingerprint => {
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
        self.set_fingerprint(workspace, server, fingerprint(spec), approved)
    }

    fn set_fingerprint(
        &mut self,
        workspace: &str,
        server: &str,
        fingerprint: String,
        approved: bool,
    ) {
        let rec = Record {
            workspace: workspace.into(),
            server: server.into(),
            fingerprint,
            approved,
            decided_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        match self
            .records
            .iter_mut()
            .find(|r| r.workspace == workspace && r.server == server)
        {
            Some(r) => *r = rec,
            None => self.records.push(rec),
        }
    }

    /// The decision for `name` from `source`, callable before anything is spawned or contacted.
    /// A [`ServerSource::User`] server is exempt (`Approved`), as in the pool and the client.
    pub fn gate(&self, source: &ServerSource, name: &str, spec: &ServerSpec) -> TrustDecision {
        match source {
            ServerSource::User => TrustDecision::Approved,
            ServerSource::Workspace(workspace) => self.decide(workspace, name, spec),
        }
    }

    /// [`TrustStore::gate`] for a workspace stdio server, before spawn. Any other spec is
    /// `Pending`: it cannot be approved through the stdio gate (fails closed).
    pub fn gate_stdio(&self, workspace: &str, name: &str, spec: &ServerSpec) -> TrustDecision {
        match spec {
            ServerSpec::Stdio { .. } => self.decide(workspace, name, spec),
            ServerSpec::Http { .. } => TrustDecision::Pending,
        }
    }

    /// True only when `source` may receive credentials for `name` as configured in `spec`: a
    /// user server, or a workspace server `Approved` for exactly this spec. `Pending`, `Denied`
    /// and a changed spec are all false.
    pub fn credential_write_allowed(
        &self,
        source: &ServerSource,
        name: &str,
        spec: &ServerSpec,
    ) -> bool {
        self.gate(source, name, spec) == TrustDecision::Approved
    }

    /// What a user is asked to approve for `name` in `workspace` (command, args, env names and
    /// the fingerprint; never env values).
    pub fn pending_approval(
        &self,
        workspace: &str,
        name: &str,
        spec: &ServerSpec,
    ) -> PendingApproval {
        pending_for(workspace, name, spec)
    }

    /// Records an approval bound to a caller-computed `fingerprint` instead of
    /// [`fingerprint`]`(spec)`, so a host with its own content hash keeps matching it through
    /// [`TrustStore::decide_with_fingerprint`]. An empty fingerprint is ignored (returns
    /// `false`); otherwise returns `true`. `decide` and `gate*` still compare the RightKit
    /// fingerprint, so such a record reads as `Pending` there.
    pub fn approve_with_fingerprint(
        &mut self,
        workspace: &str,
        server: &str,
        fingerprint: impl Into<String>,
    ) -> bool {
        self.set_decided(workspace, server, fingerprint.into(), true)
    }

    /// As [`TrustStore::approve_with_fingerprint`], recording a denial.
    pub fn deny_with_fingerprint(
        &mut self,
        workspace: &str,
        server: &str,
        fingerprint: impl Into<String>,
    ) -> bool {
        self.set_decided(workspace, server, fingerprint.into(), false)
    }

    fn set_decided(
        &mut self,
        workspace: &str,
        server: &str,
        fingerprint: String,
        approved: bool,
    ) -> bool {
        if fingerprint.is_empty() {
            return false;
        }
        self.set_fingerprint(workspace, server, fingerprint, approved);
        true
    }

    pub fn approve(&mut self, workspace: &str, server: &str, spec: &ServerSpec) {
        self.set(workspace, server, spec, true)
    }
    pub fn deny(&mut self, workspace: &str, server: &str, spec: &ServerSpec) {
        self.set(workspace, server, spec, false)
    }
}
