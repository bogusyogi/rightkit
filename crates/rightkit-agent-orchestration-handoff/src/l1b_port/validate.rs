//! L1 literal port of `src/lib/handoff/validate_handoff.py`.
//!
//! Fail-closed validator for zero-memory cold-start handoffs: heading order,
//! label presence/concreteness, resume-step structure, decision/artifact
//! table shape, readiness/proceed/work-type enums, gap-severity coherence,
//! author-gate checklist, banned phrases, and secret-pattern scanning.
//!
//! The Python CLI (`argparse` + receipt read/write to disk) is not ported:
//! this module exposes the pure validation core (`validate`, `storage_errors`,
//! path normalization) as a library so a caller wires its own I/O.

use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const FORBIDDEN_STORAGE_PARTS: &[&str] = &[
    "temp",
    "tmp",
    ".tmp",
    "temporary",
    "cache",
    ".cache",
    "review-run",
    "review-runs",
    ".review-runs",
    ".council-runs",
    "scratch",
];

pub fn clean_path_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

/// Lexical, metadata-only path normalization: unifies separators and trims a
/// trailing `/`; for non-drive-letter paths it also collapses `.`/`..`
/// segments, while drive-letter paths (`C:/...`) are NOT dot-collapsed.
/// Optional Windows case-folding. It never touches the filesystem and is NOT
/// a filesystem identity guarantee: symlinks, junctions, and `..` through a
/// link are not followed. Do not use it alone for an authority decision;
/// storage authorization goes through [`resolve_physical_existing`] /
/// [`resolve_physical_receipt`] (see [`storage_errors`]). It remains valid for
/// comparing declared packet text and for command-string binding.
pub fn normalized_path(value: &str, windows: bool) -> String {
    let raw = clean_path_value(value).replace('\\', "/");
    let drive_re = drive_re();
    let normalized = if drive_re.is_match(&raw) {
        raw.trim_end_matches('/').to_string()
    } else {
        lexical_resolve(&raw)
    };
    if windows {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

fn lexical_resolve(raw: &str) -> String {
    let is_abs = raw.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    let prefixed = if is_abs { format!("/{joined}") } else { joined };
    prefixed.trim_end_matches('/').to_string()
}

pub fn is_absolute_path(value: &str) -> bool {
    let raw = clean_path_value(value).replace('\\', "/");
    drive_re().is_match(&raw) || raw.starts_with('/')
}

fn drive_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:/").unwrap());
    &RE
}

/// Strips a Windows verbatim prefix (`\\?\C:\x` -> `C:\x`,
/// `\\?\UNC\srv\sh` -> `\\srv\sh`) that `std::fs::canonicalize` produces on
/// Windows, so physical paths compare against declared `C:\...` spellings.
/// Pure string operation; POSIX physical paths never start with `\\?\` and are
/// returned unchanged.
pub fn strip_verbatim_prefix(value: &str) -> String {
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        value.to_string()
    }
}

/// Physical path as a display string without any Windows verbatim prefix.
pub fn physical_path_string(path: &Path) -> String {
    strip_verbatim_prefix(&path.to_string_lossy())
}

/// Physical identity of an artifact that must already exist: the real
/// filesystem path with every symlink/junction/`..` followed. Resolution
/// failure is an error; there is deliberately NO lexical fallback.
///
/// This is a point-in-time resolution, not confinement: the filesystem can
/// change between this call and later reads/writes (TOCTOU).
pub fn resolve_physical_existing(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path)
        .map_err(|e| format!("cannot resolve existing path {}: {e}", path.display()))
}

/// Physical identity of a receipt sidecar. The path must have a lexical
/// basename (`Path::file_name`), checked BEFORE any filesystem call, because
/// some platforms collapse a trailing `..` during lookup and would otherwise
/// resolve it to a directory. If the receipt exists (including as a link, even
/// a dangling one) it must canonicalize AND the canonical target must be a
/// regular file (a directory, or a directory link/junction, is rejected). If it
/// is absent (`NotFound`), its parent directory must canonicalize and the
/// result is the canonical parent joined with the basename. Any other failure
/// (missing parent, dangling link, non-directory parent, permission error, no
/// basename such as `..`) is an error; there is no lexical fallback.
///
/// Same point-in-time caveat as [`resolve_physical_existing`].
pub fn resolve_physical_receipt(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("receipt path {} has no resolvable basename", path.display()))?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let canonical = resolve_physical_existing(path)?;
            let meta = std::fs::metadata(&canonical).map_err(|e| {
                format!(
                    "cannot inspect resolved receipt target {}: {e}",
                    canonical.display()
                )
            })?;
            if !meta.is_file() {
                return Err(format!(
                    "receipt path {} resolves to a non-file target {}",
                    path.display(),
                    canonical.display()
                ));
            }
            Ok(canonical)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| {
                    format!("receipt path {} has no resolvable parent", path.display())
                })?;
            let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
                format!(
                    "cannot resolve receipt parent directory {}: {e}",
                    parent.display()
                )
            })?;
            Ok(canonical_parent.join(name))
        }
        Err(e) => Err(format!(
            "cannot inspect receipt path {}: {e}",
            path.display()
        )),
    }
}

pub(crate) fn physical_norm(path: &Path, windows: bool) -> String {
    normalized_path(&physical_path_string(path), windows)
}

pub(crate) fn has_forbidden_part(normalized_paths: &[&str]) -> bool {
    normalized_paths.iter().any(|p| {
        p.split('/')
            .any(|part| FORBIDDEN_STORAGE_PARTS.contains(&part) || part.starts_with(".validator-"))
    })
}

/// Storage authorization for a handoff packet and its sidecar receipt.
///
/// Authority decisions (forbidden temp/cache storage, `.md` suffix, receipt
/// adjacency, receipt basename) are made on PHYSICAL paths: the existing
/// artifact is canonicalized; the receipt is canonicalized if it exists, else
/// its parent is canonicalized and the basename appended. If either cannot be
/// resolved, the result is an error (fail closed, no lexical fallback). The
/// forbidden-part check also still applies to the lexical spelling, so a path
/// that looked forbidden before still is.
///
/// The declared `Packet path` / `Receipt path` labels are metadata and may
/// match either the supplied lexical spelling or the physical path.
///
/// Canonicalization is point-in-time only; this makes no TOCTOU or
/// full-confinement claim.
pub fn storage_errors(
    text: &str,
    artifact: &Path,
    write_receipt: Option<&Path>,
    verify_receipt: Option<&Path>,
    windows: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    let declared_artifact =
        clean_path_value(&label_value(text, "**Packet path:**").unwrap_or_default());
    let declared_receipt =
        clean_path_value(&label_value(text, "**Receipt path:**").unwrap_or_default());
    let receipt_arg = write_receipt.or(verify_receipt);

    if !artifact.is_absolute() {
        errors.push("validator requires absolute handoff file path".to_string());
    }
    let physical_artifact = match resolve_physical_existing(artifact) {
        Ok(p) => p,
        Err(e) => {
            errors.push(format!(
                "handoff path cannot be resolved to a physical location: {e}"
            ));
            return errors;
        }
    };
    let lexical_artifact = normalized_path(&artifact.to_string_lossy(), windows);
    let physical_artifact_norm = physical_norm(&physical_artifact, windows);

    if !is_absolute_path(&declared_artifact) {
        errors.push("declared handoff packet path must be absolute".to_string());
    } else {
        let declared = normalized_path(&declared_artifact, windows);
        if declared != lexical_artifact && declared != physical_artifact_norm {
            errors.push("declared handoff packet path does not match validated file".to_string());
        }
    }

    if has_forbidden_part(&[lexical_artifact.as_str(), physical_artifact_norm.as_str()]) {
        errors.push("canonical handoff cannot use temporary/cache/review-run storage".to_string());
    }
    let suffix_is_md = physical_artifact
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase() == "md")
        .unwrap_or(false);
    if !suffix_is_md {
        errors.push("canonical handoff must be Markdown".to_string());
    }

    let Some(receipt_arg) = receipt_arg else {
        errors.push("validation must write or verify sidecar receipt".to_string());
        return errors;
    };
    if !receipt_arg.is_absolute() {
        errors.push("validator requires absolute receipt path".to_string());
    }
    let physical_receipt = match resolve_physical_receipt(receipt_arg) {
        Ok(p) => p,
        Err(e) => {
            errors.push(format!(
                "receipt path cannot be resolved to a physical location: {e}"
            ));
            return errors;
        }
    };
    let lexical_receipt = normalized_path(&receipt_arg.to_string_lossy(), windows);
    let physical_receipt_norm = physical_norm(&physical_receipt, windows);

    if !is_absolute_path(&declared_receipt) {
        errors.push("declared receipt path must be absolute".to_string());
    } else {
        let declared = normalized_path(&declared_receipt, windows);
        if declared != lexical_receipt && declared != physical_receipt_norm {
            errors.push(
                "declared receipt path does not match validator receipt argument".to_string(),
            );
        }
    }

    let receipt_parent = physical_receipt.parent().unwrap_or_else(|| Path::new(""));
    let artifact_parent = physical_artifact.parent().unwrap_or_else(|| Path::new(""));
    if physical_norm(receipt_parent, windows) != physical_norm(artifact_parent, windows) {
        errors.push("handoff receipt must be adjacent to canonical packet".to_string());
    }
    let stem = physical_artifact
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let expected_receipt = format!("{stem}.receipt.json");
    let receipt_name = physical_receipt
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if receipt_name != expected_receipt {
        errors.push(format!("handoff receipt must be named {expected_receipt}"));
    }
    if has_forbidden_part(&[lexical_receipt.as_str(), physical_receipt_norm.as_str()]) {
        errors.push("handoff receipt cannot use temporary/cache/review-run storage".to_string());
    }
    errors
}

/// Physical identity of an explicit durable artifact root outside the
/// workspace. The root must be an absolute, already-existing directory whose
/// resolved path (and lexical spelling) carries no temp/cache/scratch part.
/// Point-in-time only, like the packet and receipt checks.
pub fn resolve_artifact_root(root: &Path, windows: bool) -> Result<PathBuf, String> {
    let spelled = root.to_string_lossy();
    if !is_absolute_path(&spelled) {
        return Err(format!(
            "artifact root must be an absolute path: {}",
            root.display()
        ));
    }
    let physical = resolve_physical_existing(root)?;
    if !physical.is_dir() {
        return Err(format!(
            "artifact root is not a directory: {}",
            physical.display()
        ));
    }
    let lexical = normalized_path(&spelled, windows);
    let physical_norm = physical_norm(&physical, windows);
    if has_forbidden_part(&[lexical.as_str(), physical_norm.as_str()]) {
        return Err(format!(
            "artifact root cannot use temporary/cache/review-run/scratch storage: {}",
            physical.display()
        ));
    }
    Ok(physical)
}

/// Packet and receipt must sit inside one of the declared artifact roots, by
/// physical path. Each root is validated by [`resolve_artifact_root`] first.
pub fn artifact_root_errors(
    artifact: &Path,
    receipt: &Path,
    roots: &[PathBuf],
    windows: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut physical_roots = Vec::new();
    for root in roots {
        match resolve_artifact_root(root, windows) {
            Ok(p) => physical_roots.push(physical_norm(&p, windows)),
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return errors;
    }
    for (label, path) in [("handoff packet", artifact), ("handoff receipt", receipt)] {
        let physical = if label == "handoff packet" {
            resolve_physical_existing(path)
        } else {
            resolve_physical_receipt(path)
        };
        match physical {
            Ok(p) => {
                let norm = physical_norm(&p, windows);
                let inside = physical_roots
                    .iter()
                    .any(|r| norm == *r || norm.starts_with(&format!("{r}/")));
                if !inside {
                    errors.push(format!(
                        "{label} is outside every declared --artifact-root: {}",
                        p.display()
                    ));
                }
            }
            Err(e) => errors.push(format!("{label} cannot be resolved: {e}")),
        }
    }
    errors
}

pub const HEADINGS: &[&str] = &[
    "# COLD-START HANDOFF:",
    "## 0. Handoff Control",
    "## 1. Intent & Mission",
    "## 2. Current State",
    "## 3. Environment & Active Work",
    "## 4. Decisions, Invariants & User Corrections",
    "## 5. Artifacts & Evidence",
    "## 6. Failures, Dead Ends & Attempts",
    "## 7. Learnings, Gotchas & Landmines",
    "## 8. Open Loops & Context Gaps",
    "## 9. Safety, Authority & Boundaries",
    "## 10. Exact Resume Sequence",
    "## 11. State Verification & Invalidation",
    "## 12. First Output & Readback Contract",
    "## 13. Ready-to-Paste First Message",
    "## 14. Context Gap Report",
    "## 15. Handoff Author Gate",
];

pub const LABELS: &[&str] = &[
    "**Handoff ID:**",
    "**Created:**",
    "**Source task / chat:**",
    "**Target:**",
    "**Author:**",
    "**Receiver role:**",
    "**Proceed mode:**",
    "**Readiness:**",
    "**Handoff reason:**",
    "**Source evidence mode:**",
    "**Transcript evidence path:**",
    "**Source prefix receipt:**",
    "**Packet path:**",
    "**Receipt path:**",
    "**Original user intent verbatim:**",
    "**Underlying goal:**",
    "**Current objective:**",
    "**Definition of success:**",
    "**Out of scope:**",
    "**First responsibility:**",
    "**Must not do first:**",
    "**Phase:**",
    "**Completed:**",
    "**In progress:**",
    "**Blocked:**",
    "**Not started:**",
    "**Last action:**",
    "**Last observed result:**",
    "**Active goal / plan:**",
    "**Current hypothesis:**",
    "**Work type:**",
    "**Workspace / repo:**",
    "**Branch / version:**",
    "**Baseline revision:**",
    "**Dirty state:**",
    "**OS / shell:**",
    "**Tools / dependencies:**",
    "**Services / processes:**",
    "**Agents / tasks / threads:**",
    "**Scheduled work:**",
    "**Credentials / access:**",
    "**May do:**",
    "**Do not change:**",
    "**Do not run:**",
    "**Irreversible / production actions:**",
    "**Spend / external effects:**",
    "**Secrets handling:**",
    "**Reserved decisions:**",
    "**Verification command:**",
    "**Expected state:**",
    "**Invalidated by:**",
    "**Refresh action:**",
    "**Validator command:**",
    "**Receiver receipt check:**",
    "**First deliverable after readback:**",
    "**Gap report format:**",
    "**Gap summary:**",
    "**Safe-to-proceed scope:**",
    "**Fatal recovery owner:**",
    "**Exact recovery sequence:**",
];

pub const STEP_LABELS: &[&str] = &[
    "**Owner:**",
    "**Working directory / system:**",
    "**Exact action:**",
    "**Expected result:**",
    "**Evidence path:**",
    "**Timeout / retry:**",
    "**If failure:**",
    "**Depends on:**",
];

const GENERIC: &[&str] = &[
    "",
    "value",
    "example",
    "placeholder",
    "fixture-value",
    "todo",
    "tbd",
    "n/a",
    "none",
];
const ENUM_VALUES: &[&str] = &[
    "READY",
    "READY_WITH_GAPS",
    "NOT_READY",
    "IMMEDIATE",
    "READBACK_ONLY",
    "REVIEW_ONLY",
    "DECISION",
    "CODE",
    "DOCUMENT",
    "RESEARCH",
    "OPERATIONS",
    "MIXED",
    "LOCKED",
    "ACTIVE_ASSUMPTION",
    "REVISIT_ON",
    "FATAL",
    "HIGH",
    "MEDIUM",
    "LOW",
    "NONE",
];

fn banned_patterns() -> &'static [(&'static str, Regex)] {
    static RE: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
        vec![
            ("old-chat dependency", Regex::new(r"(?i)\b(as discussed|see above|previous chat|you already know|prior[- ](?:thread|chat|conversation) context|earlier[- ](?:thread|chat|conversation)|inherited context|remembered state|implicit credentials?|(?:old|earlier|prior)[- ](?:messages?|session|history))\b").unwrap()),
            ("orphan continuation", Regex::new(r"(?i)\b(continue where we left off|just continue|usual constraints apply)\b").unwrap()),
            ("weak gap handling", Regex::new(r"(?i)\b(ask me if anything is unclear|catch up on)\b").unwrap()),
            ("unfinished marker", Regex::new(r"\b(TODO|TBD|TK)\b").unwrap()),
        ]
    });
    &RE
}

pub(crate) fn secret_patterns() -> &'static [(&'static str, Regex)] {
    static RE: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
        vec![
            ("OpenAI-like key", Regex::new(r"\bsk-[A-Za-z0-9_-]{20,}\b").unwrap()),
            ("GitHub token", Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b").unwrap()),
            ("Slack token", Regex::new(r"\bxox[a-z]-[A-Za-z0-9-]{10,}\b").unwrap()),
            ("AWS access key", Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap()),
            ("private key", Regex::new(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----").unwrap()),
            ("literal secret assignment", Regex::new(r#"(?i)\b(password|api[_-]?key|secret|token)\s*[:=]\s*[`"']?[A-Za-z0-9+/=_-]{12,}"#).unwrap()),
            ("authorization header", Regex::new(r"(?i)\bAuthorization\s*:\s*(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{12,}").unwrap()),
            ("bearer credential", Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{20,}").unwrap()),
        ]
    });
    &RE
}

fn step_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^### Resume Step\s+\d+\s+—\s+.+$").unwrap());
    &RE
}
fn path_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:[A-Za-z]:[\\/]|/)[^\s`|]+").unwrap());
    &RE
}
fn action_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(run|read|search|inspect|verify|capture|compare|resume|execute|open|check|record|load|query|test)\b").unwrap()
    });
    &RE
}
fn placeholder_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{[^{}\n]+\}\}").unwrap());
    &RE
}

/// Minimal whitespace tokenizer for a single documented validator command
/// line. Single/double quotes group a token (no escapes; backslashes are
/// literal so Windows paths survive). Returns `None` on an unterminated quote.
/// This is deliberately NOT a shell parser: operators such as `;`, `&&`, `|`
/// are just ordinary tokens and therefore fail the exact-shape checks below.
fn split_command_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                    in_token = true;
                } else if c.is_whitespace() {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                } else {
                    current.push(c);
                    in_token = true;
                }
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_token {
        tokens.push(current);
    }
    Some(tokens)
}

fn token_basename(token: &str) -> String {
    token
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Legacy interpreter executable kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interpreter {
    /// `python`, `python<digits>`, `python<digits>.<digits>` (optional `.exe`).
    Python,
    /// Windows `py` launcher: `py` / `py.exe`.
    PyLauncher,
}

/// Classifies the executable token by basename (case-insensitive, optional
/// single `.exe`, any directory prefix). Malformed names such as `python.`,
/// `python3.`, `python..`, `python3.12.1`, `python3x` are not interpreters.
fn interpreter_kind(token: &str) -> Option<Interpreter> {
    let base = token_basename(token).to_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    if base == "py" {
        return Some(Interpreter::PyLauncher);
    }
    let version = base.strip_prefix("python")?;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let ok = match version.split_once('.') {
        None => version.is_empty() || digits(version),
        Some((major, minor)) => digits(major) && digits(minor),
    };
    ok.then_some(Interpreter::Python)
}

/// Exact `py` launcher selector token: `-2` or `-3`, optionally `.<digits>`,
/// optionally `-32`, `-64`, or `-arm64` (e.g. `-3`, `-3.11`, `-3.12-64`).
/// Rejects `-0`, `-0p`, `-V:...`, `--list`, `-3.`, `-4`, `-3-16`, etc.
fn is_py_launcher_selector(token: &str) -> bool {
    let Some(body) = token.strip_prefix('-') else {
        return false;
    };
    let (version, arch) = match body.split_once('-') {
        Some((v, a)) => (v, Some(a)),
        None => (body, None),
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let mut parts = version.split('.');
    let major_ok = matches!(parts.next(), Some("2" | "3"));
    let minor_ok = parts.next().is_none_or(digits);
    major_ok
        && minor_ok
        && parts.next().is_none()
        && arch.is_none_or(|a| matches!(a, "32" | "64" | "arm64"))
}

/// Allow-list for interpreter options after the (optional) launcher selector
/// and before the script: only no-argument file-mode switches, single letters
/// from `bBdEIOPqRsSuv`, optionally clustered (`-IB`, `-bb`, `-OO`). Everything
/// else is rejected, notably `-c`/`-m` (and attached forms like `-cprint(1)`,
/// `-mpkg`, `-Ic`), argument-taking `-W`/`-X`, `-V`/`-h` (which do not run the
/// script), `-i`, `-x`, `-`, `--`, every `--long` option, and ANY launcher
/// selector or `-0` form: a selector is not a generic repeatable flag, so it is
/// only legal immediately after `py` (see [`validator_args`]).
fn python_flag_is_supported(token: &str) -> bool {
    let Some(body) = token.strip_prefix('-') else {
        return false;
    };
    !body.is_empty() && body.chars().all(|c| "bBdEIOPqRsSuv".contains(c))
}

/// Returns the argument tokens after the validator program if (and only if)
/// the command starts with a recognized validator invocation:
/// - native: `<program> <script verb> handoff/validate-handoff ...` (see [`crate::HandoffHost`])
/// - legacy FILE-MODE, exact grammar:
///   `[<python-exe> | <py-exe> [SELECTOR]] [NOARG_FLAG...] <path>/validate-handoff.py ...`
///   - `<python-exe>`: basename `python`, `python<digits>`, or
///     `python<digits>.<digits>` (optional `.exe`/directory);
///   - `<py-exe>`: `py`/`py.exe`; only it may carry at most ONE selector,
///     and only immediately after the executable (see
///     [`is_py_launcher_selector`]; the real launcher honors only a first-arg
///     control, and `-0`/`--list`/version/help print and exit);
///   - then only known no-argument flags ([`python_flag_is_supported`]);
///   - then the validator script by exact basename.
///     Any other option returns `None`, because it could stop the script from
///     being what is actually executed.
fn validator_args(tokens: &[String]) -> Option<&[String]> {
    if let [a, b, c, rest @ ..] = tokens {
        let host = crate::handoff_host();
        if *a == host.program && *b == host.script_verb && c == "handoff/validate-handoff" {
            return Some(rest);
        }
    }
    let mut idx = 0;
    if let Some(kind) = tokens.first().and_then(|t| interpreter_kind(t)) {
        idx = 1;
        if kind == Interpreter::PyLauncher
            && tokens.get(idx).is_some_and(|t| is_py_launcher_selector(t))
        {
            idx += 1;
        }
        while let Some(token) = tokens.get(idx) {
            if !token.starts_with('-') {
                break;
            }
            if !python_flag_is_supported(token) {
                return None;
            }
            idx += 1;
        }
    }
    let script = tokens.get(idx)?;
    if token_basename(script) == "validate-handoff.py" {
        Some(&tokens[idx + 1..])
    } else {
        None
    }
}

/// True iff the tokenized command carries `flag` as a standalone token.
pub(crate) fn command_has_flag(command: &str, flag: &str) -> bool {
    split_command_tokens(command)
        .map(|tokens| tokens.iter().any(|token| token == flag))
        .unwrap_or(false)
}

fn same_bound_path(token: &str, declared: &str) -> bool {
    let token = clean_path_value(token);
    let declared = clean_path_value(declared);
    !declared.is_empty() && normalized_path(&token, false) == normalized_path(&declared, false)
}

/// True iff `command` is exactly one validator invocation (native or legacy)
/// whose only arguments are the declared packet path and `flag` followed by
/// the declared receipt path (packet-first or flag-first). Any extra token
/// (including `--template-self-check`, a second flag, or shell operators),
/// similarly named program, wrong flag, or unbound/different path is rejected.
/// Path equality is lexical metadata comparison, not filesystem identity.
fn command_binds_validator(command: &str, flag: &str, packet: &str, receipt: &str) -> bool {
    let Some(tokens) = split_command_tokens(command) else {
        return false;
    };
    let Some(args) = validator_args(&tokens) else {
        return false;
    };
    // Continuity and storage inputs are plain `--flag <value>` pairs the
    // packet records so the receiver can rerun the exact validation.
    let mut stripped: Vec<&String> = Vec::new();
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if matches!(
            token.as_str(),
            "--context" | "--context-receipt" | "--artifact-root"
        ) {
            if iter.next().is_none() {
                return false;
            }
            continue;
        }
        stripped.push(token);
    }
    let args: Vec<String> = stripped.into_iter().cloned().collect();
    let args = args.as_slice();
    match args {
        [p, f, r] if f == flag => same_bound_path(p, packet) && same_bound_path(r, receipt),
        [f, r, p] if f == flag => same_bound_path(p, packet) && same_bound_path(r, receipt),
        _ => false,
    }
}

pub fn label_value(text: &str, label: &str) -> Option<String> {
    let escaped = regex::escape(label);
    let re = Regex::new(&format!(r"(?m)^-\s+{escaped}\s*(.*)$")).ok()?;
    re.captures(text).map(|c| c[1].trim().to_string())
}

pub fn concrete(value: &str) -> bool {
    let raw = value.trim().trim_matches('`');
    if ENUM_VALUES.contains(&raw) {
        return true;
    }
    let normalized = raw.to_lowercase();
    normalized.chars().count() >= 8 && !GENERIC.contains(&normalized.as_str())
}

pub fn fenced_after(text: &str, label: &str) -> Option<String> {
    let escaped = regex::escape(label);
    let re = Regex::new(&format!(r"(?s){escaped}\s*\n+\s*```[^\n]*\n(.*?)\n```")).ok()?;
    re.captures(text).map(|c| c[1].trim().to_string())
}

pub fn table_rows(text: &str, start: &str, end: &str) -> Vec<Vec<String>> {
    let Some(left) = text.find(start) else {
        return Vec::new();
    };
    let Some(right_rel) = text[left + start.len()..].find(end) else {
        return Vec::new();
    };
    let right = left + start.len() + right_rel;
    let mut rows = Vec::new();
    let sep_re = Regex::new(r"^:?-{3,}:?$").unwrap();
    for line in text[left..right].lines() {
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if cells.iter().all(|c| sep_re.is_match(c)) {
            continue;
        }
        rows.push(cells);
    }
    rows
}

pub fn table_errors(name: &str, rows: &[Vec<String>], width: usize, template: bool) -> Vec<String> {
    if rows.len() < 2 {
        return vec![format!("{name} requires at least one data row")];
    }
    let mut errors = Vec::new();
    for (row_index, row) in rows[1..].iter().enumerate() {
        let row_index = row_index + 1;
        if row.len() != width {
            errors.push(format!("{name} row {row_index} must contain {width} cells"));
            continue;
        }
        if !template {
            for (cell_index, cell) in row.iter().enumerate() {
                let cell_index = cell_index + 1;
                if !concrete(cell) {
                    errors.push(format!(
                        "{name} row {row_index} cell {cell_index} is too vague"
                    ));
                }
            }
        }
    }
    errors
}

pub fn ordered_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut cursor: i64 = -1;
    for &heading in HEADINGS {
        match text.find(heading) {
            None => errors.push(format!("missing heading: {heading}")),
            Some(index) => {
                let index = index as i64;
                if index <= cursor {
                    errors.push(format!("heading out of order: {heading}"));
                } else {
                    cursor = index;
                }
            }
        }
    }
    errors
}

pub fn resume_errors(text: &str, template: bool) -> Vec<String> {
    let matches: Vec<_> = step_re().find_iter(text).collect();
    if matches.len() < 3 {
        return vec!["resume sequence requires at least 3 steps".to_string()];
    }
    let mut errors = Vec::new();
    let section_end = text
        .find("## 11. State Verification & Invalidation")
        .unwrap_or(text.len());
    for (index, m) in matches.iter().enumerate() {
        let end = matches
            .get(index + 1)
            .map(|n| n.start())
            .unwrap_or(section_end);
        let block = &text[m.start()..end];
        let name = m.as_str();
        for &label in STEP_LABELS {
            if !block.contains(label) {
                errors.push(format!("{name} missing label: {label}"));
            }
        }
        let action = fenced_after(block, "**Exact action:**");
        if action.is_none() {
            errors.push(format!("{name} lacks fenced exact action"));
        }
        if template {
            continue;
        }
        for &label in STEP_LABELS {
            if label == "**Exact action:**" {
                continue;
            }
            let value = label_value(block, label).unwrap_or_default();
            if !concrete(&value) {
                errors.push(format!("{name} has non-concrete value: {label}"));
            }
        }
        let action_ok = action
            .as_ref()
            .map(|a| concrete(a) && action_re().is_match(a))
            .unwrap_or(false);
        if !action_ok {
            errors.push(format!("{name} exact action is not executable/actionable"));
        }
        for label in ["**Working directory / system:**", "**Evidence path:**"] {
            let value = label_value(block, label).unwrap_or_default();
            if !path_re().is_match(&value) {
                errors.push(format!("{name} {label} lacks explicit path"));
            }
        }
        let timeout_value = label_value(block, "**Timeout / retry:**").unwrap_or_default();
        if !timeout_value.chars().any(|c| c.is_ascii_digit()) {
            errors.push(format!("{name} timeout/retry lacks numeric bound"));
        }
    }
    errors
}

pub fn validate(text: &str, template: bool) -> Vec<String> {
    let mut errors = ordered_errors(text);
    for &label in LABELS {
        match label_value(text, label) {
            None => errors.push(format!("missing label: {label}")),
            Some(value) => {
                let is_optional_empty = matches!(
                    label,
                    "**Verification command:**"
                        | "**Validator command:**"
                        | "**Receiver receipt check:**"
                );
                if value.is_empty() && !is_optional_empty {
                    errors.push(format!("empty value: {label}"));
                } else if !template
                    && GENERIC.contains(&value.trim().trim_matches('`').to_lowercase().as_str())
                {
                    errors.push(format!("generic filler value: {label}"));
                }
            }
        }
    }

    errors.extend(resume_errors(text, template));

    let tables: [(&str, &str, &str, usize); 5] = [
        (
            "decision log",
            "## 4. Decisions, Invariants & User Corrections",
            "## 5. Artifacts & Evidence",
            5,
        ),
        (
            "artifact map",
            "## 5. Artifacts & Evidence",
            "## 6. Failures, Dead Ends & Attempts",
            6,
        ),
        (
            "failure map",
            "## 6. Failures, Dead Ends & Attempts",
            "## 7. Learnings, Gotchas & Landmines",
            7,
        ),
        (
            "gotcha map",
            "## 7. Learnings, Gotchas & Landmines",
            "## 8. Open Loops & Context Gaps",
            4,
        ),
        (
            "gap map",
            "## 8. Open Loops & Context Gaps",
            "## 9. Safety, Authority & Boundaries",
            6,
        ),
    ];
    let mut parsed: std::collections::BTreeMap<&str, Vec<Vec<String>>> =
        std::collections::BTreeMap::new();
    for (name, start, end, width) in tables {
        let rows = table_rows(text, start, end);
        errors.extend(table_errors(name, &rows, width, template));
        parsed.insert(name, rows);
    }

    if !template {
        let readiness = label_value(text, "**Readiness:**").unwrap_or_default();
        let readiness = readiness.trim_matches('`');
        if !["READY", "READY_WITH_GAPS", "NOT_READY"].contains(&readiness) {
            errors.push("Readiness must be READY, READY_WITH_GAPS, or NOT_READY".to_string());
        }
        let proceed = label_value(text, "**Proceed mode:**").unwrap_or_default();
        let proceed = proceed.trim_matches('`');
        if !["IMMEDIATE", "READBACK_ONLY", "REVIEW_ONLY", "DECISION"].contains(&proceed) {
            errors.push("invalid Proceed mode".to_string());
        }
        let work_type = label_value(text, "**Work type:**").unwrap_or_default();
        let work_type = work_type.trim_matches('`');
        if !["CODE", "DOCUMENT", "RESEARCH", "OPERATIONS", "MIXED"].contains(&work_type) {
            errors.push("invalid Work type".to_string());
        }
        let source_mode = label_value(text, "**Source evidence mode:**").unwrap_or_default();
        let source_mode = source_mode.trim_matches('`').to_string();
        let evidence_path = label_value(text, "**Transcript evidence path:**").unwrap_or_default();
        let source_receipt = label_value(text, "**Source prefix receipt:**").unwrap_or_default();
        if !["TRANSCRIPT_INGEST", "LIVE_CONTEXT"].contains(&source_mode.as_str()) {
            errors
                .push("Source evidence mode must be TRANSCRIPT_INGEST or LIVE_CONTEXT".to_string());
        }
        if source_mode == "TRANSCRIPT_INGEST" {
            if !path_re().is_match(&evidence_path) {
                errors.push("TRANSCRIPT_INGEST requires absolute context path".to_string());
            }
            let upper = source_receipt.to_uppercase();
            for token in [
                "PLATFORM:",
                "SESSION_ID:",
                "CUTOFF_BYTES:",
                "SHA256:",
                "PARSER_VERSION:",
            ] {
                if !upper.contains(token) {
                    errors.push(format!(
                        "TRANSCRIPT_INGEST source prefix receipt missing {token}"
                    ));
                }
            }
            if !Regex::new(r"(?i)\bSHA256:[0-9a-f]{64}\b")
                .unwrap()
                .is_match(&source_receipt)
            {
                errors.push(
                    "TRANSCRIPT_INGEST source prefix receipt requires 64-hex SHA256".to_string(),
                );
            }
            if !Regex::new(r"(?i)\bCUTOFF_BYTES:[1-9]\d*\b")
                .unwrap()
                .is_match(&source_receipt)
            {
                errors.push(
                    "TRANSCRIPT_INGEST source prefix receipt requires positive cutoff".to_string(),
                );
            }
        } else {
            let na_re = Regex::new(r"(?i)^NOT_APPLICABLE:\s*\S").unwrap();
            if !na_re.is_match(&evidence_path) {
                errors.push(
                    "LIVE_CONTEXT transcript evidence path requires NOT_APPLICABLE reason"
                        .to_string(),
                );
            }
            if !na_re.is_match(&source_receipt) {
                errors.push(
                    "LIVE_CONTEXT source prefix receipt requires NOT_APPLICABLE reason".to_string(),
                );
            }
        }

        let mut severities: Vec<String> = Vec::new();
        if let Some(rows) = parsed.get("gap map") {
            for row in rows.iter().skip(1) {
                if row.len() == 6 {
                    let severity = row[1].trim_matches('`').to_string();
                    if !["FATAL", "HIGH", "MEDIUM", "LOW", "NONE"].contains(&severity.as_str()) {
                        errors.push(format!("invalid gap severity: {severity}"));
                    }
                    severities.push(severity);
                }
            }
        }
        if readiness == "READY" && severities.iter().any(|s| s == "FATAL" || s == "HIGH") {
            errors.push("READY cannot contain FATAL or HIGH gap".to_string());
        }
        if readiness == "NOT_READY" && !severities.iter().any(|s| s == "FATAL") {
            errors.push("NOT_READY requires FATAL gap".to_string());
        }
        if readiness == "READY_WITH_GAPS" && severities.iter().all(|s| s == "NONE") {
            errors.push("READY_WITH_GAPS requires declared gap".to_string());
        }
        if readiness == "READY_WITH_GAPS" && severities.iter().any(|s| s == "FATAL") {
            errors.push("READY_WITH_GAPS cannot contain FATAL gap; use NOT_READY".to_string());
        }

        let verification = fenced_after(text, "**Verification command:**");
        let verification_ok = verification
            .as_ref()
            .map(|v| concrete(v) && action_re().is_match(v))
            .unwrap_or(false);
        if !verification_ok {
            errors.push("state verification lacks executable fenced command/action".to_string());
        }
        let validator_command = fenced_after(text, "**Validator command:**").unwrap_or_default();
        let receiver_command =
            fenced_after(text, "**Receiver receipt check:**").unwrap_or_default();
        let declared_packet =
            clean_path_value(&label_value(text, "**Packet path:**").unwrap_or_default());
        let declared_receipt =
            clean_path_value(&label_value(text, "**Receipt path:**").unwrap_or_default());
        for (label, command, flag) in [
            ("Validator command", &validator_command, "--write-receipt"),
            (
                "Receiver receipt check",
                &receiver_command,
                "--verify-receipt",
            ),
        ] {
            if !command_binds_validator(command, flag, &declared_packet, &declared_receipt) {
                errors.push(format!(
                    "{label} must execute `{}` \
                     (or legacy validate-handoff.py) with bound packet/receipt paths and {flag}",
                    crate::handoff_host().script_command("handoff/validate-handoff")
                ));
            }
        }
        for label in [
            "**Packet path:**",
            "**Receipt path:**",
            "**Workspace / repo:**",
        ] {
            let value = label_value(text, label).unwrap_or_default();
            if !path_re().is_match(&value) {
                errors.push(format!("{label} lacks explicit path"));
            }
        }
        let first_start = text
            .find("## 13. Ready-to-Paste First Message")
            .unwrap_or(0);
        let first_end = text.find("## 14. Context Gap Report").unwrap_or(text.len());
        let first_message = if first_end > first_start {
            &text[first_start..first_end]
        } else {
            ""
        };
        if !first_message.contains("READBACK") || !first_message.contains("Proceed mode") {
            errors.push("first message must require READBACK + Proceed mode".to_string());
        }
    }

    let author_start = text
        .find("## 15. Handoff Author Gate")
        .unwrap_or(text.len());
    let author = &text[author_start..];
    if !template {
        if placeholder_re().is_match(text) {
            errors.push("unfilled placeholder remains".to_string());
        }
        if author.contains("- [ ]") {
            errors.push("handoff author gate contains unchecked item".to_string());
        }
        if author.matches("- [x]").count() < 18 {
            errors.push("handoff author gate requires 18 checked items".to_string());
        }
    }

    for (name, pattern) in banned_patterns() {
        if let Some(m) = pattern.find(text) {
            errors.push(format!("{name}: forbidden phrase '{}'", m.as_str()));
        }
    }
    if !template {
        for (name, pattern) in secret_patterns() {
            if pattern.is_match(text) {
                errors.push(format!("possible secret detected: {name}"));
            }
        }
    }

    let mut unique: Vec<String> = errors
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    unique.sort();
    unique
}

/// Convenience: absolute-path variant of `normalized_path` for callers that
/// already hold a `PathBuf` (mirrors `Path.resolve()` call sites in Python).
pub fn normalized_pathbuf(value: &Path, windows: bool) -> String {
    normalized_path(&value.to_string_lossy(), windows)
}

#[allow(dead_code)]
fn _unused(_p: PathBuf) {}
