//! Index configuration.

use std::path::PathBuf;
use std::time::Duration;

/// Extensions treated as packages (bundles shown as one item by Finder).
pub const DEFAULT_PACKAGE_EXTENSIONS: &[&str] = &[
    "app",
    "framework",
    "bundle",
    "plugin",
    "kext",
    "appex",
    "xpc",
    "photoslibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
    "fcpbundle",
    "logicx",
    "band",
    "xcodeproj",
    "xcworkspace",
    "playground",
    "prefpane",
    "qlgenerator",
    "mdimporter",
    "saver",
    "wdgt",
    "rtfd",
    "sparsebundle",
    "dsym",
    "xcarchive",
    "photolibrary",
    "aplibrary",
    "theatre",
    "lrdata",
];

/// Protected stores skipped unless `include_protected` is set. Entries are
/// relative to each home directory.
pub const PROTECTED_HOME_RELATIVE: &[&str] = &[
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Cookies",
    "Library/HomeKit",
    "Library/Suggestions",
    "Library/Calendars",
    "Library/Metadata/CoreSpotlight",
    "Library/Application Support/AddressBook",
    "Library/Application Support/CallHistoryDB",
    "Library/Application Support/com.apple.TCC",
    "Library/Group Containers/group.com.apple.notes",
];

/// Build, cache, and VCS-object directories excluded by default. Each entry is a glob over
/// path components (`*`, `?`); a multi-component entry matches consecutive components
/// anywhere below a root. Excluded directories stay in the index as empty, flagged entries.
/// Set `Config::include_build_output` to index them.
pub const DEFAULT_EXCLUDE_PATTERNS: &[&str] = &[
    "target",
    "node_modules",
    "DerivedData",
    ".build",
    ".cache",
    ".git/objects",
    "Library/Caches",
    // Package-manager, toolchain and build-tool trees that churn during builds.
    "node_modules.*",
    ".venv",
    "__pycache__",
    ".gradle",
    "Pods",
    ".turbo",
    ".next",
    "sccache",
    ".sccache",
    ".rightkit-managed",
    ".git/logs",
    ".git/lfs",
];

/// Absolute scratch roots excluded by default on macOS, matched as path prefixes (not names).
/// Dropped by `Config::include_build_output`.
pub const DEFAULT_EXCLUDE_ROOTS_MACOS: &[&str] = &["/Volumes/D/scratch"];

/// Absolute scratch roots excluded by default on Windows, matched as path prefixes,
/// ASCII case-insensitively. Dropped by `Config::include_build_output`.
pub const DEFAULT_EXCLUDE_ROOTS_WINDOWS: &[&str] = &[r"D:\scratch"];

/// Folders that raise a consent prompt when opened without Full Disk Access.
pub const CONSENT_HOME_RELATIVE: &[&str] = &["Desktop", "Documents", "Downloads"];

#[derive(Clone, Debug)]
pub struct Config {
    /// Roots to index (usually `/`). Missing or unreadable roots are reported in `Status`, not fatal.
    pub roots: Vec<PathBuf>,
    /// Where the snapshot (event-ID resume) and the memory-mapped name segments live. `None`: anonymous maps, no resume.
    pub state_dir: Option<PathBuf>,
    /// Extra directories never indexed (their folder entry exists, empty, flagged excluded).
    pub exclude: Vec<PathBuf>,
    /// Index protected stores such as ~/Library/Mail. Default false.
    pub include_protected: bool,
    /// Allow opening consent-gated folders (Desktop, Documents, Downloads) when Full Disk Access is absent. Default false.
    pub allow_consent_prompts: bool,
    /// Extra package extensions (without dot), added to `DEFAULT_PACKAGE_EXTENSIONS`.
    pub extra_package_extensions: Vec<String>,
    /// Keep the index live with the platform change feed. Default true.
    pub watch: bool,
    /// Extra glob patterns over path components, excluded in addition to the defaults
    /// (`DEFAULT_EXCLUDE_PATTERNS`, unless `include_build_output`). Matching is case-sensitive.
    pub exclude_patterns: Vec<String>,
    /// Index build, cache, and VCS-object directories too: drops `DEFAULT_EXCLUDE_PATTERNS`.
    /// Default false. `exclude_patterns` still apply.
    pub include_build_output: bool,
    /// A folder that keeps changing is re-listed at most once per this interval. The
    /// interval starts at this floor, doubles while changes keep arriving, and returns to the
    /// floor once a folder is quiet. Zero disables the gate. Default 5 s.
    pub relist_min_interval: Duration,
    /// Upper bound for the re-list interval while a folder keeps changing. Default 60 s.
    pub relist_max_interval: Duration,
    /// Cooldown after a full rescan triggered by dropped events. Further drops only mark a
    /// rescan wanted; it runs when the cooldown ends. The cooldown doubles on each rescan up to
    /// 10 min and resets after 30 quiet minutes. Zero disables the cooldown (every drop
    /// rescans at once, as before 0.3.0). Default 30 s.
    pub rescan_cooldown: Duration,
    /// Global budget for subtree re-lists (must-scan, root-changed, mount events) across all
    /// folders, as `(burst, refill interval)`: up to `burst` at once, then one more per
    /// interval. The excess waits in a pending set and drains from a timer; nothing is dropped.
    /// `None`, a zero burst or a zero interval disables the budget. Default `Some((8, 5 s))`.
    /// `Status::relists_pending` shows what is waiting.
    pub relist_budget: Option<(u32, Duration)>,
    /// Home directories used to place protected and consent-gated exclusions. `None`: $HOME and /Users/*.
    pub home_dirs: Option<Vec<PathBuf>>,
    /// FSEvents delivery latency. Default 500 ms.
    pub watch_latency: Duration,
    /// Subscribers get at most one batch per this window. Default 1 s.
    pub change_coalesce: Duration,
    /// Max changed paths per batch (the rest is counted, not listed).
    pub max_change_paths: usize,
    /// Max rollup updates per batch.
    pub max_change_rollups: usize,
    /// Crawl worker threads. Clamped to 1..=8 at use, and the threads are drawn from one
    /// process-wide budget of 8, so concurrent crawls queue instead of multiplying threads.
    pub crawl_threads: usize,
    /// A snapshot older than this is discarded for a full crawl.
    pub snapshot_max_age: Duration,
    #[cfg(feature = "content")]
    pub content: ContentConfig,
}

impl Config {
    pub fn new(roots: Vec<PathBuf>) -> Config {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        Config {
            roots,
            state_dir: None,
            exclude: Vec::new(),
            include_protected: false,
            allow_consent_prompts: false,
            extra_package_extensions: Vec::new(),
            watch: true,
            exclude_patterns: Vec::new(),
            include_build_output: false,
            relist_min_interval: Duration::from_secs(5),
            relist_max_interval: Duration::from_secs(60),
            rescan_cooldown: Duration::from_secs(30),
            relist_budget: Some((8, Duration::from_secs(5))),
            home_dirs: None,
            watch_latency: Duration::from_millis(500),
            change_coalesce: Duration::from_secs(1),
            max_change_paths: 1024,
            max_change_rollups: 2048,
            crawl_threads: cpus.clamp(2, 8),
            snapshot_max_age: Duration::from_secs(3 * 24 * 3600),
            #[cfg(feature = "content")]
            content: ContentConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Excludes;

    fn defaults() -> Excludes {
        Excludes {
            patterns: DEFAULT_EXCLUDE_PATTERNS
                .iter()
                .map(|p| {
                    p.split('/')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.as_bytes().to_vec())
                        .collect()
                })
                .collect(),
            roots: vec![b"/work".to_vec()],
            ..Excludes::default()
        }
    }

    #[test]
    fn the_new_default_excludes_match() {
        let ex = defaults();
        for p in [
            "node_modules",
            "node_modules.medium-20261009",
            "pkg/node_modules",
            "pkg/node_modules/a/b",
            ".venv",
            "svc/__pycache__",
            ".gradle",
            "ios/Pods",
            "web/.turbo",
            "web/.next",
            "sccache",
            ".sccache",
            ".rightkit-managed",
            "repo/.git/objects",
            "repo/.git/logs",
            "repo/.git/lfs",
        ] {
            let full = format!("/work/{p}");
            assert!(ex.matches(full.as_bytes()), "{p} should be excluded");
        }
    }

    #[test]
    fn generic_names_and_the_rest_of_git_stay_indexed() {
        let ex = defaults();
        for p in [
            "scratch",
            "tmp",
            "dist",
            "build",
            "repo/.git",
            "repo/.git/config",
            "repo/.git/refs",
            "node_modulesX",
            "my_node_modules",
            "src",
        ] {
            let full = format!("/work/{p}");
            assert!(!ex.matches(full.as_bytes()), "{p} should be indexed");
        }
    }
}

/// Content search settings. Everything is off until `enabled` is true.
#[cfg(feature = "content")]
#[derive(Clone, Debug)]
pub struct ContentConfig {
    pub enabled: bool,
    /// Only files under these roots are read. Empty means nothing is read.
    pub allowlist: Vec<PathBuf>,
    /// Files larger than this are skipped.
    pub max_file_size: u64,
    /// Extensions (lowercase, no dot) eligible as text.
    pub text_extensions: Vec<String>,
    /// Directories never read (in addition to `Config::exclude`).
    pub exclude: Vec<PathBuf>,
}

#[cfg(feature = "content")]
impl Default for ContentConfig {
    fn default() -> Self {
        ContentConfig {
            enabled: false,
            allowlist: Vec::new(),
            max_file_size: 1 << 20,
            text_extensions: [
                "txt", "md", "rs", "toml", "json", "yaml", "yml", "swift", "py", "js", "ts", "c",
                "h", "cpp", "html", "css", "sh", "csv", "log",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            exclude: Vec::new(),
        }
    }
}
