//! Wrapper over Pulse's `membrane` CLI (issue #26 lane A).
//!
//! Contract source: Pulse `membrane/README.md` "CLI contract" and "Finding the binary", and
//! `membrane/engine/crates/membrane/src/{cli.rs,modes.rs}` at Pulse commit 858972ca6. Every
//! `--json` document carries `"schema": "membrane.<command>.v<N>"`; this adapter parses by
//! schema id and rejects any major version other than [`SUPPORTED_MAJOR`]. Exit codes map to
//! [`MembraneError`] (0 ok, 1 internal, 2 usage or refusal, 3 not available, 4 resident
//! unreachable). A cursor conflict on `transcript append` is exit 2 with a conflict document on
//! stdout; it is returned as [`TranscriptAppend::Conflict`], not as an error.
//!
//! Discovery: env override, `notch-state.json` `membraneBinary.path`
//! (`{"path": null, "reason": ..}` means not installed), a fresh `resident.json` `binary`, then
//! the installed Helpers path; [`MembraneCli::backend`] verifies the candidate with
//! `membrane where --json` (`binary` must equal the path that was run).

use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Environment override for the binary location.
pub const BIN_ENV: &str = "RIGHTKIT_MEMBRANE_BIN";
/// Reason reported when Pulse's Membrane is not installed.
pub const OFF_REASON: &str = "off: pulse not installed";
/// Default hard timeout for one invocation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// The only schema major version this adapter understands.
pub const SUPPORTED_MAJOR: u32 = 1;

/// Pulse `membrane` exit codes (`membrane/src/lib.rs`).
pub const EXIT_OK: i32 = 0;
pub const EXIT_INTERNAL_ERROR: i32 = 1;
pub const EXIT_USER_ERROR: i32 = 2;
pub const EXIT_NOT_AVAILABLE: i32 = 3;
pub const EXIT_RESIDENT_UNREACHABLE: i32 = 4;

/// `resident.json` older than this is stale (README "Finding the binary").
const RESIDENT_STALE_MS: u128 = 30_000;

#[derive(Debug, thiserror::Error)]
pub enum MembraneError {
    /// Pulse is not installed (no binary found).
    #[error("{0}")]
    Unavailable(String),
    #[error("cannot start membrane: {0}")]
    Spawn(String),
    #[error("membrane timed out after {0:?} and was killed")]
    Timeout(Duration),
    /// Exit 1: unreadable or corrupt store, encoding failure.
    #[error("membrane internal error: {0}")]
    Internal(String),
    /// Exit 2: bad command, flag or value.
    #[error("membrane usage error: {0}")]
    Usage(String),
    /// Exit 3: the answer cannot be produced (no store, not enrolled, recall off, no graph).
    #[error("membrane not available: {0}")]
    NotAvailable(String),
    /// Exit 4: only `status --require-resident`.
    #[error("membrane resident unreachable: {0}")]
    ResidentUnreachable(String),
    /// Any exit code outside the documented set.
    #[error("membrane exited with {code:?}: {stderr}")]
    Exit { code: Option<i32>, stderr: String },
    #[error("cannot parse membrane output: {0}")]
    Parse(String),
    /// A document whose schema major version is not [`SUPPORTED_MAJOR`].
    #[error("unsupported membrane schema `{got}` (supported major: v{SUPPORTED_MAJOR})")]
    UnsupportedSchema { got: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPull {
    /// Context text; empty when Membrane had nothing or is off.
    pub text: String,
    /// Set when context is off or not available, e.g. [`OFF_REASON`].
    pub off_reason: Option<String>,
    /// Retrieval reason from `membrane.context.pull.v1` (`empty` or the retrieval reason).
    pub reason: Option<String>,
    /// Reason detail (object or string), `Null` when absent.
    pub detail: Value,
}

/// One row of `memory list` / `memory search`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryItem {
    pub id: String,
    pub tier: String,
    pub chars: Option<u64>,
    pub access: Value,
    pub inject: Value,
    /// Search only: the memory's scope.
    pub scope: Option<String>,
    /// Search only.
    pub score: Option<f64>,
    /// Search only: the memory text.
    pub content: Option<String>,
}

/// `membrane.memory.list.v1`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryList {
    pub scope: Option<String>,
    pub limit: u64,
    pub items: Vec<MemoryItem>,
    pub completeness: Value,
}

/// `membrane.memory.search.v1`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearch {
    pub query: Option<String>,
    /// Tier names asked for; empty means every tier.
    pub tiers: Vec<String>,
    pub scope: Option<String>,
    pub limit: Option<u64>,
    /// Candidates ranked before the tier filter.
    pub pool: Option<u64>,
    /// True when more memories matched than `limit`.
    pub more: bool,
    pub items: Vec<MemoryItem>,
}

/// `membrane.memory.delete.v1`. `deleted` is false (exit 3) when no memory had the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDelete {
    pub id: String,
    pub deleted: bool,
}

/// `membrane.blueprint.v1` / `membrane.blueprint.search.v1`: the Blueprint answer as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintAnswer {
    pub schema: String,
    /// A `schema` the Blueprint answer carried itself.
    pub payload_schema: Option<String>,
    /// Remaining fields of the document.
    pub body: Value,
}

/// `membrane.status.v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    pub product_version: String,
    /// `resident`, `in_process`, or `off` from [`Unavailable`].
    pub mode: String,
    pub state_root: Option<String>,
    pub db_path: Option<String>,
    pub db_present: Option<bool>,
    pub model: Option<String>,
    pub resident: Value,
    /// The whole document, for fields this adapter does not type.
    pub raw: Value,
}

/// `membrane.where.v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhereInfo {
    pub binary: PathBuf,
    pub product_version: Option<String>,
    pub membrane_source_commit: Option<String>,
    pub target: Option<String>,
    pub state_root: Option<String>,
}

/// Result of `membrane transcript append`.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptAppend {
    /// Exit 0. `replayed` is true when the batch was an exact replay of committed rows.
    Committed {
        session: String,
        cursor: u64,
        replayed: bool,
        /// `streamId`, `inserted`, `alreadyPresent`, `lastSeq`, `lastSourceSeq`, `prefixDigest`.
        receipt: Value,
    },
    /// Exit 2 with `conflict: true`; nothing was written.
    Conflict(TranscriptConflict),
}

/// The conflict receipt of `transcript append`.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptConflict {
    /// The stream's committed end; `None` when the tip could not be read.
    pub current_cursor: Option<u64>,
    /// 400, 409 or 410.
    pub status: Option<u64>,
    /// `transcript_cursor_mismatch`, `transcript_event_conflict`, `transcript_session_gone`,
    /// `transcript_tip_unavailable`, or the refusal's own code.
    pub code: String,
    pub detail: Value,
}

/// `membrane.transcript.tip.v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptTip {
    pub session: String,
    /// The stream's committed end; 0 for an empty stream.
    pub current_cursor: u64,
    pub prefix_digest: Option<String>,
}

pub trait MembraneBackend {
    fn context_pull(
        &self,
        cwd: &Path,
        prompt: &str,
        budget: Option<usize>,
        repo: Option<&Path>,
    ) -> Result<ContextPull, MembraneError>;
    fn memory_push(&self, text: &str) -> Result<Value, MembraneError>;
    fn memory_list(&self, limit: usize, repo: Option<&Path>) -> Result<MemoryList, MembraneError>;
    fn memory_search(
        &self,
        query: &str,
        tiers: &[String],
        limit: usize,
        repo: Option<&Path>,
    ) -> Result<MemorySearch, MembraneError>;
    fn memory_delete(&self, id: &str) -> Result<MemoryDelete, MembraneError>;
    fn blueprint_search(
        &self,
        q: &str,
        limit: usize,
        repo: Option<&Path>,
    ) -> Result<BlueprintAnswer, MembraneError>;
    fn status(&self) -> Result<StatusInfo, MembraneError>;
    /// `adapt observe`: an `AdaptObservationRequestV1` document on stdin. Returns `result`.
    fn adapt_observe(&self, request: &Value) -> Result<Value, MembraneError>;
    /// `adapt recall <query> -k n --scope s --dimension K=V`. Returns `result`.
    fn adapt_recall(
        &self,
        query: &str,
        k: usize,
        scope: &str,
        dimensions: &[String],
    ) -> Result<Value, MembraneError>;
    /// `adapt apply --insights <file> --installation-id <id> [--dry-run]`. Returns `result`.
    fn adapt_apply_insights(
        &self,
        insights: &Path,
        installation_id: &str,
        dry_run: bool,
    ) -> Result<Value, MembraneError>;
    /// `adapt apply --manifest <file> [--dry-run]`. Returns `result`.
    fn adapt_apply_manifest(&self, manifest: &Path, dry_run: bool) -> Result<Value, MembraneError>;
    /// `adapt taste-review --input <file> --installation-id <id> --created-at <t>
    /// [--canonical-pool-sha256 <d>]`. Returns `result`.
    fn adapt_taste_review(
        &self,
        input: &Path,
        installation_id: &str,
        created_at: &str,
        canonical_pool_sha256: Option<&str>,
    ) -> Result<Value, MembraneError>;
    /// `transcript append`: cursor-checked; a conflict is [`TranscriptAppend::Conflict`].
    /// `events` is the JSON array of `host_event_v1` rows.
    fn transcript_append(
        &self,
        session: &str,
        cursor: u64,
        scope: Option<&str>,
        events: &Value,
    ) -> Result<TranscriptAppend, MembraneError>;
    /// `transcript tip`; a forgotten session is exit 2, [`MembraneError::Usage`].
    fn transcript_tip(&self, session: &str) -> Result<TranscriptTip, MembraneError>;
}

/// Backend used when Pulse is not installed: empty context, explicit errors.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl Unavailable {
    fn err() -> MembraneError {
        MembraneError::Unavailable(OFF_REASON.to_owned())
    }
}

impl MembraneBackend for Unavailable {
    fn context_pull(
        &self,
        _: &Path,
        _: &str,
        _: Option<usize>,
        _: Option<&Path>,
    ) -> Result<ContextPull, MembraneError> {
        Ok(ContextPull {
            text: String::new(),
            off_reason: Some(OFF_REASON.to_owned()),
            reason: None,
            detail: Value::Null,
        })
    }
    fn memory_push(&self, _: &str) -> Result<Value, MembraneError> {
        Err(Self::err())
    }
    fn memory_list(&self, limit: usize, _: Option<&Path>) -> Result<MemoryList, MembraneError> {
        Ok(MemoryList {
            scope: None,
            limit: limit as u64,
            items: Vec::new(),
            completeness: Value::Null,
        })
    }
    fn memory_search(
        &self,
        query: &str,
        _: &[String],
        limit: usize,
        _: Option<&Path>,
    ) -> Result<MemorySearch, MembraneError> {
        Ok(empty_search(query, limit))
    }
    fn memory_delete(&self, _: &str) -> Result<MemoryDelete, MembraneError> {
        Err(Self::err())
    }
    fn blueprint_search(
        &self,
        _: &str,
        _: usize,
        _: Option<&Path>,
    ) -> Result<BlueprintAnswer, MembraneError> {
        Err(Self::err())
    }
    fn status(&self) -> Result<StatusInfo, MembraneError> {
        let raw = serde_json::json!({"mode": "off", "reason": OFF_REASON});
        Ok(StatusInfo {
            product_version: String::new(),
            mode: "off".to_owned(),
            state_root: None,
            db_path: None,
            db_present: None,
            model: None,
            resident: Value::Null,
            raw,
        })
    }
    fn adapt_observe(&self, _: &Value) -> Result<Value, MembraneError> {
        Err(Self::err())
    }
    fn adapt_recall(
        &self,
        _: &str,
        _: usize,
        _: &str,
        _: &[String],
    ) -> Result<Value, MembraneError> {
        Err(Self::err())
    }
    fn adapt_apply_insights(&self, _: &Path, _: &str, _: bool) -> Result<Value, MembraneError> {
        Err(Self::err())
    }
    fn adapt_apply_manifest(&self, _: &Path, _: bool) -> Result<Value, MembraneError> {
        Err(Self::err())
    }
    fn adapt_taste_review(
        &self,
        _: &Path,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<Value, MembraneError> {
        Err(Self::err())
    }
    fn transcript_append(
        &self,
        _: &str,
        _: u64,
        _: Option<&str>,
        _: &Value,
    ) -> Result<TranscriptAppend, MembraneError> {
        Err(Self::err())
    }
    fn transcript_tip(&self, _: &str) -> Result<TranscriptTip, MembraneError> {
        Err(Self::err())
    }
}

fn empty_search(query: &str, limit: usize) -> MemorySearch {
    MemorySearch {
        query: Some(query.to_owned()),
        tiers: Vec::new(),
        scope: None,
        limit: Some(limit as u64),
        pool: None,
        more: false,
        items: Vec::new(),
    }
}

#[derive(Debug, Clone)]
pub struct MembraneCli {
    binary: PathBuf,
    timeout: Duration,
    /// Extra environment for the child only; never the parent process env.
    env: Vec<(OsString, OsString)>,
}

impl MembraneCli {
    pub fn new(binary: PathBuf) -> Self {
        Self {
            binary,
            timeout: DEFAULT_TIMEOUT,
            env: Vec::new(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set one environment variable on the spawned child only.
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Discovery order: env override, `notch-state.json`
    /// `membraneBinary.path`, a fresh `resident.json` `binary`, then Pulse's installed
    /// Helpers path; `None` when Pulse is not installed.
    pub fn locate() -> Option<PathBuf> {
        Self::locate_with(
            std::env::var_os(BIN_ENV),
            std::env::var_os("LOCALAPPDATA"),
            std::env::var_os("HOME"),
        )
    }

    pub fn locate_with(
        env_bin: Option<OsString>,
        local_app_data: Option<OsString>,
        home: Option<OsString>,
    ) -> Option<PathBuf> {
        if let Some(bin) = env_bin.filter(|b| !b.is_empty()) {
            let path = PathBuf::from(bin);
            return path.is_file().then_some(path);
        }
        if let Some(path) = notch_state_binary(local_app_data.as_deref(), home.as_deref()) {
            return Some(path);
        }
        if let Some(path) = resident_binary(local_app_data.as_deref(), home.as_deref()) {
            return Some(path);
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        if cfg!(windows) {
            if let Some(base) = local_app_data {
                // pulse.nsi: InstallDir "$LOCALAPPDATA\Programs\Pulse"; Helpers\membrane.exe
                candidates.push(PathBuf::from(base).join("Programs/Pulse/Helpers/membrane.exe"));
            }
        } else if cfg!(target_os = "macos") {
            candidates.push(PathBuf::from(
                "/Applications/Pulse.app/Contents/Helpers/membrane",
            ));
            if let Some(h) = home {
                candidates.push(
                    PathBuf::from(h).join("Applications/Pulse.app/Contents/Helpers/membrane"),
                );
            }
        }
        candidates.into_iter().find(|p| p.is_file())
    }

    /// A real backend when the binary is found, else [`Unavailable`].
    pub fn backend() -> Box<dyn MembraneBackend> {
        match Self::locate() {
            Some(bin) => {
                let cli = Self::new(bin);
                if cli.verify().is_ok() {
                    Box::new(cli)
                } else {
                    Box::new(Unavailable)
                }
            }
            None => Box::new(Unavailable),
        }
    }

    /// README "Finding the binary" step 4: `where --json` must answer `membrane.where.v1`
    /// and its `binary` must be the path that was run.
    pub fn verify(&self) -> Result<WhereInfo, MembraneError> {
        let info = self.where_binary()?;
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        if same(&info.binary, &self.binary) {
            Ok(info)
        } else {
            Err(MembraneError::Parse(format!(
                "`where` reports {} but {} was run",
                info.binary.display(),
                self.binary.display()
            )))
        }
    }

    /// `membrane where --json` (`membrane.where.v1`). Read-only; never contacts the resident.
    pub fn where_binary(&self) -> Result<WhereInfo, MembraneError> {
        let doc = self.run_json(&[s("where"), s("--json")], &["membrane.where"])?;
        Ok(WhereInfo {
            binary: PathBuf::from(str_field(&doc, "binary")?),
            product_version: opt_str(&doc, "product_version"),
            membrane_source_commit: opt_str(&doc, "membrane_source_commit"),
            target: opt_str(&doc, "target"),
            state_root: opt_str(&doc, "stateRoot"),
        })
    }

    /// Run with a hard timeout; the child is killed and reaped on expiry.
    pub fn run(&self, args: &[String]) -> Result<String, MembraneError> {
        self.run_stdin(args, None)
    }

    /// [`Self::run`] with optional standard input.
    pub fn run_stdin(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> Result<String, MembraneError> {
        let (code, stdout, stderr) = self.run_output(args, stdin)?;
        if code == Some(0) {
            Ok(stdout)
        } else {
            Err(map_exit(code, &stderr))
        }
    }

    /// Run and return `(exit code, stdout, stderr)` for any exit; only spawn failure and
    /// timeout are errors.
    fn run_output(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> Result<(Option<i32>, String, String), MembraneError> {
        // The shared RightKit runner: hidden console, stdin written on its own thread (or null), both
        // streams drained to their caps, the whole process tree terminated at the deadline.
        let mut owned = rightkit_child_process::OwnedCommand::new(&self.binary);
        owned
            .command_mut()
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        let options = rightkit_child_process::RunOptions::new(self.timeout)
            .stdout_cap(usize::MAX)
            .stderr_cap(usize::MAX);
        let output = rightkit_child_process::run_command(owned, stdin, &options)
            .map_err(|e| MembraneError::Spawn(e.to_string()))?;
        if output.timed_out {
            return Err(MembraneError::Timeout(self.timeout));
        }
        Ok((
            output.status.code(),
            String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
            String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
        ))
    }

    /// Run, parse one JSON document and check its schema id against `accepted` bases
    /// (`membrane.<command>`, no version) and [`SUPPORTED_MAJOR`].
    fn run_json(&self, args: &[String], accepted: &[&str]) -> Result<Value, MembraneError> {
        let out = self.run(args)?;
        parse_document(&out, accepted)
    }
}

/// Exit code to error (README "Exit codes"); stderr is one `membrane: <reason>` line.
pub fn map_exit(code: Option<i32>, stderr: &str) -> MembraneError {
    let trimmed = stderr.trim();
    let reason = trimmed
        .strip_prefix("membrane:")
        .unwrap_or(trimmed)
        .trim()
        .to_owned();
    match code {
        Some(EXIT_INTERNAL_ERROR) => MembraneError::Internal(reason),
        Some(EXIT_USER_ERROR) => MembraneError::Usage(reason),
        Some(EXIT_NOT_AVAILABLE) => MembraneError::NotAvailable(reason),
        Some(EXIT_RESIDENT_UNREACHABLE) => MembraneError::ResidentUnreachable(reason),
        other => MembraneError::Exit {
            code: other,
            stderr: trimmed.to_owned(),
        },
    }
}

/// Parse stdout as one JSON object and validate `schema` (`membrane.<cmd>.v<N>`).
pub fn parse_document(out: &str, accepted: &[&str]) -> Result<Value, MembraneError> {
    let doc: Value =
        serde_json::from_str(out.trim()).map_err(|e| MembraneError::Parse(e.to_string()))?;
    check_schema(&doc, accepted)?;
    Ok(doc)
}

/// Accept only `accepted` command bases at major [`SUPPORTED_MAJOR`]; unknown majors are
/// [`MembraneError::UnsupportedSchema`].
pub fn check_schema(doc: &Value, accepted: &[&str]) -> Result<(), MembraneError> {
    let schema = doc
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| MembraneError::Parse("document has no `schema`".to_owned()))?;
    let (base, major) = schema
        .rsplit_once(".v")
        .and_then(|(b, n)| n.parse::<u32>().ok().map(|n| (b, n)))
        .ok_or_else(|| MembraneError::Parse(format!("malformed schema id `{schema}`")))?;
    if !accepted.contains(&base) {
        return Err(MembraneError::Parse(format!(
            "unexpected schema `{schema}` (wanted {accepted:?})"
        )));
    }
    if major != SUPPORTED_MAJOR {
        return Err(MembraneError::UnsupportedSchema {
            got: schema.to_owned(),
        });
    }
    Ok(())
}

fn str_field(doc: &Value, key: &str) -> Result<String, MembraneError> {
    doc.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| MembraneError::Parse(format!("missing string field `{key}`")))
}

fn opt_str(doc: &Value, key: &str) -> Option<String> {
    doc.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn parse_items(doc: &Value) -> Result<Vec<MemoryItem>, MembraneError> {
    let items = doc
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| MembraneError::Parse("missing array field `items`".to_owned()))?;
    items
        .iter()
        .map(|it| {
            Ok(MemoryItem {
                id: str_field(it, "id")?,
                tier: str_field(it, "tier")?,
                chars: it.get("chars").and_then(Value::as_u64),
                access: it.get("access").cloned().unwrap_or(Value::Null),
                inject: it.get("inject").cloned().unwrap_or(Value::Null),
                scope: opt_str(it, "scope"),
                score: it.get("score").and_then(Value::as_f64),
                content: opt_str(it, "content"),
            })
        })
        .collect()
}

fn s(v: &str) -> String {
    v.to_owned()
}

fn push_repo(args: &mut Vec<String>, repo: Option<&Path>) {
    if let Some(r) = repo {
        args.push(s("--repo"));
        args.push(r.to_string_lossy().into_owned());
    }
}

impl MembraneBackend for MembraneCli {
    fn context_pull(
        &self,
        cwd: &Path,
        prompt: &str,
        budget: Option<usize>,
        repo: Option<&Path>,
    ) -> Result<ContextPull, MembraneError> {
        let mut args = vec![
            s("context"),
            s("pull"),
            s("--json"),
            s("--cwd"),
            cwd.to_string_lossy().into_owned(),
            s("--prompt"),
            s(prompt),
        ];
        if let Some(b) = budget {
            args.push(s("--budget"));
            args.push(b.to_string());
        }
        push_repo(&mut args, repo);
        let doc = match self.run_json(&args, &["membrane.context.pull"]) {
            Ok(doc) => doc,
            // Exit 3: not enrolled, recall off or no graph. Context is simply off.
            Err(MembraneError::NotAvailable(reason)) => {
                return Ok(ContextPull {
                    text: String::new(),
                    off_reason: Some(reason),
                    reason: None,
                    detail: Value::Null,
                });
            }
            Err(e) => return Err(e),
        };
        let text = doc
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_end()
            .to_owned();
        Ok(ContextPull {
            text,
            off_reason: None,
            reason: opt_str(&doc, "reason"),
            detail: doc.get("detail").cloned().unwrap_or(Value::Null),
        })
    }

    fn memory_push(&self, text: &str) -> Result<Value, MembraneError> {
        // Not a versioned document in the README table; returned as one JSON value.
        let out = self.run(&[s("memory"), s("push"), s(text)])?;
        serde_json::from_str(out.trim()).map_err(|e| MembraneError::Parse(e.to_string()))
    }

    fn memory_list(&self, limit: usize, repo: Option<&Path>) -> Result<MemoryList, MembraneError> {
        let mut args = vec![
            s("memory"),
            s("list"),
            s("--json"),
            s("--limit"),
            limit.to_string(),
        ];
        push_repo(&mut args, repo);
        let doc = match self.run_json(&args, &["membrane.memory.list"]) {
            Ok(doc) => doc,
            // Exit 3: no memory store yet (before the first push).
            Err(MembraneError::NotAvailable(_)) => {
                return Ok(MemoryList {
                    scope: None,
                    limit: limit as u64,
                    items: Vec::new(),
                    completeness: Value::Null,
                });
            }
            Err(e) => return Err(e),
        };
        Ok(MemoryList {
            scope: opt_str(&doc, "scope"),
            limit: doc
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(limit as u64),
            items: parse_items(&doc)?,
            completeness: doc.get("completeness").cloned().unwrap_or(Value::Null),
        })
    }

    fn memory_search(
        &self,
        query: &str,
        tiers: &[String],
        limit: usize,
        repo: Option<&Path>,
    ) -> Result<MemorySearch, MembraneError> {
        let mut args = vec![s("memory"), s("search"), s(query)];
        for t in tiers {
            args.push(s("--tier"));
            args.push(t.clone());
        }
        args.push(s("--limit"));
        args.push(limit.to_string());
        push_repo(&mut args, repo);
        args.push(s("--json"));
        let doc = match self.run_json(&args, &["membrane.memory.search"]) {
            Ok(doc) => doc,
            Err(MembraneError::NotAvailable(_)) => return Ok(empty_search(query, limit)),
            Err(e) => return Err(e),
        };
        Ok(MemorySearch {
            query: opt_str(&doc, "query"),
            tiers: doc
                .get("tiers")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            scope: opt_str(&doc, "scope"),
            limit: doc.get("limit").and_then(Value::as_u64),
            pool: doc.get("pool").and_then(Value::as_u64),
            more: doc.get("more").and_then(Value::as_bool).unwrap_or(false),
            items: parse_items(&doc)?,
        })
    }

    fn memory_delete(&self, id: &str) -> Result<MemoryDelete, MembraneError> {
        // Exit 3 with `deleted: false` when no memory has the id.
        let (code, stdout, stderr) =
            self.run_output(&[s("memory"), s("delete"), s(id), s("--json")], None)?;
        match code {
            Some(0) | Some(EXIT_NOT_AVAILABLE) if !stdout.trim().is_empty() => {
                let doc = parse_document(&stdout, &["membrane.memory.delete"])?;
                Ok(MemoryDelete {
                    id: opt_str(&doc, "id").unwrap_or_else(|| id.to_owned()),
                    deleted: doc
                        .get("deleted")
                        .and_then(Value::as_bool)
                        .unwrap_or(code == Some(0)),
                })
            }
            Some(EXIT_NOT_AVAILABLE) => Ok(MemoryDelete {
                id: id.to_owned(),
                deleted: false,
            }),
            _ => Err(map_exit(code, &stderr)),
        }
    }

    fn blueprint_search(
        &self,
        q: &str,
        limit: usize,
        repo: Option<&Path>,
    ) -> Result<BlueprintAnswer, MembraneError> {
        // `blueprint <query...> [--repo <path>]`: the whole tail is the query and `--repo` is
        // split out of it (modes.rs `split_repo_flag`), so it follows the verb's own words.
        let mut args = vec![
            s("blueprint"),
            s("search"),
            s("--limit"),
            limit.to_string(),
            s(q),
        ];
        push_repo(&mut args, repo);
        let doc = self.run_json(&args, &["membrane.blueprint.search"])?;
        let schema = str_field(&doc, "schema")?;
        let payload_schema = opt_str(&doc, "payload_schema");
        let mut body = doc;
        if let Some(obj) = body.as_object_mut() {
            obj.remove("schema");
            obj.remove("payload_schema");
        }
        Ok(BlueprintAnswer {
            schema,
            payload_schema,
            body,
        })
    }

    fn status(&self) -> Result<StatusInfo, MembraneError> {
        let doc = self.run_json(&[s("status"), s("--json")], &["membrane.status"])?;
        Ok(StatusInfo {
            product_version: str_field(&doc, "product_version")?,
            mode: str_field(&doc, "mode")?,
            state_root: opt_str(&doc, "stateRoot"),
            db_path: doc
                .pointer("/db/path")
                .and_then(Value::as_str)
                .map(str::to_owned),
            db_present: doc.pointer("/db/present").and_then(Value::as_bool),
            model: opt_str(&doc, "model"),
            resident: doc.get("resident").cloned().unwrap_or(Value::Null),
            raw: doc,
        })
    }

    fn adapt_observe(&self, request: &Value) -> Result<Value, MembraneError> {
        let body = serde_json::to_vec(request).map_err(|e| MembraneError::Parse(e.to_string()))?;
        let out = self.run_stdin(
            &[s("adapt"), s("observe"), s("--stdin"), s("--json")],
            Some(&body),
        )?;
        adapt_result(&out, "membrane.adapt.observe")
    }

    fn adapt_recall(
        &self,
        query: &str,
        k: usize,
        scope: &str,
        dimensions: &[String],
    ) -> Result<Value, MembraneError> {
        let mut args = vec![
            s("adapt"),
            s("recall"),
            s(query),
            s("-k"),
            k.to_string(),
            s("--scope"),
            s(scope),
        ];
        for d in dimensions {
            args.push(s("--dimension"));
            args.push(d.clone());
        }
        args.push(s("--json"));
        adapt_result(&self.run(&args)?, "membrane.adapt.recall")
    }

    fn adapt_apply_insights(
        &self,
        insights: &Path,
        installation_id: &str,
        dry_run: bool,
    ) -> Result<Value, MembraneError> {
        let mut args = vec![
            s("adapt"),
            s("apply"),
            s("--insights"),
            insights.to_string_lossy().into_owned(),
            s("--installation-id"),
            s(installation_id),
        ];
        if dry_run {
            args.push(s("--dry-run"));
        }
        args.push(s("--json"));
        adapt_result(&self.run(&args)?, "membrane.adapt.apply")
    }

    fn adapt_apply_manifest(&self, manifest: &Path, dry_run: bool) -> Result<Value, MembraneError> {
        let mut args = vec![
            s("adapt"),
            s("apply"),
            s("--manifest"),
            manifest.to_string_lossy().into_owned(),
        ];
        if dry_run {
            args.push(s("--dry-run"));
        }
        args.push(s("--json"));
        adapt_result(&self.run(&args)?, "membrane.adapt.apply")
    }

    fn adapt_taste_review(
        &self,
        input: &Path,
        installation_id: &str,
        created_at: &str,
        canonical_pool_sha256: Option<&str>,
    ) -> Result<Value, MembraneError> {
        let mut args = vec![
            s("adapt"),
            s("taste-review"),
            s("--input"),
            input.to_string_lossy().into_owned(),
            s("--installation-id"),
            s(installation_id),
            s("--created-at"),
            s(created_at),
        ];
        if let Some(d) = canonical_pool_sha256 {
            args.push(s("--canonical-pool-sha256"));
            args.push(s(d));
        }
        args.push(s("--json"));
        adapt_result(&self.run(&args)?, "membrane.adapt.taste_review")
    }

    fn transcript_append(
        &self,
        session: &str,
        cursor: u64,
        scope: Option<&str>,
        events: &Value,
    ) -> Result<TranscriptAppend, MembraneError> {
        let mut args = vec![
            s("transcript"),
            s("append"),
            s("--session"),
            s(session),
            s("--cursor"),
            cursor.to_string(),
        ];
        if let Some(sc) = scope {
            args.push(s("--scope"));
            args.push(s(sc));
        }
        args.push(s("--stdin"));
        args.push(s("--json"));
        let body = serde_json::to_vec(events).map_err(|e| MembraneError::Parse(e.to_string()))?;
        let (code, stdout, stderr) = self.run_output(&args, Some(&body))?;
        // Exit 2 prints the conflict receipt on stdout; other exit-2 refusals print none.
        if code == Some(0) || (code == Some(EXIT_USER_ERROR) && !stdout.trim().is_empty()) {
            let doc = parse_document(&stdout, &["membrane.transcript.append"])?;
            if doc.get("conflict").and_then(Value::as_bool) == Some(true) {
                return Ok(TranscriptAppend::Conflict(TranscriptConflict {
                    current_cursor: doc.get("current_cursor").and_then(Value::as_u64),
                    status: doc.get("status").and_then(Value::as_u64),
                    code: opt_str(&doc, "code").unwrap_or_default(),
                    detail: doc.get("detail").cloned().unwrap_or(Value::Null),
                }));
            }
            if code == Some(0) {
                return Ok(TranscriptAppend::Committed {
                    session: opt_str(&doc, "session").unwrap_or_else(|| session.to_owned()),
                    cursor: doc.get("cursor").and_then(Value::as_u64).unwrap_or(cursor),
                    replayed: doc
                        .get("replayed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    receipt: doc.get("receipt").cloned().unwrap_or(Value::Null),
                });
            }
        }
        Err(map_exit(code, &stderr))
    }

    fn transcript_tip(&self, session: &str) -> Result<TranscriptTip, MembraneError> {
        let doc = self.run_json(
            &[
                s("transcript"),
                s("tip"),
                s("--session"),
                s(session),
                s("--json"),
            ],
            &["membrane.transcript.tip"],
        )?;
        Ok(TranscriptTip {
            session: opt_str(&doc, "session").unwrap_or_else(|| session.to_owned()),
            current_cursor: doc
                .get("current_cursor")
                .and_then(Value::as_u64)
                .ok_or_else(|| MembraneError::Parse("missing `current_cursor`".to_owned()))?,
            prefix_digest: opt_str(&doc, "prefix_digest"),
        })
    }
}

/// The `result` of an `adapt` document.
fn adapt_result(out: &str, base: &str) -> Result<Value, MembraneError> {
    let doc = parse_document(out, &[base])?;
    doc.get("result")
        .cloned()
        .ok_or_else(|| MembraneError::Parse("missing field `result`".to_owned()))
}

/// Pulse's notch state folder: Windows `%LOCALAPPDATA%\Pulse`, Mac
/// `~/Library/Application Support/Pulse` (`pulse/core/src/usage_snapshot.rs`,
/// `app_manager.rs`).
fn notch_state_path(
    local_app_data: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if cfg!(windows) {
        local_app_data.map(|b| PathBuf::from(b).join("Pulse/notch-state.json"))
    } else {
        home.map(|h| PathBuf::from(h).join("Library/Application Support/Pulse/notch-state.json"))
    }
}

/// `membraneBinary.path` of `notch-state.json`, when it names an existing file
/// (`{"path": null, "reason": ..}` means not installed).
fn notch_state_binary(
    local_app_data: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let text = std::fs::read_to_string(notch_state_path(local_app_data, home)?).ok()?;
    let doc: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let path = PathBuf::from(doc.pointer("/membraneBinary/path")?.as_str()?);
    path.is_file().then_some(path)
}

/// `binary` of a fresh `resident.json` (README "Finding the binary" step 1). Staleness is
/// judged by `ts` (Unix ms) only; a dead pid with a fresh `ts` cannot occur within 30 s of
/// the resident's 10 s refresh except right after a crash, and the `is_file` check still holds.
fn resident_binary(
    local_app_data: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let state = match std::env::var_os("MEMBRANE_STATE_ROOT") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ if cfg!(windows) => PathBuf::from(local_app_data?).join("Orthic Labs/Membrane/state"),
        _ => PathBuf::from(home?).join("Library/Application Support/Orthic Labs/Membrane/state"),
    };
    let text = std::fs::read_to_string(state.join("resident.json")).ok()?;
    let doc: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let ts = u128::from(doc.get("ts")?.as_u64()?);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    if now.saturating_sub(ts) > RESIDENT_STALE_MS {
        return None;
    }
    let path = PathBuf::from(doc.get("binary")?.as_str()?);
    path.is_file().then_some(path)
}
