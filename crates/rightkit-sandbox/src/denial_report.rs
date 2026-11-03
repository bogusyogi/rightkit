//! Typed, human-readable sandbox denial reports and the grants that could lift them (ported from
//! CodeRight `sandbox/denial_report.rs`).
//!
//! [`classify_denial`](crate::classify_denial) says *what kind* of denial raw output shows. A
//! caller that wants to tell the operator what was blocked, and ask for the narrowest extra grant
//! that would let a re-run succeed, needs more: the denied path, whether it is a protected
//! secret, and the grant to request. [`report_denial`] builds that from the command's output, the
//! command text and how the command was confined.
//!
//! The output is **untrusted**: a confined child can print any text, including a path of its
//! choosing. A report is therefore only ever a *request* for a grant that a person or an
//! admission authority decides on, and the request is built to stay narrow: a protected secret
//! never gets one, nor does a filesystem root or a path with `..`, whatever the output says.
//! Pure text and path logic: no I/O except `is_dir` on the denied path to pick its folder.

use crate::deny_read::{
    path_is_protected_secret, path_is_within, secret_paths_within, secret_roots,
};
use crate::{classify_denial, DenialCategory, DenialSource};
use std::path::{Path, PathBuf};

/// What one command run is additionally allowed to do beyond its policy. Per command, never
/// persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grant {
    /// Outbound network.
    pub network: bool,
    /// Extra read-only roots.
    pub read_paths: Vec<PathBuf>,
    /// Extra read/write roots.
    pub write_paths: Vec<PathBuf>,
}

impl Grant {
    /// A grant of outbound network and nothing else.
    pub fn network() -> Self {
        Self {
            network: true,
            ..Self::default()
        }
    }

    /// Whether the grant allows nothing.
    pub fn is_empty(&self) -> bool {
        !self.network && self.read_paths.is_empty() && self.write_paths.is_empty()
    }

    /// Union of two grants, without duplicate paths.
    pub fn merge(&mut self, other: &Grant) {
        self.network |= other.network;
        for path in &other.read_paths {
            if !self.read_paths.contains(path) {
                self.read_paths.push(path.clone());
            }
        }
        for path in &other.write_paths {
            if !self.write_paths.contains(path) {
                self.write_paths.push(path.clone());
            }
        }
    }

    /// One-line, human-readable form for an approval prompt.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.network {
            parts.push("network access".to_string());
        }
        for path in &self.read_paths {
            parts.push(format!("read access to {}", path.display()));
        }
        for path in &self.write_paths {
            parts.push(format!("read/write access to {}", path.display()));
        }
        parts.join(", ")
    }
}

/// How the command was confined when it ran.
#[derive(Debug, Clone)]
pub struct ConfinementInfo {
    /// Backend that produced the denial text.
    pub source: DenialSource,
    /// Whether this run was allowed outbound network.
    pub network_allowed: bool,
    /// The workspace root the policy grants. A failure on a path inside it is not a grant
    /// problem and is never reported.
    pub workspace_root: PathBuf,
}

/// A classified, reportable sandbox denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenialReport {
    /// [`DenialCategory::Network`] or [`DenialCategory::Filesystem`].
    pub category: DenialCategory,
    /// The path the sandbox refused, for a filesystem denial.
    pub denied_path: Option<PathBuf>,
    /// The path is on the protected-secret list: it can never be granted.
    pub protected_secret: bool,
    /// The narrowest grant that lifts the denial; `None` is a hard stop.
    pub requested_grant: Option<Grant>,
    /// Human-readable explanation naming the sandbox as the cause.
    pub message: String,
}

impl DenialReport {
    /// Stable machine identifier, e.g. `sandbox-denial:network`.
    pub fn rule_id(&self) -> &'static str {
        match (self.category, self.protected_secret) {
            (DenialCategory::Network, _) => "sandbox-denial:network",
            (_, true) => "sandbox-denial:protected-secret",
            _ => "sandbox-denial:filesystem",
        }
    }

    /// The text block to append to a tool result.
    pub fn render(&self) -> String {
        format!("[sandbox denial: {}] {}", self.rule_id(), self.message)
    }
}

/// Text signatures of an OS-level network refusal (PowerShell, curl, Winsock, resolver).
const NETWORK_BLOCK_SIGNATURES: &[&str] = &[
    "forbidden by its access permissions",
    "wsaeacces",
    "unable to connect to the remote server",
    "the remote name could not be resolved",
    "no such host is known",
    "could not resolve host",
    "name or service not known",
    "temporary failure in name resolution",
    "network is unreachable",
    "a socket operation was attempted to an unreachable",
    "failed to connect to",
    "couldn't connect to server",
    "an attempt was made to access a socket",
];

/// Whether lowercase `lowered` output carries the signature of a policy-blocked connection (as
/// opposed to a genuinely remote failure).
pub fn looks_like_blocked_network(lowered: &str) -> bool {
    if NETWORK_BLOCK_SIGNATURES
        .iter()
        .any(|signature| lowered.contains(signature))
    {
        return true;
    }
    lowered.contains("operation not permitted")
        && (lowered.contains("connect")
            || lowered.contains("ssh")
            || lowered.contains("socket")
            || lowered.contains("network"))
}

/// Whether the command text looks like it writes. Used only to pick the narrowest grant to
/// request, never to allow anything.
fn command_looks_like_write(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    const TOKENS: &[&str] = &[
        "set-content",
        "add-content",
        "out-file",
        "new-item",
        "copy-item",
        "move-item",
        "remove-item",
        "rename-item",
        "mkdir",
        "md ",
        "touch ",
        "tee ",
        "del ",
        "rm ",
        "cp ",
        "mv ",
        "copy ",
        "move ",
        "[io.file]::write",
        "writeall",
    ];
    lowered.contains('>') || TOKENS.iter().any(|token| lowered.contains(token))
}

fn looks_like_absolute_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    (bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || text.starts_with('/')
        || text.starts_with("\\\\")
}

/// First quoted absolute path in `text` (`'C:\x'`, `"C:\x"` or a backtick pair).
fn first_quoted_path(text: &str) -> Option<PathBuf> {
    for quote in ['\'', '"', '`'] {
        let mut rest = text;
        while let Some(start) = rest.find(quote) {
            let after = &rest[start + 1..];
            let Some(end) = after.find(quote) else { break };
            let candidate = &after[..end];
            if looks_like_absolute_path(candidate) {
                return Some(PathBuf::from(candidate));
            }
            rest = &after[end + 1..];
        }
    }
    None
}

/// The path of a Unix-style `tool: /path: Permission denied` line.
fn path_before_errno(output: &str) -> Option<PathBuf> {
    for line in output.lines() {
        let lowered = line.to_ascii_lowercase();
        for marker in [": permission denied", ": operation not permitted"] {
            let Some(end) = lowered.find(marker) else {
                continue;
            };
            // Cut on the original line at the same byte offset: ASCII lowering keeps lengths.
            let head = &line[..end];
            let candidate = head.rsplit(": ").next().unwrap_or(head).trim();
            let candidate = candidate.trim_matches(|c| matches!(c, '\'' | '"' | '`'));
            if looks_like_absolute_path(candidate) {
                return Some(PathBuf::from(candidate));
            }
        }
    }
    None
}

/// Best-effort path the sandbox refused: the path the OS named, else the first absolute path
/// spelled in the command itself.
fn extract_denied_path(output: &str, command: &str) -> Option<PathBuf> {
    extract_denied_path_raw(output, command).map(|path| reconcile_wrapped_path(path, command))
}

/// Console error text is hard-wrapped (PowerShell wraps near 120 columns), which can split a
/// long quoted path across lines. Prefer the spelling the command itself used when it matches
/// modulo whitespace, case and slash direction; otherwise drop the line breaks.
fn reconcile_wrapped_path(path: PathBuf, command: &str) -> PathBuf {
    let text = path.to_string_lossy().into_owned();
    let squash = |value: &str| -> String {
        value
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| {
                if c == '/' {
                    '\\'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    };
    let wanted = squash(&text);
    // PowerShell also ellipsizes paths. Recover only a uniquely matching literal.
    if let Some((prefix, _)) = wanted.split_once("...") {
        let candidates: Vec<_> = command
            .split(['\'', '"', '`'])
            .filter(|piece| looks_like_absolute_path(piece) && squash(piece).starts_with(prefix))
            .collect();
        if candidates.len() == 1 {
            return PathBuf::from(candidates[0]);
        }
    }
    for piece in command.split(['\'', '"', '`']) {
        if looks_like_absolute_path(piece) && squash(piece) == wanted {
            return PathBuf::from(piece);
        }
    }
    if !text.contains(['\r', '\n']) {
        return path;
    }
    PathBuf::from(text.replace(['\r', '\n'], ""))
}

fn extract_denied_path_raw(output: &str, command: &str) -> Option<PathBuf> {
    let lowered = output.to_ascii_lowercase();
    if let Some(index) = lowered.find("access to the path") {
        if let Some(path) = first_quoted_path(&output[index..]) {
            return Some(path);
        }
    }
    if let Some(path) = first_quoted_path(output) {
        return Some(path);
    }
    if let Some(path) = path_before_errno(output) {
        return Some(path);
    }
    command
        .split_whitespace()
        .map(|token| token.trim_matches(|c| matches!(c, '\'' | '"' | '`' | ';' | ',' | ')' | '(')))
        .find(|token| looks_like_absolute_path(token))
        .map(PathBuf::from)
}

/// Whether the text spells a filesystem root: `/`, a bare drive (`C:` or `C:\`) or a UNC share
/// root. Textual on purpose: the text comes from child output and may be a spelling of another
/// platform than the one running.
fn is_root_text(text: &str) -> bool {
    let trimmed = text.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return true;
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    match trimmed.strip_prefix("\\\\") {
        Some(unc) => unc.split('\\').filter(|part| !part.is_empty()).count() <= 2,
        None => false,
    }
}

/// Whether a grant on `root` may be requested at all: absolute, never a filesystem root, never
/// a path that climbs with `..`.
fn grantable_root(root: &Path) -> bool {
    let text = root.to_string_lossy();
    looks_like_absolute_path(&text)
        && !is_root_text(&text)
        && !text.split(['/', '\\']).any(|segment| segment == "..")
}

/// Whether granting `root` would expose a protected location: a protected root lies inside it
/// (spelled lexically, so a not-yet-created `.ssh` still counts) or an existing secret does
/// (`existing_within`, which also covers symlinked spellings).
fn root_contains_protected(root: &Path, secrets: &[PathBuf], existing_within: &[PathBuf]) -> bool {
    !existing_within.is_empty() || secrets.iter().any(|secret| path_is_within(secret, root))
}

/// The folder containing `path`, split on either separator so a path spelled for another
/// platform still yields its folder. A top-level file yields the root.
fn parent_folder(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(0) => PathBuf::from(&text[..1]),
        Some(index) => {
            let head = &trimmed[..index];
            if head.len() == 2 && head.ends_with(':') {
                PathBuf::from(format!("{head}\\"))
            } else {
                PathBuf::from(head)
            }
        }
        None => path.to_path_buf(),
    }
}

/// Classify a confined command's output into a reportable denial.
///
/// Returns `None` when the failure is not recognizably the sandbox's doing: an unrecognized
/// failure is never labelled a sandbox denial (it may be a real bug), and a failure on a path
/// inside the workspace is not a grant problem. A denial report is only a request; see the
/// module notes on untrusted output.
pub fn report_denial(
    output: &str,
    command: &str,
    exit_ok: bool,
    info: &ConfinementInfo,
) -> Option<DenialReport> {
    let lowered = output.to_ascii_lowercase();
    // AppContainer can silently drop TCP traffic: PowerShell then reports a WebException
    // timeout instead of WSAEACCES. Attribute that only to a failed Windows HTTP request that
    // ran with network disabled; ordinary timeouts and other platforms are not evidence.
    let confined_http_timeout = !exit_ok
        && info.source == DenialSource::WindowsAppContainer
        && !info.network_allowed
        && lowered.contains("the operation has timed out")
        && lowered.contains("webcmdletwebresponseexception")
        && (lowered.contains("invoke-webrequest") || lowered.contains("invoke-restmethod"));
    let network_text = looks_like_blocked_network(&lowered) || confined_http_timeout;

    if network_text && !info.network_allowed {
        return Some(DenialReport {
            category: DenialCategory::Network,
            denied_path: None,
            protected_secret: false,
            requested_grant: Some(Grant::network()),
            message: "This command's network access was blocked by the sandbox, not by the \
                      network or the remote host. Network is off by default for sandboxed \
                      commands. Allow network for this command to re-run it with network \
                      access."
                .into(),
        });
    }
    if exit_ok {
        return None;
    }

    let signature = classify_denial(output, info.source);
    if signature.category != DenialCategory::Filesystem {
        return None;
    }
    let path = extract_denied_path(output, command)?;

    if path_is_protected_secret(&path) {
        return Some(DenialReport {
            category: DenialCategory::Filesystem,
            message: format!(
                "Access to {} was blocked by the sandbox: it is a protected secret location \
                 (credentials or keys) and cannot be granted to commands. Do not retry; ask \
                 the user to handle it outside the agent.",
                path.display()
            ),
            denied_path: Some(path),
            protected_secret: true,
            requested_grant: None,
        });
    }
    if path_is_within(&path, &info.workspace_root) {
        return None;
    }
    let write = command_looks_like_write(command);
    // The exact denied object when it exists; only a path that does not exist yet falls back to
    // its containing folder. Never widen a file to its parent: the parent can hold protected
    // locations, and a `Grant` carries no enforced carve-outs.
    let root = if path.exists() {
        path.clone()
    } else {
        parent_folder(&path)
    };
    if root_contains_protected(&root, &secret_roots(), &secret_paths_within(&root)) {
        return Some(DenialReport {
            category: DenialCategory::Filesystem,
            message: format!(
                "Access to {} was blocked by the sandbox, and the only root that would lift                  it contains protected secret locations, which a grant cannot exclude. Keep                  the work inside the workspace or name a narrower path.",
                path.display()
            ),
            denied_path: Some(path),
            protected_secret: false,
            requested_grant: None,
        });
    }
    if !grantable_root(&root) {
        return Some(DenialReport {
            category: DenialCategory::Filesystem,
            message: format!(
                "Access to {} was blocked by the sandbox: the path is outside the granted \
                 locations, and it is too broad to grant. Keep the work inside the workspace.",
                path.display()
            ),
            denied_path: Some(path),
            protected_secret: false,
            requested_grant: None,
        });
    }
    let mut grant = Grant::default();
    if write {
        grant.write_paths.push(root.clone());
    } else {
        grant.read_paths.push(root.clone());
    }
    Some(DenialReport {
        category: DenialCategory::Filesystem,
        message: format!(
            "Access to {} was blocked by the sandbox: the path is outside the workspace and \
             has not been granted. Allow {} access to {} to re-run this command with that \
             grant, or keep the work inside the workspace.",
            path.display(),
            if write { "read/write" } else { "read" },
            root.display()
        ),
        denied_path: Some(path),
        protected_secret: false,
        requested_grant: Some(grant),
    })
}
