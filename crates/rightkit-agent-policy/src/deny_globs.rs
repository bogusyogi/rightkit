use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// Default paths that are always denied for reads. This is the *only*
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
    // Host runtime credentials (hosts with their own state dir add `**/<state-dir>/runtime/**` via `with_additional_patterns`) (daemon discovery file carries the API token)
    "**/daemon.port",
    "**/daemon.lock",
    "**/.rightkit/runtime/**",
];

/// Default paths that are always denied for writes. Beyond VCS/build internals,
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
    // Host runtime credentials (hosts with their own state dir add `**/<state-dir>/runtime/**` via `with_additional_patterns`) / owning-process state
    "**/daemon.port",
    "**/daemon.lock",
    "**/.rightkit/runtime/**",
];

/// Error type for deny-glob construction.
#[derive(Debug, thiserror::Error)]
pub enum DenyGlobError {
    #[error("invalid glob pattern '{pattern}': {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
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
    /// Build with the default deny lists.
    pub fn new() -> Self {
        Self::with_patterns(
            DEFAULT_READ_DENY.iter().map(|s| s.to_string()),
            DEFAULT_WRITE_DENY.iter().map(|s| s.to_string()),
        )
        .expect("default deny globs are always valid")
    }

    /// Default lists extended with user-configured patterns (`[permissions]
    /// deny-read-globs` / `deny-write-globs`). Defaults are never removed.
    pub fn with_additional_patterns(
        extra_read: impl IntoIterator<Item = String>,
        extra_write: impl IntoIterator<Item = String>,
    ) -> Result<Self, DenyGlobError> {
        Self::with_patterns(
            DEFAULT_READ_DENY
                .iter()
                .map(|s| s.to_string())
                .chain(extra_read),
            DEFAULT_WRITE_DENY
                .iter()
                .map(|s| s.to_string())
                .chain(extra_write),
        )
    }

    /// Build from custom pattern lists. Returns an error if any pattern is invalid.
    pub fn with_patterns(
        read_deny: impl IntoIterator<Item = String>,
        write_deny: impl IntoIterator<Item = String>,
    ) -> Result<Self, DenyGlobError> {
        let read_patterns: Vec<String> = read_deny.into_iter().collect();
        let write_patterns: Vec<String> = write_deny.into_iter().collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_dot_and_double_slash_cannot_dodge_a_deny_glob() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied("src/../.git/config"));
        assert!(dgs.is_write_denied("src//.git//config"));
        assert!(dgs.is_read_denied("a/b/../../.ssh/id_rsa"));
        assert!(dgs.is_read_denied("work\\..\\.aws\\credentials"));
        assert!(!dgs.is_write_denied("src/../lib/main.rs"));
    }

    #[test]
    fn runtime_credential_files_are_denied_for_read_and_write() {
        let dgs = DenyGlobSet::new();
        for path in [
            "home/.rightkit/daemon.port",
            "state/daemon.lock",
            "w/.rightkit/runtime/token",
        ] {
            assert!(dgs.is_read_denied(path), "read {path}");
            assert!(dgs.is_write_denied(path), "write {path}");
        }
    }

    #[test]
    fn glob_matching_dot_env_at_root() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied(".env"));
        assert!(dgs.is_write_denied("config/.env"));
        assert!(dgs.is_write_denied("deep/nested/.env"));
        assert!(!dgs.is_write_denied("environment.txt"));
    }

    #[test]
    fn glob_matching_git_dir() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied(".git/config"));
        assert!(dgs.is_write_denied(".git/hooks/pre-commit"));
        assert!(!dgs.is_write_denied("mygit/config"));
    }

    #[test]
    fn glob_matching_node_modules() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied("a/node_modules/b/c.js"));
        assert!(!dgs.is_write_denied("src/app/main.cs"));
    }

    #[test]
    fn glob_matching_target_dir() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied("engine/target/debug/out"));
    }

    #[test]
    fn glob_read_deny_ssh() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_read_denied(".ssh/id_rsa"));
        assert!(dgs.is_read_denied("home/user/.ssh/known_hosts"));
    }

    #[test]
    fn deny_globs_are_case_insensitive() {
        // On Windows/macOS `.ENV` is the same file as `.env`; a case-sensitive glob
        // would let a model dodge the `**/.env` deny by varying case.
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied(".ENV"));
        assert!(dgs.is_write_denied("config/.Env"));
        assert!(dgs.is_read_denied(".SSH/id_rsa"));
        assert!(dgs.is_read_denied("Home/User/.Ssh/known_hosts"));
        assert_eq!(dgs.first_write_match(".ENV"), Some("**/.env"));
    }

    #[test]
    fn glob_read_deny_env() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_read_denied(".env"));
        assert!(dgs.is_read_denied("config/.env"));
    }

    #[test]
    fn glob_read_deny_secret_pattern() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_read_denied("mysecretfile.txt"));
        assert!(dgs.is_read_denied("credentials.json"));
    }

    #[test]
    fn first_write_match_returns_pattern_name() {
        let dgs = DenyGlobSet::new();
        assert_eq!(dgs.first_write_match(".env"), Some("**/.env"));
        assert_eq!(dgs.first_write_match(".git/config"), Some(".git/**"));
        assert_eq!(dgs.first_write_match("safe.rs"), None);
    }

    #[test]
    fn first_read_match_returns_pattern_name() {
        let dgs = DenyGlobSet::new();
        assert_eq!(dgs.first_read_match(".env"), Some("**/.env"));
        assert_eq!(dgs.first_read_match("safe.rs"), None);
    }

    #[test]
    fn custom_patterns() {
        let dgs = DenyGlobSet::with_patterns(
            vec!["**/*.secret".to_string()],
            vec!["**/*.lock".to_string()],
        )
        .unwrap();
        assert!(dgs.is_read_denied("my.secret"));
        assert!(!dgs.is_read_denied("my.txt"));
        assert!(dgs.is_write_denied("Cargo.lock"));
        assert!(!dgs.is_write_denied("Cargo.toml"));
    }

    #[test]
    fn custom_patterns_do_not_inherit_defaults() {
        let dgs = DenyGlobSet::with_patterns(Vec::<String>::new(), Vec::<String>::new()).unwrap();

        assert!(!dgs.is_read_denied(".env"));
        assert!(!dgs.is_write_denied(".git/config"));
    }

    #[test]
    fn invalid_custom_pattern_reports_the_pattern() {
        let err = DenyGlobSet::with_patterns(vec!["[".to_string()], Vec::<String>::new())
            .expect_err("invalid glob should fail");

        assert!(matches!(
            err,
            DenyGlobError::InvalidGlob { ref pattern, .. } if pattern == "["
        ));
    }

    #[test]
    fn first_match_respects_custom_pattern_order() {
        let dgs = DenyGlobSet::with_patterns(
            Vec::<String>::new(),
            vec!["**/*.rs".to_string(), "src/**".to_string()],
        )
        .unwrap();

        assert_eq!(dgs.first_write_match("src/lib.rs"), Some("**/*.rs"));
    }

    #[test]
    fn read_and_write_lists_are_independent() {
        let dgs = DenyGlobSet::with_patterns(
            vec!["secrets/**".to_string()],
            vec!["build/**".to_string()],
        )
        .unwrap();

        assert!(dgs.is_read_denied("secrets/token.txt"));
        assert!(!dgs.is_write_denied("secrets/token.txt"));
        assert!(dgs.is_write_denied("build/out.txt"));
        assert!(!dgs.is_read_denied("build/out.txt"));
    }

    #[test]
    fn glob_matching_ported_from_csharp_tests() {
        // Mirrors the C# [Theory] in PermissionEngineTests.
        // **/.env matches .env (root)
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied(".env"));
        // **/.env matches deep/nested/.env
        assert!(dgs.is_write_denied("deep/nested/.env"));
        // **/.env does NOT match environment.txt
        assert!(!dgs.is_write_denied("environment.txt"));
        // .git/** matches .git/hooks/pre-commit
        assert!(dgs.is_write_denied(".git/hooks/pre-commit"));
        // **/node_modules/** matches a/node_modules/b/c.js
        assert!(dgs.is_write_denied("a/node_modules/b/c.js"));
    }

    #[test]
    fn glob_read_deny_cloud_and_key_material() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_read_denied("home/user/.aws/config"));
        assert!(dgs.is_read_denied("home/user/.aws/credentials"));
        assert!(dgs.is_read_denied(".config/gcloud/application_default_credentials.json"));
        assert!(dgs.is_read_denied("home/user/.azure/accessTokens.json"));
        assert!(dgs.is_read_denied(".kube/config"));
        assert!(dgs.is_read_denied("home/.docker/config.json"));
        assert!(dgs.is_read_denied(".npmrc"));
        assert!(dgs.is_read_denied("home/.pypirc"));
        assert!(dgs.is_read_denied(".netrc"));
        assert!(dgs.is_read_denied("home/.gnupg/secring.gpg"));
        assert!(dgs.is_read_denied("certs/server.pem"));
        assert!(dgs.is_read_denied("keys/id_ed25519.key"));
        assert!(dgs.is_read_denied("store.p12"));
        assert!(dgs.is_read_denied("keystore.jks"));
        // Non-secret files in the same project still readable.
        assert!(!dgs.is_read_denied("src/main.rs"));
        assert!(!dgs.is_read_denied("README.md"));
    }

    #[test]
    fn glob_write_deny_shell_init_and_persistence() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied("home/user/.bashrc"));
        assert!(dgs.is_write_denied(".zshrc"));
        assert!(dgs.is_write_denied("home/.profile"));
        assert!(dgs.is_write_denied(".bash_profile"));
        assert!(dgs.is_write_denied("home/.gitconfig"));
        assert!(dgs.is_write_denied("home/user/.ssh/authorized_keys"));
        assert!(!dgs.is_write_denied("src/app.rs"));
    }

    #[test]
    fn backslash_paths_normalized() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied(".git\\hooks\\pre-commit"));
        assert!(dgs.is_write_denied("a\\node_modules\\b\\c.js"));
    }

    #[test]
    fn leading_dot_slash_stripped() {
        let dgs = DenyGlobSet::new();
        assert!(dgs.is_write_denied("./.env"));
        assert!(dgs.is_write_denied("./.git/config"));
    }
}
