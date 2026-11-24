use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// Workspace state folder name (with its leading dot) until the host calls
/// [`set_state_dir_name`]. The folder's `runtime` subdirectory holds the daemon discovery file
/// (it carries the API token), so `**/<state-dir>/runtime/**` is denied for read and write.
pub const DEFAULT_STATE_DIR_NAME: &str = ".rightkit";

static HOST_STATE_DIR_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the host's workspace state folder name, with its leading dot (for example `.myapp`).
/// Call once at startup before any engine or [`DenyGlobSet`] is built; returns `false` (and
/// changes nothing) if a name was already set or `name` is empty. Hosts that also use
/// `rightkit-agent-config` set the same name there through its own `set_state_dir_name`.
pub fn set_state_dir_name(name: &str) -> bool {
    !name.is_empty() && HOST_STATE_DIR_NAME.set(name.to_string()).is_ok()
}

/// The workspace state folder name in effect.
pub fn state_dir_name() -> &'static str {
    HOST_STATE_DIR_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_STATE_DIR_NAME)
}

/// The glob that denies the state folder's `runtime` directory (`**/<state-dir>/runtime/**`).
fn runtime_deny_glob() -> String {
    format!("**/{}/runtime/**", state_dir_name())
}

/// [`DEFAULT_READ_DENY`] with the runtime glob of the state folder in effect
/// ([`state_dir_name`]). Equal to [`DEFAULT_READ_DENY`] until the host sets a name.
pub fn default_read_deny() -> Vec<String> {
    with_runtime_glob(DEFAULT_READ_DENY)
}

/// [`DEFAULT_WRITE_DENY`] with the runtime glob of the state folder in effect
/// ([`state_dir_name`]). Equal to [`DEFAULT_WRITE_DENY`] until the host sets a name.
pub fn default_write_deny() -> Vec<String> {
    with_runtime_glob(DEFAULT_WRITE_DENY)
}

fn with_runtime_glob(list: &[&str]) -> Vec<String> {
    list.iter()
        .map(|s| {
            if *s == "**/.rightkit/runtime/**" {
                runtime_deny_glob()
            } else {
                s.to_string()
            }
        })
        .collect()
}

/// Default paths that are always denied for reads (with the default state folder
/// `.rightkit`; see [`default_read_deny`] for the list in effect). This is the *only*
/// filesystem protection on macOS/Windows (Landlock is Linux-only), so the list
/// covers the common cloud/package/key credential stores, not just `.ssh`/`.env`.
pub const DEFAULT_READ_DENY: &[&str] = &[
    "**/.ssh/**",
    "**/AppData/**",
    "**/.env",
    "**/*secret*",
    "**/*credential*",
    // Cloud provider credentials
    "**/.aws/**",
    "**/.config/gcloud/**",
    "**/.azure/**",
    "**/.kube/config",
    "**/.docker/config.json",
    // Package-registry / network auth
    "**/.npmrc",
    "**/.pypirc",
    "**/.netrc",
    // Key material
    "**/.gnupg/**",
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/*.jks",
    // Host runtime credentials (daemon discovery file carries the API token); the last entry follows `set_state_dir_name`
    "**/daemon.port",
    "**/daemon.lock",
    "**/.rightkit/runtime/**",
];

/// Default paths that are always denied for writes (with the default state folder
/// `.rightkit`; see [`default_write_deny`] for the list in effect). Beyond VCS/build internals,
/// this blocks shell-init and auth files an agent could use to persist itself.
pub const DEFAULT_WRITE_DENY: &[&str] = &[
    ".git/**",
    "**/.git/**",
    "**/.env",
    "**/node_modules/**",
    "**/target/**",
    // Shell init / persistence vectors
    "**/.bashrc",
    "**/.zshrc",
    "**/.profile",
    "**/.bash_profile",
    "**/.gitconfig",
    "**/authorized_keys",
    // Host runtime credentials / owning-process state; the last entry follows `set_state_dir_name`
    "**/daemon.port",
    "**/daemon.lock",
    "**/.rightkit/runtime/**",
];

/// The subset of [`DEFAULT_READ_DENY`] the Linux shell sandbox withholds from the
/// Landlock ruleset, matched on a workspace-relative path: `.env`, `*.pem`,
/// `daemon.port`, `daemon.lock` (files) and `<state-dir>/runtime` (directory), where the
/// state folder is `.rightkit` until the host calls [`set_state_dir_name`].
/// Names compare case-insensitively, like the globs. The state folder is the one in
/// effect ([`state_dir_name`]); [`shell_withholds_read_in`] takes the name explicitly.
///
/// `*secret*`, `*credential*` and `*.key` are excluded on purpose: they match source
/// files and directories (`secrets.rs`, `credentials.go`, `foo.key` fixtures), and
/// withholding those would break compilers and test runners.
pub fn shell_withholds_read(rel: &std::path::Path, is_dir: bool) -> bool {
    shell_withholds_read_in(rel, is_dir, state_dir_name())
}

/// [`shell_withholds_read`] for a host whose runtime directory lives under
/// `state_dir_name` (for example `.coderight`) instead of `.rightkit`.
pub fn shell_withholds_read_in(rel: &std::path::Path, is_dir: bool, state_dir_name: &str) -> bool {
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if is_dir {
        return name.eq_ignore_ascii_case("runtime")
            && rel
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(state_dir_name));
    }
    name.eq_ignore_ascii_case(".env")
        || name.eq_ignore_ascii_case("daemon.port")
        || name.eq_ignore_ascii_case("daemon.lock")
        || name.to_ascii_lowercase().ends_with(".pem")
}

/// Error type for deny-glob construction.
#[derive(Debug, thiserror::Error)]
pub enum DenyGlobError {
    #[error("invalid glob pattern '{pattern}': {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    /// File tools match a workspace-relative spelling, so an absolute glob never matched a
    /// workspace file while looking like a protection. It is refused instead.
    #[error(
        "deny glob '{pattern}' is an absolute path, but deny globs match workspace-relative \
         paths; write '**/{suggestion}' instead"
    )]
    AbsolutePattern { pattern: String, suggestion: String },
}

/// Path deny lists (spec §13). Secrets and VCS internals are blocked regardless of mode/trust —
/// the most-restrictive backstop. Globs are matched case-insensitively against a
/// forward-slash-normalized path.
///
/// Note: `globset` handles `**/` and `**` with case-insensitive matching, closely mirroring
/// the C# hand-rolled regex translation. The C# engine also treated `**/` as "any number of
/// directories *including none*" — `globset` does the same when building globs with
/// `case_insensitive(true)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenyGlobSet {
    read_patterns: Vec<String>,
    write_patterns: Vec<String>,
    #[serde(skip)]
    read_set: Option<GlobSet>,
    #[serde(skip)]
    write_set: Option<GlobSet>,
}

impl DenyGlobSet {
    /// Every read-deny pattern this set matches on, defaults included and in build order, so a
    /// host can report which ones it cannot enforce on a given platform. Subtract
    /// [`default_read_deny`] to get the user-configured ones alone.
    #[must_use]
    pub fn read_patterns(&self) -> &[String] {
        &self.read_patterns
    }

    /// Every write-deny pattern this set matches on, the counterpart of [`Self::read_patterns`].
    #[must_use]
    pub fn write_patterns(&self) -> &[String] {
        &self.write_patterns
    }

    /// Build with the default deny lists.
    pub fn new() -> Self {
        Self::with_patterns(default_read_deny(), default_write_deny())
            .expect("default deny globs are always valid")
    }

    /// Default lists extended with user-configured patterns (`[permissions]
    /// deny-read-globs` / `deny-write-globs`). Defaults are never removed.
    pub fn with_additional_patterns(
        extra_read: impl IntoIterator<Item = String>,
        extra_write: impl IntoIterator<Item = String>,
    ) -> Result<Self, DenyGlobError> {
        Self::with_patterns(
            default_read_deny().into_iter().chain(extra_read),
            default_write_deny().into_iter().chain(extra_write),
        )
    }

    /// Build from custom pattern lists. Returns an error if any pattern is invalid.
    pub fn with_patterns(
        read_deny: impl IntoIterator<Item = String>,
        write_deny: impl IntoIterator<Item = String>,
    ) -> Result<Self, DenyGlobError> {
        let read_patterns: Vec<String> = read_deny.into_iter().collect();
        let write_patterns: Vec<String> = write_deny.into_iter().collect();

        if let Some(pattern) = read_patterns
            .iter()
            .chain(&write_patterns)
            .find(|pattern| is_absolute_pattern(pattern))
        {
            let normalized = pattern.replace('\\', "/");
            let suggestion = normalized
                .rsplit('/')
                .find(|segment| !segment.is_empty())
                .unwrap_or("*")
                .to_string();
            return Err(DenyGlobError::AbsolutePattern {
                pattern: pattern.clone(),
                suggestion,
            });
        }

        let read_set = build_glob_set(&read_patterns, true)?;
        let write_set = build_glob_set(&write_patterns, true)?;

        Ok(Self {
            read_patterns,
            write_patterns,
            read_set: Some(read_set),
            write_set: Some(write_set),
        })
    }

    /// Returns `true` if `path` matches any read-deny glob.
    pub fn is_read_denied(&self, path: &str) -> bool {
        let p = normalize(path);
        let c = collapse_dots(&p);
        self.read_set
            .as_ref()
            .map(|s| s.is_match(&*p) || s.is_match(&c))
            .unwrap_or(false)
    }

    /// Returns `true` if `path` matches any write-deny glob.
    pub fn is_write_denied(&self, path: &str) -> bool {
        let p = normalize(path);
        let c = collapse_dots(&p);
        self.write_set
            .as_ref()
            .map(|s| s.is_match(&*p) || s.is_match(&c))
            .unwrap_or(false)
    }

    /// Returns the first matching write-deny glob pattern string, or `None`.
    pub fn first_write_match(&self, path: &str) -> Option<&str> {
        first_match(&self.write_patterns, self.write_set.as_ref()?, path)
    }

    /// Returns the first matching read-deny glob pattern string, or `None`.
    pub fn first_read_match(&self, path: &str) -> Option<&str> {
        first_match(&self.read_patterns, self.read_set.as_ref()?, path)
    }
}

impl Default for DenyGlobSet {
    fn default() -> Self {
        Self::new()
    }
}

// Normalize a path: convert backslashes to forward slashes, strip a leading `./`.
fn normalize(path: &str) -> std::borrow::Cow<'_, str> {
    let p = if path.contains('\\') {
        std::borrow::Cow::Owned(path.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(path)
    };

    match p.strip_prefix("./") {
        Some(rest) => std::borrow::Cow::Owned(rest.to_string()),
        None => p,
    }
}

/// Lexically collapse `.`, `..` and repeated slashes so `a/../.env` or
/// `x//.git/config` cannot dodge a glob. Callers test BOTH the raw and the
/// collapsed form, so collapsing can only add denials, never remove one.
pub(crate) fn collapse_dots(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let absolute = slashed.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in slashed.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if matches!(parts.last(), Some(last) if *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Build a `GlobSet` from a slice of pattern strings with optional case-insensitivity.
fn build_glob_set(patterns: &[String], case_insensitive: bool) -> Result<GlobSet, DenyGlobError> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        // Honor case-insensitivity at glob-build time so a write to `.ENV` is still
        // caught by `**/.env` — critical on Windows/macOS, where the FS treats
        // `.ENV` and `.env` as the same secret file but a case-sensitive glob would
        // not, letting a model dodge the deny by varying case.
        let glob = GlobBuilder::new(pat)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| DenyGlobError::InvalidGlob {
                pattern: pat.clone(),
                source: e,
            })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| DenyGlobError::InvalidGlob {
        pattern: "<set>".to_string(),
        source: e,
    })
}

/// Walk each pattern individually to find which one first fires.
/// `globset` doesn't expose which pattern matched in a set, so we check one-by-one.
fn first_match<'a>(patterns: &'a [String], set: &GlobSet, path: &str) -> Option<&'a str> {
    let p = normalize(path);
    let c = collapse_dots(&p);
    // Fast path: does anything match at all?
    if !set.is_match(&*p) && !set.is_match(&c) {
        return None;
    }
    // Slow path: find first individual pattern.
    for pattern in patterns {
        // Build a single-pattern glob set to check.
        if let Ok(g) = GlobBuilder::new(pattern).case_insensitive(true).build() {
            let m = g.compile_matcher();
            if m.is_match(&*p) || m.is_match(&c) {
                return Some(pattern.as_str());
            }
        }
    }
    None
}

/// True for a glob written as an absolute path: a leading `/` or `\\`, or a drive prefix such
/// as `C:/` or `C:\\`.
fn is_absolute_pattern(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    matches!(bytes.first(), Some(b'/' | b'\\'))
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}
