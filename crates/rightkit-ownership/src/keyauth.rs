//! Shared key authority: reads the approval key log from the authority checkout's Git
//! HEAD and merges it with the host's protected checkpoint state. Port of
//! `ownership-key-authority.mjs` (read side).
//!
//! The protected checkpoint files live in the platform trust directory. Their location
//! audit is supplied through [`AuthorityEnv`], so this module does not depend on how the
//! host audits protected paths.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::approval::{canonical_repository_identity, resolve_key_log_trust, ResolvedKeyLog};
use crate::committed::{git_args, hash_git_object};
use crate::gitio::{self, native_git, protected_env};
use crate::json::{field, is_hex64, safe_ordinal, str_of};
use crate::paths;

const LOG_PATH: &str = ".rightkit/ownership-keys.json";
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Audits one protected path and returns the problems found.
pub type AuditFn = Box<dyn Fn(&Path) -> Vec<String> + Send + Sync>;

/// Where the protected checkpoint lives and how its location is audited.
pub struct AuthorityEnv {
    /// Protected trust file; checkpoints sit beside it. `None` fails closed.
    pub trust_file: Option<PathBuf>,
    /// Returns the problems found auditing one protected path.
    pub audit: AuditFn,
}

impl AuthorityEnv {
    /// An environment with no protected checkpoint location. Every authority read fails
    /// closed once it needs the checkpoint.
    pub fn unavailable() -> Self {
        Self {
            trust_file: None,
            audit: Box::new(|_| vec!["protected checkpoint location is unavailable".to_string()]),
        }
    }
}

pub struct Authority {
    pub digest: String,
    pub root_fingerprint: Value,
    pub key_checkpoint: Value,
    pub resolved_trust: Value,
    /// `{repositoryIdentity, commit, tree}`.
    pub binding: Value,
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let safe = if cfg!(windows) {
        PathBuf::from(root.to_string_lossy().replace('\\', "/"))
    } else {
        root.to_path_buf()
    };
    let full = git_args(root, &safe, args);
    let env = protected_env();
    match gitio::run(
        native_git(),
        &full,
        Some(&env),
        None,
        GIT_TIMEOUT,
        (MAX_LOG_BYTES as usize) + 4096,
    ) {
        Ok(out) if out.success() => Ok(out.stdout),
        _ => Err(format!("authority Git {} failed", args[0])),
    }
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    git(root, args).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

struct Expected {
    repository_identity: String,
    checkout_path: PathBuf,
}

fn validate_authority(authority: &Value) -> Result<Expected, String> {
    let exact = authority.as_object().is_some_and(|m| {
        m.len() == 2 && m.contains_key("checkoutPath") && m.contains_key("repositoryIdentity")
    });
    if !exact {
        return Err("protected key authority fields are malformed".into());
    }
    let checkout = field(authority, "checkoutPath")
        .as_str()
        .filter(|p| Path::new(p).is_absolute() && !p.contains('\0'));
    let Some(checkout) = checkout else {
        return Err("protected key authority checkoutPath must be absolute".into());
    };
    let identity = canonical_repository_identity(field(authority, "repositoryIdentity"))?;
    if Some(identity.as_str()) != field(authority, "repositoryIdentity").as_str() {
        return Err("protected key authority identity must be canonical".into());
    }
    let real = paths::real(Path::new(checkout)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            format!("ENOENT: no such file or directory, realpath '{checkout}'")
        }
        _ => e.to_string(),
    })?;
    if !real.is_dir() {
        return Err("protected key authority checkoutPath must be a directory".into());
    }
    Ok(Expected {
        repository_identity: identity,
        checkout_path: real,
    })
}

fn valid_checkpoint(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|m| m.len() == 2 && m.contains_key("head") && m.contains_key("sequence"))
        && safe_ordinal(field(value, "sequence"))
        && is_hex64(str_of(field(value, "head")))
}

fn bounded_json(file: &Path, env: &AuthorityEnv) -> Result<Value, std::io::Error> {
    let meta = std::fs::symlink_metadata(file)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_LOG_BYTES {
        return Err(std::io::Error::other(
            "protected checkpoint input must be a bounded regular file",
        ));
    }
    let problems = (env.audit)(file);
    if !problems.is_empty() {
        return Err(std::io::Error::other(format!(
            "protected checkpoint audit failed: {}",
            problems.join("; ")
        )));
    }
    let text = std::fs::read_to_string(file)?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}

struct CheckpointState {
    resolved: ResolvedKeyLog,
    state: Option<Value>,
}

fn resolve_protected_checkpoint(
    trust: &Value,
    bytes: &[u8],
    file: &Path,
    env: &AuthorityEnv,
) -> Result<CheckpointState, String> {
    let disk = bounded_json(file, env).map_err(|e| e.to_string())?;
    if field(&disk, "rootFingerprint") != field(trust, "rootFingerprint")
        || !valid_checkpoint(field(&disk, "keyCheckpoint"))
    {
        return Err(
            "protected checkpoint trust pin is malformed or differs from reader pin".into(),
        );
    }
    let mut floors = vec![
        field(trust, "keyCheckpoint").clone(),
        field(&disk, "keyCheckpoint").clone(),
    ];
    let state_path = file
        .parent()
        .unwrap_or(Path::new(""))
        .join("checkpoints")
        .join(format!("{}.json", str_of(field(trust, "rootFingerprint"))));
    let state = match bounded_json(&state_path, env) {
        Ok(state) => Some(state),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if let Some(state) = &state {
        let exact = state.as_object().is_some_and(|m| {
            m.len() == 3
                && m.contains_key("keyCheckpoint")
                && m.contains_key("rootFingerprint")
                && m.contains_key("schema")
        });
        if !exact
            || field(state, "schema").as_f64() != Some(1.0)
            || field(state, "rootFingerprint") != field(trust, "rootFingerprint")
            || !valid_checkpoint(field(state, "keyCheckpoint"))
        {
            return Err("protected checkpoint state is malformed".into());
        }
        floors.push(field(state, "keyCheckpoint").clone());
    }
    let mut resolved = None;
    for floor in floors {
        let mut pinned = trust.clone();
        pinned["keyCheckpoint"] = floor;
        resolved = Some(
            resolve_key_log_trust(&pinned, Some(bytes))
                .map_err(|p| format!("authority key log verification failed: {}", p.join("; ")))?,
        );
    }
    Ok(CheckpointState {
        resolved: resolved.expect("at least one floor"),
        state,
    })
}

/// Reads one exact key-log blob from the authority checkout's Git HEAD and resolves it.
/// `Err` carries the problems, as Node's `{ok: false, problems}`.
pub fn read_committed_key_authority(
    trust: &Value,
    env: &AuthorityEnv,
) -> Result<Authority, Vec<String>> {
    read(trust, env).map_err(|message| vec![message])
}

fn read(trust: &Value, env: &AuthorityEnv) -> Result<Authority, String> {
    let expected = validate_authority(field(trust, "keyAuthority"))?;
    let checkout = &expected.checkout_path;
    let top = git_text(checkout, &["rev-parse", "--show-toplevel"])?;
    let normalized_top = paths::resolve(Path::new(&top));
    let same = if cfg!(windows) {
        normalized_top.to_string_lossy().to_lowercase() == checkout.to_string_lossy().to_lowercase()
    } else {
        normalized_top == *checkout
    };
    if !same {
        return Err(
            "protected key authority checkoutPath must be exact Git repository root".into(),
        );
    }
    let origin = git_text(checkout, &["config", "--get", "remote.origin.url"])?;
    if canonical_repository_identity(&json!(origin))
        .ok()
        .as_deref()
        != Some(expected.repository_identity.as_str())
    {
        return Err("authority repository origin does not match protected identity".into());
    }
    let commit = git_text(checkout, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let tree = git_text(
        checkout,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )?;
    let listing = git(checkout, &["ls-tree", "-z", &commit, "--", LOG_PATH])?;
    let text = String::from_utf8_lossy(&listing).into_owned();
    let rows: Vec<&str> = text.split('\0').filter(|r| !r.is_empty()).collect();
    if rows.len() != 1 {
        return Err("authority committed key log is missing or ambiguous".into());
    }
    let oid = parse_log_row(rows[0]).ok_or(
        "authority key log must be a regular committed blob, not a symlink or unsupported mode",
    )?;
    let size: u64 = git_text(checkout, &["cat-file", "-s", &oid])?
        .parse()
        .map_err(|_| "authority key log exceeds 2 MiB or has invalid size".to_string())?;
    if size > MAX_LOG_BYTES {
        return Err("authority key log exceeds 2 MiB or has invalid size".into());
    }
    let bytes = git(checkout, &["cat-file", "blob", &oid])?;
    if bytes.len() as u64 != size {
        return Err("authority key log blob size changed during read".into());
    }
    if hash_git_object("blob", &bytes, oid.len() == 64) != oid {
        return Err("authority key log blob hash does not match committed tree".into());
    }
    let first = resolve_key_log_trust(trust, Some(&bytes))
        .map_err(|p| format!("authority key log verification failed: {}", p.join("; ")))?;
    let digest = first.key_log_digest.clone();
    let root_fingerprint = field(&first.trust, "rootFingerprint").clone();
    let file = env
        .trust_file
        .as_deref()
        .ok_or("protected checkpoint audit failed: protected checkpoint location is unavailable")?;
    let protected = resolve_protected_checkpoint(trust, &bytes, file, env)?;
    let advanced = protected.state.as_ref().is_some_and(|state| {
        field(field(state, "keyCheckpoint"), "sequence")
            .as_f64()
            .unwrap_or(-1.0)
            >= field(&protected.resolved.checkpoint, "sequence")
                .as_f64()
                .unwrap_or(0.0)
    });
    if !advanced {
        return Err(
            "checkpoint advance required: run the elevated ownership trust update on this host"
                .into(),
        );
    }
    Ok(Authority {
        digest,
        root_fingerprint,
        key_checkpoint: protected.resolved.checkpoint.clone(),
        resolved_trust: protected.resolved.trust,
        binding: json!({"repositoryIdentity": expected.repository_identity, "commit": commit, "tree": tree}),
    })
}

/// `^(100644|100755) blob ([0-9a-f]{40}|[0-9a-f]{64})\t\.rightkit\/ownership-keys\.json$`.
fn parse_log_row(row: &str) -> Option<String> {
    let (head, path) = row.split_once('\t')?;
    if path != LOG_PATH {
        return None;
    }
    let mut parts = head.split(' ');
    let (mode, kind, oid) = (parts.next()?, parts.next()?, parts.next()?);
    let hex = oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (matches!(mode, "100644" | "100755")
        && kind == "blob"
        && parts.next().is_none()
        && hex
        && (oid.len() == 40 || oid.len() == 64))
        .then(|| oid.to_string())
}
