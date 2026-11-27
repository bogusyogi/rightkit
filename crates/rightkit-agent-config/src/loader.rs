//! Config loading: TOML parsing, layered merging, and env overrides.

use thiserror::Error;

use crate::merge::merge;
use crate::types::HostConfig;
use std::collections::HashMap;

static HARDENED_LOADER: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Select the hardened config loader for this process (once per process; returns `false` if it
/// was already selected). Until it is called every loader entry point behaves as it always did.
/// After it:
///
/// - [`ConfigLoader::load_files_for_workspace`] (and so [`ConfigLoader::load_files_default`])
///   reads layers through [`ConfigLoader::load_files_for_workspace_granted`] with no grant: a layer
///   is read only from a regular file of at most 1 MiB, a workspace layer that is a link is
///   refused, a parse error names a line and column and never the file's text, an invalid user
///   layer is moved aside with its bytes kept, an invalid workspace layer is left in place and
///   ignored, a linked git worktree uses its project's config, an untrusted workspace keeps only
///   its `[prompt]` and cosmetic `[tui]` values, explicitly written scalars stand even when they
///   equal the default, and the [`cli_flags`](crate::cli_flags) overrides apply;
/// - [`ConfigLoader::update_user`] starts from the user file alone ([`ConfigLoader::load_user_file`]),
///   never the merged stack, so a workspace layer is never copied into the user-global file;
/// - [`ConfigSource::effective_for_workspace`](crate::ConfigSource::effective_for_workspace) reads
///   its layers the same way.
///
/// [`ConfigLoader::load_files_for_workspace_granted`] and [`ConfigLoader::load_user_file`] are
/// hardened whether or not this was called. A host that selects this should also select
/// [`set_nearest_ancestor_trust`](crate::set_nearest_ancestor_trust) so the trust decision for the
/// workspace layer uses the nearest-ancestor rules.
pub fn set_hardened_config_loader() -> bool {
    HARDENED_LOADER.set(()).is_ok()
}

pub(crate) fn hardened_loader_selected() -> bool {
    HARDENED_LOADER.get().is_some()
}

/// The text of the error for a state root that does not resolve: the host app name is the one
/// [`crate::app_name`] gives (`cannot determine the Agent state directory` for the default).
fn state_dir_unavailable() -> String {
    format!("cannot determine the {} state directory", crate::app_name())
}

/// The folder whose `<state-dir>/config.toml` applies to `workspace`: the workspace itself, or,
/// when the workspace is a linked git worktree with no config file of its own, the project the
/// worktree belongs to. Only the small `.git` pointer file is read; nothing is followed that
/// does not name a `.git/worktrees/<name>` folder.
pub fn project_root_for_config(workspace: &std::path::Path) -> std::path::PathBuf {
    if workspace
        .join(crate::state_dir_name())
        .join("config.toml")
        .exists()
    {
        return workspace.to_path_buf();
    }
    let pointer = workspace.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&pointer) else {
        return workspace.to_path_buf();
    };
    if !meta.is_file() || meta.len() > 4096 {
        return workspace.to_path_buf();
    }
    let Ok(text) = std::fs::read_to_string(&pointer) else {
        return workspace.to_path_buf();
    };
    let Some(gitdir) = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("gitdir:"))
    else {
        return workspace.to_path_buf();
    };
    // `<project>/.git/worktrees/<name>`
    let gitdir = std::path::Path::new(gitdir.trim());
    let project = gitdir
        .parent()
        .filter(|worktrees| worktrees.file_name().is_some_and(|n| n == "worktrees"))
        .and_then(|worktrees| worktrees.parent())
        .filter(|dot_git| dot_git.file_name().is_some_and(|n| n == ".git"))
        .and_then(|dot_git| dot_git.parent());
    match project {
        Some(project)
            if project
                .join(crate::state_dir_name())
                .join("config.toml")
                .exists() =>
        {
            project.to_path_buf()
        }
        _ => workspace.to_path_buf(),
    }
}

/// Errors that can occur while loading configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse TOML: {0}")]
    Toml(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Smallest accepted `context-window`, in tokens. There is no upper bound: the window is the
/// user's statement about their model.
const MIN_CONTEXT_WINDOW: u64 = 1024;

/// Clear out-of-range `context-window` / `compaction-trigger-percent` values in
/// `[model-limits.*]` and return one message per cleared value, ordered by key.
fn sanitize_limits(cfg: &mut HostConfig) -> Vec<String> {
    let mut keys: Vec<String> = cfg.model_limits.keys().cloned().collect();
    keys.sort();
    let mut messages = Vec::new();
    for key in keys {
        let Some(limits) = cfg.model_limits.get_mut(&key) else {
            continue;
        };
        if let Some(w) = limits.context_window.filter(|w| *w < MIN_CONTEXT_WINDOW) {
            limits.context_window = None;
            messages.push(format!(
                "model-limits.{key}.context-window must be at least {MIN_CONTEXT_WINDOW} tokens (got {w})"
            ));
        }
        if let Some(p) = limits
            .compaction_trigger_percent
            .filter(|p| !(1..=95).contains(p))
        {
            limits.compaction_trigger_percent = None;
            messages.push(format!(
                "model-limits.{key}.compaction-trigger-percent must be between 1 and 95 (got {p})"
            ));
        }
    }
    messages
}

/// The digits of a `context-window = <integer>` line whose integer does not fit `i64`, the most a TOML
/// document can hold. `None` for every other line.
fn oversized_context_window(line: &str) -> Option<&str> {
    let rest = line
        .trim()
        .strip_prefix("context-window")?
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
        .unwrap_or(rest.len());
    let (number, tail) = rest.split_at(end);
    let tail = tail.trim();
    if !number.starts_with(|c: char| c.is_ascii_digit())
        || !(tail.is_empty() || tail.starts_with('#'))
    {
        return None;
    }
    number
        .replace('_', "")
        .parse::<i64>()
        .is_err()
        .then_some(number)
}

/// The TOML lexer rejects an integer above `i64::MAX` before serde runs, which would fail the whole
/// layer and move the file aside. Blank each such `context-window` line (the newline stays, so error
/// positions are unchanged) and warn once, exactly as [`sanitize_limits`] does for a small value.
fn drop_oversized_context_windows<'a>(
    path: &std::path::Path,
    content: &'a str,
) -> std::borrow::Cow<'a, str> {
    if !content.contains("context-window") {
        return std::borrow::Cow::Borrowed(content);
    }
    let mut cleaned = String::with_capacity(content.len());
    let mut changed = false;
    for line in content.split_inclusive('\n') {
        if let Some(number) = oversized_context_window(line) {
            warn_once(format!(
                "warning: config {}: context-window {number} is too large (at most {} fits); value ignored",
                path.display(),
                i64::MAX
            ));
            changed = true;
            cleaned.push_str(
                line.strip_prefix(line.trim_end_matches(['\r', '\n']))
                    .unwrap_or(""),
            );
        } else {
            cleaned.push_str(line);
        }
    }
    if changed {
        std::borrow::Cow::Owned(cleaned)
    } else {
        std::borrow::Cow::Borrowed(content)
    }
}

/// Print a config warning at most once per process: layers are re-read on every
/// `ConfigLoader::load`, and a repeated line would flood the terminal.
fn warn_once(line: String) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let mut warned = WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if warned.insert(line.clone()) {
        eprintln!("{line}");
    }
}

/// One config layer that failed to parse and was ignored, kept for `/status` and `doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredLayer {
    /// `"user"` or `"workspace"`.
    pub layer: &'static str,
    pub path: std::path::PathBuf,
    /// First line of the parse error (a position only for a workspace layer).
    pub error: String,
    /// Where an invalid user file was moved aside to, when it was.
    pub moved_to: Option<std::path::PathBuf>,
}

/// Process-wide record of ignored layers. A user file is moved aside on its first failed read, so
/// later loads see no file; the record outlives that. A workspace entry is dropped when its file
/// parses again.
fn ignored_layers_cell() -> &'static std::sync::Mutex<Vec<IgnoredLayer>> {
    static IGNORED: std::sync::OnceLock<std::sync::Mutex<Vec<IgnoredLayer>>> =
        std::sync::OnceLock::new();
    IGNORED.get_or_init(Default::default)
}

fn record_ignored_layer(entry: IgnoredLayer) {
    let mut list = ignored_layers_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    list.retain(|e| !(e.layer == entry.layer && e.path == entry.path));
    list.push(entry);
}

/// A layer file's keys that no setting reads (a typo such as `[permisions]` or `max-turn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKeys {
    /// `"user"` or `"workspace"`.
    pub layer: &'static str,
    pub path: std::path::PathBuf,
    /// Dotted key paths, sorted (`permisions`, `session.max-turn`).
    pub keys: Vec<String>,
}

fn unknown_keys_cell() -> &'static std::sync::Mutex<Vec<UnknownKeys>> {
    static UNKNOWN: std::sync::OnceLock<std::sync::Mutex<Vec<UnknownKeys>>> =
        std::sync::OnceLock::new();
    UNKNOWN.get_or_init(Default::default)
}

fn record_unknown_keys(layer: &'static str, path: &std::path::Path, keys: Vec<String>) {
    let mut list = unknown_keys_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    list.retain(|e| !(e.layer == layer && e.path == path));
    if !keys.is_empty() {
        list.push(UnknownKeys {
            layer,
            path: path.to_path_buf(),
            keys,
        });
    }
}

/// Key paths in `content` that the parsed `cfg` has no field for. `cfg` serializes every field
/// except an unset `Option` and a key the host retired ([`crate::set_retired_config_keys`]), and a
/// key the file set is never unset, so a raw key missing from the serialized form is one nothing
/// reads: a retired key is reported like a misspelt one. Free-form tables (provider routes, hooks, keymaps)
/// copy their own keys through, so they never show up here.
fn find_unknown_keys(content: &str, cfg: &HostConfig) -> Vec<String> {
    fn walk(raw: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
        for (key, value) in raw {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match known.get(key) {
                // `schema-version` is written by older installs and read by nothing.
                None if prefix.is_empty() && key == "schema-version" => {}
                None => out.push(path),
                Some(toml::Value::Table(known_table)) => {
                    if let toml::Value::Table(raw_table) = value {
                        walk(raw_table, known_table, &path, out);
                    }
                }
                Some(_) => {}
            }
        }
    }
    let (Ok(raw), Ok(known)) = (content.parse::<toml::Table>(), toml::Table::try_from(cfg)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&raw, &known, "", &mut out);
    out.sort();
    out
}

/// Merge the `-c` layer (TOML text of the given pairs) over `merged`. Its scalars win even when they
/// equal the type default, and a key no setting reads is recorded as unknown for layer `cli`.
fn merge_override_layer(merged: HostConfig, text: &str) -> HostConfig {
    let Ok(layer) = toml::from_str::<HostConfig>(text) else {
        return merged;
    };
    record_unknown_keys(
        "cli",
        std::path::Path::new("-c"),
        find_unknown_keys(text, &layer),
    );
    let keys = explicit_restatable_keys(text);
    let mut merged = merge(merged, layer.clone());
    restate(&mut merged, &layer, &keys);
    merged
}

/// The scalar settings a later layer may restate at their default value (`temperature = 0.0`,
/// `theme = ""`). They are plain values with a default, so a merge cannot tell "set to the
/// default" from "not set"; the raw file can.
const RESTATABLE: [(&str, &str); 10] = [
    ("model", "default-model"),
    ("model", "max-tokens"),
    ("model", "temperature"),
    ("session", "effort"),
    ("permissions", "mode"),
    ("tui", "theme"),
    ("tui", "notify"),
    ("orchestration", "boss-model"),
    ("orchestration", "role-model-mode"),
    ("compaction", "builtin-summarizer-routes"),
];

fn explicit_restatable_keys(content: &str) -> Vec<(&'static str, &'static str)> {
    let Ok(raw) = content.parse::<toml::Table>() else {
        return Vec::new();
    };
    RESTATABLE
        .iter()
        .copied()
        .filter(|(table, key)| {
            raw.get(*table)
                .and_then(toml::Value::as_table)
                .is_some_and(|t| t.contains_key(*key))
        })
        .collect()
}

/// Copy the settings `layer` named explicitly onto `merged` (see [`RESTATABLE`]).
fn restate(merged: &mut HostConfig, layer: &HostConfig, keys: &[(&str, &str)]) {
    for (table, key) in keys {
        match (*table, *key) {
            ("model", "default-model") => {
                merged.model.default_model = layer.model.default_model.clone()
            }
            ("model", "max-tokens") => merged.model.max_tokens = layer.model.max_tokens,
            ("model", "temperature") => merged.model.temperature = layer.model.temperature,
            ("session", "effort") => merged.session.effort = layer.session.effort.clone(),
            ("permissions", "mode") => merged.permissions.mode = layer.permissions.mode.clone(),
            ("tui", "theme") => merged.tui.theme = layer.tui.theme.clone(),
            ("tui", "notify") => merged.tui.notify = layer.tui.notify.clone(),
            ("orchestration", "boss-model") => {
                merged.orchestration.boss_model = layer.orchestration.boss_model.clone()
            }
            ("orchestration", "role-model-mode") => {
                merged.orchestration.role_model_mode = layer.orchestration.role_model_mode.clone()
            }
            ("compaction", "builtin-summarizer-routes") => {
                merged.compaction.builtin_summarizer_routes =
                    layer.compaction.builtin_summarizer_routes
            }
            _ => {}
        }
    }
}

fn remove_workspace_compaction_consent(
    cfg: &mut HostConfig,
    keys: &mut Vec<(&'static str, &'static str)>,
) {
    cfg.compaction = crate::types::CompactionConfig::default();
    keys.retain(|(table, _)| *table != "compaction");
}

fn clear_ignored_workspace_layer(path: &std::path::Path) {
    ignored_layers_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|e| !(e.layer == "workspace" && e.path == path));
}

/// Loads and combines configuration from multiple layers.
#[derive(Debug, Default, Clone)]
pub struct ConfigLoader;

impl ConfigLoader {
    /// Layers this process found invalid and ignored (user and workspace config), oldest first.
    pub fn ignored_layers() -> Vec<IgnoredLayer> {
        ignored_layers_cell()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Keys this process found in a user or workspace config file that no setting reads.
    pub fn unknown_keys() -> Vec<UnknownKeys> {
        unknown_keys_cell()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Create a new loader.
    pub fn new() -> Self {
        ConfigLoader
    }

    /// Parse a single TOML string into a [`HostConfig`]. Out-of-range `[model-limits]`
    /// values are dropped exactly as `Self::parse_layer` drops them, so a value that the boot
    /// path ignores never makes a settings save fail.
    pub fn load_str(&self, toml_str: &str) -> Result<HostConfig, ConfigError> {
        let mut cfg: HostConfig =
            toml::from_str(toml_str).map_err(|e| ConfigError::Toml(e.to_string()))?;
        sanitize_limits(&mut cfg);
        Ok(cfg)
    }

    /// Fold [`merge`] over all layers, starting from defaults.
    /// Earlier layers are overridden by later ones (lists accumulate).
    pub fn load_layered(&self, layers: Vec<HostConfig>) -> HostConfig {
        layers.into_iter().fold(HostConfig::default(), merge)
    }

    /// Load the standard two-file stack:
    ///   1. `<state root>/config.toml` (user-global)
    ///   2. `<state-dir>/config.toml` (workspace, relative to cwd)
    ///
    /// Missing files are silently skipped. Returns merged config.
    pub fn load_files_default(&self) -> HostConfig {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        self.load_files_for_workspace(&cwd)
    }

    /// Load the standard user + explicit workspace stack:
    ///   1. `<state root>/config.toml` (user-global)
    ///   2. `<workspace>/<state-dir>/config.toml`
    ///
    /// Missing files are skipped. An invalid file is moved aside with its bytes kept and
    /// reported on stderr (see `Self::load_layer_file`); it never silently falls back.
    ///
    /// After [`set_hardened_config_loader`] this is [`Self::load_files_for_workspace_granted`]
    /// with no grant, which reads and reduces the layers as that method documents.
    pub fn load_files_for_workspace(&self, workspace: &std::path::Path) -> HostConfig {
        if hardened_loader_selected() {
            return self.load_files_for_workspace_granted(workspace, false);
        }
        let mut layers = vec![];

        // User-global config.
        if let Some(root) = crate::state_root() {
            if let Some(cfg) = self.load_layer_file(&root.join("config.toml")) {
                layers.push(cfg);
            }
        }

        // Workspace config (<workspace>/<state-dir>/config.toml).
        let ws_path = workspace.join(crate::state_dir_name()).join("config.toml");
        if let Some(mut cfg) = self.load_layer_file(&ws_path) {
            // 0.1.4 did not know these keys, so a workspace file could not set them. They name
            // programs and text the host runs or submits (hooks, the statusline command, quick
            // slots, the compaction model), so this loader, which does not check workspace trust,
            // still takes none of them from a workspace file. The hardened loader reads them from
            // a trusted workspace.
            cfg.hooks = crate::types::HooksConfig::default();
            cfg.compaction = crate::types::CompactionConfig::default();
            cfg.tui.quick_slots.clear();
            cfg.tui.statusline_command.clear();
            layers.push(cfg);
        }

        self.load_layered(layers)
    }

    /// [`Self::load_files_for_workspace`] for one session that carries a per-session trust grant
    /// (`POST /sessions` `trustWorkspace`): the workspace layer is used whole when `granted` is
    /// set, or when the store/process trusts the workspace. The grant is never persisted.
    pub fn load_files_for_workspace_granted(
        &self,
        workspace: &std::path::Path,
        granted: bool,
    ) -> HostConfig {
        let mut layers = vec![];
        let mut restated = vec![];

        // User-global config (`--ignore-user-config` leaves it unread).
        if let Some(root) = crate::state_root().filter(|_| !crate::cli_flags::ignore_user_config())
        {
            if let Some((cfg, keys)) =
                self.load_layer_file_keyed(&root.join("config.toml"), LayerKind::User)
            {
                restated.push(keys);
                layers.push(cfg);
            }
        }

        // Workspace config (<workspace>/<state-dir>/config.toml). It is read before the trust
        // decision, so the read is bounded, regular-file only and never follows a link, and a
        // parse error reports a position only, never the file's text.
        // A linked git worktree of a project (a sub-agent's own checkout) has no config of its
        // own, because the file is the person's and is not committed. It uses the project's.
        let project = project_root_for_config(workspace);
        let workspace = project.as_path();
        let ws_path = workspace.join(crate::state_dir_name()).join("config.toml");
        if let Some((cfg, mut keys)) = self.load_layer_file_keyed(&ws_path, LayerKind::Workspace) {
            let mut cfg = if granted || crate::is_workspace_trusted(workspace) {
                cfg
            } else {
                // Only the cosmetic `[tui]` values survive an untrusted workspace.
                keys.retain(|(table, _)| *table == "tui");
                untrusted_workspace_layer(cfg)
            };
            // Sending the transcript to a third-party summarizer is the person's own consent:
            // it comes from the user file or `-c`, never from a file inside a repository.
            remove_workspace_compaction_consent(&mut cfg, &mut keys);
            restated.push(keys);
            layers.push(cfg);
        }

        // Fold the layers, then let each layer's explicitly written scalars stand even when they
        // equal the type default (a later `temperature = 0.0` beats an earlier 0.7).
        let mut merged = HostConfig::default();
        for (layer, keys) in layers.into_iter().zip(restated) {
            merged = merge(merged, layer.clone());
            restate(&mut merged, &layer, &keys);
        }
        // `-c key=value` is the top layer: it beats the user and workspace files.
        if let Some(text) = crate::cli_flags::overrides_layer_text() {
            merged = merge_override_layer(merged, &text);
        }
        merged
    }

    /// The user-global `<state root>/config.toml` alone, with no workspace layer merged in: the
    /// starting point of every user-file save ([`Self::update_user`]). A missing file is the
    /// default config. An unreadable file is an error, so a save never replaces a file it could
    /// not read. An invalid file is moved aside with its bytes kept (as [`Self::load_files_default`]
    /// does) and the save starts from defaults.
    pub fn load_user_file(&self) -> Result<HostConfig, ConfigError> {
        let root = crate::state_root().ok_or_else(|| ConfigError::Io(state_dir_unavailable()))?;
        self.load_user_file_at(&root.join("config.toml"))
    }

    fn load_user_file_at(&self, path: &std::path::Path) -> Result<HostConfig, ConfigError> {
        let content = match read_layer_bytes(path, LayerKind::User) {
            Ok(Some(content)) => content,
            Ok(None) => return Ok(HostConfig::default()),
            Err(error) => {
                return Err(ConfigError::Io(format!(
                    "config {} is unreadable: {error}",
                    path.display()
                )))
            }
        };
        Ok(self
            .parse_layer(path, &content, LayerKind::User)
            .unwrap_or_default())
    }

    /// One layer file. A missing file contributes nothing. A file that cannot be read or parsed is renamed aside with its original bytes
    /// kept (`<name>.corrupt-<unix secs>`), so the user's text is never overwritten and
    /// defaults apply, with a notice on stderr. Renaming fails closed: if the file cannot be
    /// moved, it is kept where it is and the notice says so.
    fn load_layer_file(&self, path: &std::path::Path) -> Option<HostConfig> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                eprintln!(
                    "warning: config {} is unreadable ({error}); defaults apply for this layer",
                    path.display()
                );
                return None;
            }
        };
        match self.load_str(&content) {
            Ok(cfg) => Some(cfg),
            Err(error) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let aside = path.with_file_name(format!(
                    "{}.corrupt-{stamp}",
                    path.file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default()
                ));
                match std::fs::rename(path, &aside) {
                    Ok(()) => eprintln!(
                        "warning: config {} is invalid ({error}); moved aside to {} and defaults apply",
                        path.display(),
                        aside.display()
                    ),
                    Err(rename_error) => eprintln!(
                        "warning: config {} is invalid ({error}) and could not be moved aside ({rename_error}); it was left in place and defaults apply",
                        path.display()
                    ),
                }
                None
            }
        }
    }

    /// [`Self::load_layer_file`], with the restatable scalars the file wrote (see [`RESTATABLE`]).
    fn load_layer_file_keyed(
        &self,
        path: &std::path::Path,
        kind: LayerKind,
    ) -> Option<(HostConfig, Vec<(&'static str, &'static str)>)> {
        let content = match read_layer_bytes(path, kind) {
            Ok(Some(content)) => content,
            Ok(None) => return None,
            Err(error) => {
                eprintln!(
                    "warning: config {} is unreadable ({error}); defaults apply for this layer",
                    path.display()
                );
                return None;
            }
        };
        self.parse_layer_keyed(path, &content, kind)
    }

    /// Parse a layer's text. On a parse failure the file is moved aside (see
    /// [`Self::load_layer_file`]). An out-of-range `[model-limits]` value is dropped with a
    /// warning on stderr; the rest of the layer is kept and the file is left untouched.
    /// A workspace layer's error names a line and column only: its text may come from anywhere.
    fn parse_layer(
        &self,
        path: &std::path::Path,
        content: &str,
        kind: LayerKind,
    ) -> Option<HostConfig> {
        self.parse_layer_keyed(path, content, kind)
            .map(|(cfg, _)| cfg)
    }

    fn parse_layer_keyed(
        &self,
        path: &std::path::Path,
        content: &str,
        kind: LayerKind,
    ) -> Option<(HostConfig, Vec<(&'static str, &'static str)>)> {
        let cleaned = drop_oversized_context_windows(path, content);
        let content = cleaned.as_ref();
        let error = match toml::from_str::<HostConfig>(content) {
            Ok(mut cfg) => {
                // Before sanitizing: a cleared `context-window` is still a key the file set.
                record_unknown_keys(
                    match kind {
                        LayerKind::User => "user",
                        LayerKind::Workspace => "workspace",
                    },
                    path,
                    find_unknown_keys(content, &cfg),
                );
                let explicit = explicit_restatable_keys(content);
                for message in sanitize_limits(&mut cfg) {
                    warn_once(format!(
                        "warning: config {}: {message}; value ignored",
                        path.display()
                    ));
                }
                if kind == LayerKind::Workspace {
                    clear_ignored_workspace_layer(path);
                }
                return Some((cfg, explicit));
            }
            Err(error) => match kind {
                LayerKind::User => error.to_string(),
                LayerKind::Workspace => parse_error_position(content, error.span()),
            },
        };
        // Recorded so `/status` and `doctor` can say the layer is not in effect.
        let first_line = error
            .lines()
            .next()
            .unwrap_or("invalid TOML")
            .trim()
            .to_string();
        if kind == LayerKind::Workspace {
            record_ignored_layer(IgnoredLayer {
                layer: "workspace",
                path: path.to_path_buf(),
                error: first_line,
                moved_to: None,
            });
            // The file lives in the user's repository: never move it, only report and ignore it.
            warn_once(format!(
                "warning: config {} is invalid ({error}); the file was left in place and is ignored, so its permission rules and deny globs are NOT in effect",
                path.display()
            ));
            return None;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let aside = path.with_file_name(format!(
            "{}.corrupt-{stamp}",
            path.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ));
        let renamed = std::fs::rename(path, &aside);
        record_ignored_layer(IgnoredLayer {
            layer: "user",
            path: path.to_path_buf(),
            error: first_line,
            moved_to: renamed.is_ok().then(|| aside.clone()),
        });
        match renamed {
            Ok(()) => eprintln!(
                "warning: config {} is invalid ({error}); moved aside to {} and defaults apply. Permission rules and deny globs from that file are NOT in effect",
                path.display(),
                aside.display()
            ),
            Err(rename_error) => eprintln!(
                "warning: config {} is invalid ({error}) and could not be moved aside ({rename_error}); it was left in place and defaults apply. Permission rules and deny globs from that file are NOT in effect",
                path.display()
            ),
        }
        None
    }

    /// Apply environment-variable overrides as scalar overrides.
    ///
    /// The env map is passed in (rather than read from the process) so the
    /// function is fully testable. Recognized keys:
    /// `RIGHTKIT_MODEL`, `RIGHTKIT_PERMISSION_MODE`,
    /// `RIGHTKIT_WEB_SEARCH_PROVIDER`, `RIGHTKIT_WEB_SEARCH_API_KEY`,
    /// and `RIGHTKIT_TELEGRAM_TOKEN` (the `RIGHTKIT` prefix is [`crate::env_prefix`]).
    pub fn apply_env(
        &self,
        mut config: HostConfig,
        env_vars: &HashMap<String, String>,
    ) -> HostConfig {
        if let Some(v) = env_vars.get(&crate::env_name("MODEL")) {
            if !v.is_empty() {
                config.model.default_model = v.clone();
            }
        }
        if let Some(v) = env_vars.get(&crate::env_name("PERMISSION_MODE")) {
            if !v.is_empty() {
                config.permissions.mode = v.clone();
            }
        }
        if let Some(v) = env_vars.get(&crate::env_name("WEB_SEARCH_PROVIDER")) {
            if !v.is_empty() {
                config.tools.web_search_provider = Some(v.clone());
            }
        }
        if let Some(v) = env_vars.get(&crate::env_name("WEB_SEARCH_API_KEY")) {
            if !v.is_empty() {
                config.tools.web_search_api_key = Some(v.clone());
            }
        }
        if let Some(v) = env_vars.get(&crate::env_name("TELEGRAM_TOKEN")) {
            if !v.is_empty() {
                config.remote.telegram_token = Some(v.clone());
            }
        }
        if let Some(v) = env_vars.get(&crate::env_name("CLI_ACQUISITION_V1")) {
            if let Some(enabled) = parse_bool(v) {
                config.features.cli_acquisition_v1 = Some(enabled);
            }
        }
        config
    }

    /// Persist a config to the user-global `<state root>/config.toml`.
    /// Creates the directory if it does not exist.
    pub fn save_user(&self, config: &HostConfig) -> Result<(), ConfigError> {
        let dir = crate::state_root().ok_or_else(|| ConfigError::Io(state_dir_unavailable()))?;
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let path = dir.join("config.toml");
        let content =
            toml::to_string_pretty(config).map_err(|e| ConfigError::Toml(e.to_string()))?;
        rightkit_app_settings::atomic_write(&path, content.as_bytes())
            .map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(())
    }

    /// Read-modify-write of the user-global `<state root>/config.toml` under a cross-process lock
    /// (`config.toml.lock` beside it, [`rightkit_fs::FileLock`], waiting up to five seconds), so two
    /// processes saving settings at once cannot lose each other's update. The edit starts from the
    /// merged stack [`Self::load_files_default`] returns and the result is saved with
    /// [`Self::save_user`]. Returns the saved config. After [`set_hardened_config_loader`] the edit
    /// starts from the user file alone ([`Self::load_user_file`]) instead: a workspace layer (whatever
    /// the cwd holds) is never copied into the user-global file.
    pub fn update_user(
        &self,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        self.update_user_from(
            |loader| {
                if hardened_loader_selected() {
                    loader.load_user_file()
                } else {
                    Ok(loader.load_files_default())
                }
            },
            edit,
        )
    }

    /// [`Self::update_user`] with an explicit starting point read under the same lock (for callers
    /// that edit the user file alone rather than the merged stack). Every writer of the user
    /// config goes through one of the two, so no save can be lost to a concurrent one.
    ///
    /// The wait can last up to five seconds and blocks the calling thread: call it from a blocking
    /// context (`spawn_blocking`), never directly on an async worker.
    pub fn update_user_from(
        &self,
        base: impl FnOnce(&Self) -> Result<HostConfig, ConfigError>,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        let dir = crate::state_root().ok_or_else(|| ConfigError::Io(state_dir_unavailable()))?;
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let _lock = lock_config(&dir, CONFIG_LOCK_WAIT)?;
        let mut config = base(self)?;
        edit(&mut config);
        self.save_user(&config)?;
        Ok(config)
    }

    /// Read-modify-write of `<workspace>/<state-dir>/config.toml` under its own cross-process lock
    /// (`config.toml.lock` beside it, the same lock kind and wait as [`Self::update_user_from`]).
    /// `base` reads the starting config under the lock. Every writer of the workspace config goes
    /// through this, so no save can be lost to a concurrent one.
    ///
    /// The wait can last up to five seconds and blocks the calling thread: call it from a blocking
    /// context (`spawn_blocking`), never directly on an async worker.
    pub fn update_workspace_from(
        &self,
        workspace: &std::path::Path,
        base: impl FnOnce() -> Result<HostConfig, ConfigError>,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        self.update_workspace_from_waiting(workspace, CONFIG_LOCK_WAIT, base, edit)
    }

    fn update_workspace_from_waiting(
        &self,
        workspace: &std::path::Path,
        wait: std::time::Duration,
        base: impl FnOnce() -> Result<HostConfig, ConfigError>,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        let dir = workspace.join(crate::state_dir_name());
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let _lock = lock_config(&dir, wait)?;
        let mut config = base()?;
        edit(&mut config);
        self.save_workspace(workspace, &config)?;
        Ok(config)
    }

    /// Persist a config to `<workspace>/<state-dir>/config.toml`.
    /// Creates the directory if it does not exist. A read-modify-write goes through
    /// [`Self::update_workspace_from`], which holds the file's lock; this alone takes none.
    pub fn save_workspace(
        &self,
        workspace: &std::path::Path,
        config: &HostConfig,
    ) -> Result<(), ConfigError> {
        let dir = workspace.join(crate::state_dir_name());
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let path = dir.join("config.toml");
        let content =
            toml::to_string_pretty(config).map_err(|e| ConfigError::Toml(e.to_string()))?;
        rightkit_app_settings::atomic_write(&path, content.as_bytes())
            .map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(())
    }
}

/// What an untrusted workspace's `<state-dir>/config.toml` may still contribute (rules 13 and 58).
///
/// Allowlist, so a key added later is withheld until someone decides it is safe. Only `[prompt]`
/// (the brief-output default, prompt shaping only) and `[tui]` (theme, notify, statusline
/// and other cosmetic terminal preferences) stay: neither runs a command, grants a tool, changes a
/// trust root or moves model traffic. Everything else is withheld because it does one of those:
/// `[hooks]` and `orchestration.pre-gate-checks` run commands; `[permissions]`, `[session]`,
/// `[toolsets.*]` and `[channel-profiles.*]` grant tool authority (a workspace toolset or profile
/// would replace a user or built-in one of the same id); `[model]`, `[provider-routes.*]` and the
/// rest of `[orchestration]` (boss model, role model mode, agents, routing, budget) choose models,
/// endpoints and spend; `[tools]` picks the web-search provider and key; `[features]` holds the
/// opt-in flags that remain (`cli-acquisition-v1`, `turn-checkpoints`, `auto-commit-on-exit`,
/// `self-review`, `advisor`), several of which write to the workspace or spend; `[remote]` opens
/// Telegram/web control; `[turn]` sets run limits. A trusted workspace keeps its whole layer.
///
/// `[tui.keymap]` is withheld too: it binds `approve` and `deny`, so a hostile repository could
/// swap them (or put approve on Enter) and turn a deliberate deny into an approval. Key bindings
/// come from the user file or a trusted workspace only.
pub(crate) fn untrusted_workspace_layer(cfg: HostConfig) -> HostConfig {
    let mut tui = cfg.tui;
    tui.keymap.clear();
    // A quick slot submits text or runs a command on one key press, so it comes from the user or a trusted workspace only.
    tui.quick_slots.clear();
    // The statusline command is a program the terminal runs: it comes from the user or a trusted workspace only.
    tui.statusline_command.clear();
    HostConfig {
        prompt: cfg.prompt,
        tui,
        ..HostConfig::default()
    }
}

/// Which layer a config file is: the user's own file, or a workspace file that may come from a
/// cloned repository and is read before its trust is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayerKind {
    User,
    Workspace,
}

/// Largest config file read, for any layer. A real config is a few KiB.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Read a layer file: `Ok(None)` when it does not exist. At most [`MAX_CONFIG_BYTES`] are read and
/// only a regular file is read (a FIFO, device or directory is refused). A workspace layer that
/// is a link (symlink, or a Windows reparse point reported as one) is refused without following it.
/// Content that is not UTF-8 is an error.
pub(crate) fn read_layer_bytes(
    path: &std::path::Path,
    kind: LayerKind,
) -> std::io::Result<Option<String>> {
    use std::io::Read;
    let invalid =
        |message: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_string());
    let link_meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let target_meta = if link_meta.file_type().is_symlink() {
        if kind == LayerKind::Workspace {
            return Err(invalid(
                "a workspace config may not be a link; it was not followed",
            ));
        }
        std::fs::metadata(path)?
    } else {
        link_meta
    };
    // Checked before opening: opening a FIFO for reading would block.
    if !target_meta.is_file() {
        return Err(invalid("not a regular file"));
    }
    let file = std::fs::File::open(path)?;
    // Checked on the opened handle, so a swap after the check above still cannot open a device.
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(invalid("not a regular file"));
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(invalid("larger than the 1 MiB config limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(invalid("larger than the 1 MiB config limit"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| invalid("not valid UTF-8"))
}

/// "line L, column C" for a parse error span, computed on the content, with none of its text.
fn parse_error_position(content: &str, span: Option<std::ops::Range<usize>>) -> String {
    let Some(start) = span.map(|span| span.start.min(content.len())) else {
        return "invalid TOML".to_string();
    };
    let start = (0..=start)
        .rev()
        .find(|index| content.is_char_boundary(*index))
        .unwrap_or(0);
    let before = &content[..start];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map(|tail| tail.chars().count() + 1)
        .unwrap_or(1);
    format!("invalid TOML at line {line}, column {column}")
}

/// How long a config writer waits for another writer of the same file.
const CONFIG_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The cross-process lock beside a `config.toml` (`config.toml.lock` in `dir`). The default lock kind
/// also excludes other threads of this process.
fn lock_config(
    dir: &std::path::Path,
    wait: std::time::Duration,
) -> Result<rightkit_fs::FileLock, ConfigError> {
    rightkit_fs::FileLock::try_lock_with(
        &dir.join("config.toml.lock"),
        rightkit_fs::LockKind::default(),
        wait,
    )
    .map_err(|e| ConfigError::Io(format!("config.toml is locked by another process: {e}")))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
