//! Per-app config: `rightkit-qa.toml`, the TOML replacement for `right-qa.config.mjs`.
//!
//! ```toml
//! app = "genright"
//! app_root = "."                         # relative to this file
//! # `evidence_dir` is no longer read (0.5.0): evidence is always written under the managed runs
//! # root, `<managed root>/runs/<app>/<run-id>/`, never inside the app checkout. An old key is
//! # ignored.
//! scenarios = ["tests/scenarios"]        # dirs scanned for *.scenario.toml
//!
//! [env]                                  # every process the harness starts
//! GENRIGHT_NO_KEYCHAIN = "1"
//!
//! [engine]
//! binary = { darwin = "target/debug/genright-engine", windows = "target/debug/genright-engine.exe" }
//! package = "genright-engine"
//! build = ["rustright", "cargo", "build", "-p", "genright-engine"]
//! data_dir_env = "GENRIGHT_APP_DATA"     # required: a launch with no isolated data dir is refused
//! # data_dir_arg = "--data-dir"          # or name the dir in the step's arguments
//! # forbidden_args = ["--use-user-library"]   # flags that select the real library: refused
//!
//! [ui]
//! binaries = { darwin = "target/debug/genright", windows = "target/debug/genright.exe" }
//! window_label = "main"
//! mode = "hidden"
//! feature = "qa-native"
//! # Finding the dev build (`QaConfig::resolve_ui_binary`, see `binary`): env RIGHTKIT_QA_APP_BINARY,
//! # then `ui.binary_env`, then the pointer file, then the broker's target root, then `binaries`.
//! # bin = "genright"                     # default: package, else the `binaries` file stem
//! # workspace_root = "src-tauri"         # cargo workspace the broker built, relative to app_root
//! # pointer_file = ".rightkit-qa-app-binary"
//! # binary_env = "GENRIGHT_APP_BINARY"
//! # env_keys = ["GENRIGHT_PROFILE"]      # non-secret app env keys a macOS launch may carry
//! # installed_dirs = ["GenRight"]        # Windows: %LOCALAPPDATA% folders of an installed copy
//!                                        # (default: productName in src-tauri/tauri.conf.json)
//!
//! [host]                                 # the scenario pool (ADR-0001 item 2); every key optional
//! # workers = 2                          # pool size; absent: min(cores, commit headroom / peak_mb), capped by the OS start value
//! # peak_mb = 1024                       # configured peak memory of one scenario, in MB
//! # port_base = 41000                    # first port of the first world's range
//! # ports_per_world = 16                 # ports each scenario world may bind
//!
//! [run_tests]                            # `rightkit-qa run-tests` defaults (every key optional; see [`RunTestsConfig`])
//! # parallel = 2                         # test binaries run at the same time
//! # binary_timeout_s = 1800              # one binary's deadline
//! # total_timeout_s = 7200               # the whole execution phase's deadline
//! # test_args = ["--test-threads=1"]     # libtest arguments used when the command line gives none after `--`
//! # env = { GENRIGHT_QA = "1" }          # added to every binary's environment
//! ```
use crate::util::{err, platform, require_text, Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PerPlatform {
    #[serde(alias = "macos")]
    pub darwin: Option<String>,
    #[serde(alias = "win32")]
    pub windows: Option<String>,
}

impl PerPlatform {
    pub fn for_current(&self) -> Result<Option<&str>> {
        Ok(match platform()? {
            "darwin" => self.darwin.as_deref(),
            _ => self.windows.as_deref(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EngineConfig {
    #[serde(default)]
    pub binary: PerPlatform,
    pub package: Option<String>,
    #[serde(default)]
    pub build: Vec<String>,
    /// Env var that receives the per-scenario data dir. Required for any engine step: a launch
    /// that names no isolated data directory is refused (see `userdata::check_engine_launch`).
    pub data_dir_env: Option<String>,
    /// Flag the engine takes its data dir under (e.g. `--data-dir`), when a step names the
    /// isolated directory in its arguments instead of through `data_dir_env`.
    pub data_dir_arg: Option<String>,
    /// Flags that select the real library (e.g. `--use-user-library`); a step whose arguments
    /// contain one is refused.
    #[serde(default)]
    pub forbidden_args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default)]
    pub binaries: PerPlatform,
    #[serde(default = "default_label")]
    pub window_label: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Default for `ui_launch` `focus`: make the window key and focused (off by default).
    #[serde(default)]
    pub focus: bool,
    #[serde(default = "default_feature")]
    pub feature: String,
    /// Build command for the app itself (a debug build with the control feature).
    #[serde(default)]
    pub build: Vec<String>,
    /// Cargo target name whose executable the build produces.
    pub package: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Source files the guard reads (relative to app_root).
    #[serde(default)]
    pub rust_sources: Vec<String>,
    pub cargo_toml: Option<String>,
    pub package_json: Option<String>,
    pub startup_timeout_s: Option<u64>,
    /// Executable name to look for in the broker target root (default: `package`, else the stem of
    /// the `binaries` entry).
    pub bin: Option<String>,
    /// Cargo workspace root, relative to app_root, that the broker built (default: `src-tauri`
    /// when it exists, and app_root itself).
    pub workspace_root: Option<String>,
    /// Pointer file under app_root holding the executable path (default `.rightkit-qa-app-binary`).
    pub pointer_file: Option<String>,
    /// The app's own env var for the executable (checked after `RIGHTKIT_QA_APP_BINARY`).
    pub binary_env: Option<String>,
    /// App-specific, non-secret env keys a macOS launch may carry besides `RIGHTKIT_*` (`open
    /// --env K=V` shows the values in argv, so the harness refuses every other key there). Each
    /// key must be listed in the app's `switches.json` as a release-forbidden switch, so the
    /// release scan proves it absent from shipped binaries; `guard` checks that. A key that names
    /// a credential is refused here and again at launch.
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// Product folder names under `%LOCALAPPDATA%` that hold an installed copy of this app on
    /// Windows (the installed-app guard refuses to drive them). Default: the `productName` of
    /// `<app_root>/src-tauri/tauri.conf.json`, else of `<app_root>/tauri.conf.json`
    /// ([`QaConfig::installed_product_dirs`]); an explicit list, even an empty one, replaces it.
    #[serde(default)]
    pub installed_dirs: Option<Vec<String>>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            binaries: PerPlatform::default(),
            window_label: default_label(),
            mode: default_mode(),
            focus: false,
            feature: default_feature(),
            build: vec![],
            package: None,
            env: BTreeMap::new(),
            rust_sources: vec![],
            cargo_toml: None,
            package_json: None,
            startup_timeout_s: None,
            bin: None,
            workspace_root: None,
            pointer_file: None,
            binary_env: None,
            env_keys: vec![],
            installed_dirs: None,
        }
    }
}

fn default_label() -> String {
    "main".into()
}
fn default_mode() -> String {
    "hidden".into()
}
fn default_feature() -> String {
    "qa-native".into()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AudioConfig {
    /// Also mute the macOS system output for the run (restored afterwards).
    #[serde(default)]
    pub system_mute: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LockConfig {
    pub timeout_s: Option<u64>,
    pub stale_s: Option<u64>,
}

/// `[host]`: how the scenario pool sizes itself and where it hands out ports (see [`crate::pool`]).
/// Unknown keys are refused, so a misspelled `workers` cannot silently fall back to the default.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Pool size. Absent: the pool computes it from cores, commit headroom and `peak_mb`.
    pub workers: Option<usize>,
    /// Configured peak memory of one scenario, in MB, for the headroom division.
    pub peak_mb: Option<u64>,
    /// First port of the first scenario world's range.
    pub port_base: Option<u16>,
    /// Ports in each scenario world's range.
    pub ports_per_world: Option<u16>,
}

impl HostConfig {
    fn validate(&self) -> Result<()> {
        if self.workers == Some(0) {
            return err("[host] workers must be at least 1");
        }
        if self.peak_mb == Some(0) {
            return err("[host] peak_mb must be at least 1");
        }
        if self.ports_per_world == Some(0) {
            return err("[host] ports_per_world must be at least 1");
        }
        if let Some(base) = self.port_base {
            if base < crate::worker::MIN_WORLD_PORT {
                return err(format!(
                    "[host] port_base {base} is below {}, the start of the unprivileged range",
                    crate::worker::MIN_WORLD_PORT
                ));
            }
        }
        Ok(())
    }
}

/// `[run_tests]`: defaults for `rightkit-qa run-tests`, the command that compiles an app's test
/// binaries through the broker and runs them outside it. A command-line flag wins over the file, the
/// file over the built-in default. Unknown keys are refused, so a misspelled `parallel` cannot
/// silently fall back to the default.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunTestsConfig {
    /// Test binaries run at the same time. Default 2 ([`RunTestsConfig::DEFAULT_PARALLEL`]).
    pub parallel: Option<usize>,
    /// Deadline for one test binary, in seconds. Default 1800.
    pub binary_timeout_s: Option<u64>,
    /// Deadline for the whole execution phase (not the compile), in seconds. Default 7200.
    pub total_timeout_s: Option<u64>,
    /// Deadline for the compile, in seconds. Default 2700 (the journeys build's own bound).
    pub compile_timeout_s: Option<u64>,
    /// Libtest arguments used when the command line has none after `--` (for example
    /// `["--test-threads=1", "--nocapture"]`). An explicit `--` on the command line replaces them.
    pub test_args: Option<Vec<String>>,
    /// Where each binary runs: `package` (the default: its package directory, as cargo does),
    /// `workspace` (the app root) or a directory relative to the app root.
    pub cwd: Option<String>,
    /// Added to every binary's environment. `--env KEY=VALUE` on the command line wins per key.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables removed from every binary's environment. Default
    /// `["RIGHTKIT_FORWARD_ENV", "RUSTC_WRAPPER"]`, the build-time variables a test binary must not see.
    pub unset_env: Option<Vec<String>>,
    /// Give each binary its own short temp directory (`TMPDIR`, or `TEMP` and `TMP` on Windows),
    /// removed when it exits. Default off.
    pub isolate_tmp: Option<bool>,
    /// A run whose binaries report any ignored test fails: an ignored test is a test that did not
    /// run. Default off.
    pub deny_ignored: Option<bool>,
    /// Refuse to compile anywhere but the broker (`rustright cargo`): a host without it fails
    /// instead of falling back to plain cargo. Default off.
    pub require_broker: Option<bool>,
}

impl RunTestsConfig {
    pub const DEFAULT_PARALLEL: usize = 2;
    pub const MAX_PARALLEL: usize = 32;
    pub const DEFAULT_BINARY_TIMEOUT_S: u64 = 1800;
    pub const DEFAULT_TOTAL_TIMEOUT_S: u64 = 7200;
    pub const DEFAULT_COMPILE_TIMEOUT_S: u64 = 2700;
    /// Build-time variables a test binary does not need and must not inherit (Pulse's gate unsets
    /// both before it runs the binaries).
    pub const DEFAULT_UNSET_ENV: [&'static str; 2] = ["RIGHTKIT_FORWARD_ENV", "RUSTC_WRAPPER"];

    pub fn validate(&self) -> Result<()> {
        if let Some(n) = self.parallel {
            if !(1..=Self::MAX_PARALLEL).contains(&n) {
                return err(format!(
                    "[run_tests] parallel must be between 1 and {}",
                    Self::MAX_PARALLEL
                ));
            }
        }
        for (key, value) in [
            ("binary_timeout_s", self.binary_timeout_s),
            ("total_timeout_s", self.total_timeout_s),
            ("compile_timeout_s", self.compile_timeout_s),
        ] {
            if value == Some(0) {
                return err(format!("[run_tests] {key} must be at least 1"));
            }
        }
        if let Some(cwd) = &self.cwd {
            if cwd.trim().is_empty() {
                return err(
                    "[run_tests] cwd must not be empty (package, workspace or a directory)",
                );
            }
        }
        for key in self.env.keys().chain(self.unset_env.iter().flatten()) {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return err(format!(
                    "[run_tests] {key:?} is not a valid environment variable name"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Raw {
    app: String,
    #[serde(default = "dot")]
    app_root: String,
    #[serde(default)]
    scenarios: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    engine: EngineConfig,
    #[serde(default)]
    ui: UiConfig,
    #[serde(default)]
    audio: AudioConfig,
    #[serde(default)]
    lock: LockConfig,
    #[serde(default)]
    host: HostConfig,
    /// `[run_tests]`: defaults for `rightkit-qa run-tests` (see [`RunTestsConfig`]).
    #[serde(default)]
    run_tests: RunTestsConfig,
    /// `[tiers]` maps a tier name to the tiers it also selects ("all" = every tier).
    #[serde(default)]
    default_tier: Option<String>,
    /// `[plugin]`: the app's step plugin for a `run --qualifying` (see [`PluginConfig`]).
    #[serde(default)]
    plugin: Option<PluginConfig>,
}

fn dot() -> String {
    ".".into()
}

/// `[plugin]` in `rightkit-qa.toml`: the step plugin binary a `run --qualifying` starts per scenario
/// (protocol v1, `crate::plugin`). `binary` is relative to the app root, per platform. A journeys run
/// takes its plugin from `journeys.toml` `[plugin]` (a build-record role) instead.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    #[serde(default)]
    pub binary: PerPlatform,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub hello_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct QaConfig {
    pub app: String,
    pub app_root: PathBuf,
    pub scenario_dirs: Vec<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub engine: EngineConfig,
    pub ui: UiConfig,
    pub audio: AudioConfig,
    pub lock: LockConfig,
    pub host: HostConfig,
    pub run_tests: RunTestsConfig,
    pub default_tier: String,
    pub config_path: PathBuf,
    pub plugin: Option<PluginConfig>,
}

pub const TIERS: [&str; 3] = ["fast", "local-models", "paid"];

impl QaConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        let cfg = Self::parse(&text, path)?;
        // The installed-app guard must know this app's product folders before anything launches.
        // Here, not in `parse`, which stays free of process-wide effects.
        crate::installed::declare_product_dirs(cfg.installed_product_dirs());
        Ok(cfg)
    }

    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let raw: Raw = toml::from_str(text)?;
        let app = require_text(&raw.app, "app")?;
        if !app
            .chars()
            .next()
            .map(|c| c.is_ascii_lowercase())
            .unwrap_or(false)
            || !app
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return err("app must be a lowercase app key");
        }
        let base = path.parent().unwrap_or(Path::new("."));
        let app_root = base.join(require_text(&raw.app_root, "app_root")?);
        let app_root = crate::util::canonical_plain(&app_root).unwrap_or(app_root);
        if raw.ui.window_label.trim().is_empty() {
            return err("ui.window_label is required");
        }
        crate::control::Mode::parse(&raw.ui.mode)?;
        validate_env_keys(&raw.ui.env_keys)?;
        let default_tier = raw.default_tier.unwrap_or_else(|| "fast".into());
        validate_tier_list(&default_tier)?;
        raw.host.validate()?;
        raw.run_tests.validate()?;
        Ok(Self {
            scenario_dirs: raw.scenarios.iter().map(|s| app_root.join(s)).collect(),
            app,
            app_root,
            env: raw.env,
            engine: raw.engine,
            ui: raw.ui,
            audio: raw.audio,
            lock: raw.lock,
            host: raw.host,
            run_tests: raw.run_tests,
            default_tier,
            config_path: path.to_path_buf(),
            plugin: raw.plugin,
        })
    }

    /// The `[plugin]` of `rightkit-qa.toml` as a spec the qualifying suite starts: the binary for this
    /// platform resolved against the app root. `None` when no `[plugin]` is declared; a declared
    /// `[plugin]` with no binary for this platform, or a hello timeout outside 1 to 60000 ms, is refused.
    pub fn plugin_spec(&self) -> Result<Option<crate::plugin::PluginSpec>> {
        let Some(p) = &self.plugin else {
            return Ok(None);
        };
        let Some(rel) = p.binary.for_current()? else {
            return err("[plugin] names no binary for this platform: set [plugin].binary");
        };
        let timeout = p.hello_timeout_ms.unwrap_or(10_000);
        if !(1..=60_000).contains(&timeout) {
            return err("[plugin].hello_timeout_ms must be between 1 and 60000");
        }
        let mut spec = crate::plugin::PluginSpec::new(self.app_root.join(rel));
        spec.args = p.args.clone();
        spec.hello_timeout = std::time::Duration::from_millis(timeout);
        Ok(Some(spec))
    }

    /// Product folder names under `%LOCALAPPDATA%` that hold an installed copy of this app: the
    /// explicit `[ui] installed_dirs`, else the `productName` of
    /// `<app_root>/src-tauri/tauri.conf.json`, else of `<app_root>/tauri.conf.json` (the first
    /// that names one). A missing, unreadable or unparsable file, or one with no `productName`,
    /// gives an empty list, never an error.
    pub fn installed_product_dirs(&self) -> Vec<String> {
        if let Some(dirs) = &self.ui.installed_dirs {
            return dirs.clone();
        }
        ["src-tauri/tauri.conf.json", "tauri.conf.json"]
            .iter()
            .find_map(|rel| product_name_of(&self.app_root.join(rel)))
            .into_iter()
            .collect()
    }

    pub fn engine_binary(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .engine
            .binary
            .for_current()?
            .map(|p| self.app_root.join(p)))
    }
    pub fn ui_binary(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .ui
            .binaries
            .for_current()?
            .map(|p| self.app_root.join(p)))
    }

    /// The dev executable for this app: env override, pointer file, the broker's target root, then
    /// the `ui.binaries` path (see [`crate::binary`]), always returned as the fixed stash copy
    /// (`<app root>/.rightkit-qa-bin/...`, copied in place with a kept `.previous`), never a broker
    /// generation path. `source` names where the build was found. Errors when none of them holds a
    /// valid file.
    pub fn resolve_ui_binary(&self) -> Result<crate::binary::ResolvedBinary> {
        self.resolve_ui_binary_stashed_with(
            Some(crate::binary::broker_root_of(
                &crate::storage::managed_root(None),
            )),
            &|k| std::env::var(k).ok(),
        )
    }

    /// [`Self::resolve_ui_binary`] with the broker root and env supplied (tests, odd layouts).
    pub fn resolve_ui_binary_stashed_with(
        &self,
        broker_root: Option<PathBuf>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<crate::binary::ResolvedBinary> {
        self.with_binary_query(broker_root, env, crate::binary::resolve_stashed)
    }

    /// Discovery only, without the stash copy: where the build is now (which may be a broker
    /// generation path). Launches use [`Self::resolve_ui_binary`], never this path.
    pub fn resolve_ui_binary_with(
        &self,
        broker_root: Option<PathBuf>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<crate::binary::ResolvedBinary> {
        self.with_binary_query(broker_root, env, crate::binary::resolve)
    }

    fn with_binary_query(
        &self,
        broker_root: Option<PathBuf>,
        env: &dyn Fn(&str) -> Option<String>,
        f: impl FnOnce(&crate::binary::BinaryQuery) -> Result<crate::binary::ResolvedBinary>,
    ) -> Result<crate::binary::ResolvedBinary> {
        let rel = self.ui.binaries.for_current()?;
        let bin = self
            .ui
            .bin
            .clone()
            .or_else(|| self.ui.package.clone())
            .or_else(|| {
                rel.and_then(|r| Path::new(r).file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
            });
        let Some(bin) = bin else {
            return err("set [ui].bin, [ui].package or [ui].binaries to name the executable");
        };
        let workspace_roots = match &self.ui.workspace_root {
            Some(w) => vec![self.app_root.join(w)],
            None => vec![self.app_root.join("src-tauri"), self.app_root.clone()],
        };
        f(&crate::binary::BinaryQuery {
            bin: &bin,
            app_root: &self.app_root,
            workspace_roots,
            broker_root,
            extra_env: self.ui.binary_env.as_deref(),
            pointer_file: self.ui.pointer_file.as_deref(),
            app_relative: rel,
            env,
        })
    }
}

/// The non-empty `productName` of a Tauri config file; `None` when it cannot be read or parsed as
/// JSON, or names no product. A leading byte-order mark is skipped.
fn product_name_of(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let name = json.get("productName")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// `[ui] env_keys`: each key is a plain environment variable name (ASCII letters, digits and
/// `_`, not starting with a digit) that names no credential. It uses the launch's own credential
/// word list ([`crate::control::key_names_credential`]), so config and launch cannot disagree.
fn validate_env_keys(keys: &[String]) -> Result<()> {
    for key in keys {
        let well_formed = !key.is_empty()
            && !key.starts_with(|c: char| c.is_ascii_digit())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !well_formed {
            return err(format!(
                "[ui] env_keys: {key:?} is not an environment variable name (ASCII letters, digits and '_', not starting with a digit)"
            ));
        }
        if crate::control::key_names_credential(key) {
            return err(format!(
                "[ui] env_keys: {key} names a credential; secrets are never passed to a launched app through its command line or environment"
            ));
        }
    }
    Ok(())
}

pub fn validate_tier_list(list: &str) -> Result<()> {
    for t in list.split(',').map(str::trim) {
        if t != "all" && !TIERS.contains(&t) {
            return err(format!("unknown tier '{t}' (fast|local-models|paid|all)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
app = "genright"
[env]
GENRIGHT_NO_KEYCHAIN = "1"
[engine]
binary = { darwin = "target/debug/genright-engine", win32 = "target/debug/genright-engine.exe" }
data_dir_env = "GENRIGHT_APP_DATA"
[ui]
binaries = { macos = "a", windows = "b" }
mode = "background"
"#;

    #[test]
    fn parses_aliases_and_defaults() {
        let c = QaConfig::parse(SAMPLE, Path::new("/tmp/x/rightkit-qa.toml")).unwrap();
        assert_eq!(c.app, "genright");
        assert_eq!(c.ui.window_label, "main");
        assert_eq!(
            c.engine.binary.windows.as_deref(),
            Some("target/debug/genright-engine.exe")
        );
        assert_eq!(c.ui.binaries.darwin.as_deref(), Some("a"));
        assert_eq!(c.default_tier, "fast");
    }

    #[test]
    fn ui_focus_defaults_off_and_parses() {
        let c = QaConfig::parse(SAMPLE, Path::new("/tmp/x/rightkit-qa.toml")).unwrap();
        assert!(!c.ui.focus);
        let on = SAMPLE.replace("mode = \"background\"", "mode = \"hidden\"\nfocus = true");
        let c = QaConfig::parse(&on, Path::new("/tmp/x/rightkit-qa.toml")).unwrap();
        assert!(c.ui.focus);
    }

    #[test]
    fn resolves_ui_binary_from_config_without_the_real_broker() {
        let base = std::env::temp_dir().join(format!("rkqa-cfg-{}", crate::util::new_id()));
        let exe = if cfg!(windows) { "myapp.exe" } else { "myapp" };
        let built = base.join("target/debug").join(exe);
        std::fs::create_dir_all(built.parent().unwrap()).unwrap();
        std::fs::write(&built, crate::binary::QA_CONTROL_MARKER.as_bytes()).unwrap();
        let toml = format!(
            "app = \"myapp\"
[ui]
binaries = {{ macos = \"target/debug/{exe}\", windows = \"target/debug/{exe}\" }}
"
        );
        let c = QaConfig::parse(&toml, &base.join("rightkit-qa.toml")).unwrap();
        let none = |_: &str| None;
        let got = c.resolve_ui_binary_with(None, &none).unwrap();
        assert_eq!(got.source, crate::binary::BinarySource::AppRoot);
        assert!(got.path.ends_with(exe));
        let env =
            |k: &str| (k == "RIGHTKIT_QA_APP_BINARY").then(|| built.to_string_lossy().into_owned());
        let got = c.resolve_ui_binary_with(None, &env).unwrap();
        assert!(matches!(got.source, crate::binary::BinarySource::Env(_)));
    }

    #[test]
    fn host_section_defaults_parses_and_refuses_bad_values() {
        let p = Path::new("/tmp/x/rightkit-qa.toml");
        let c = QaConfig::parse(SAMPLE, p).unwrap();
        assert_eq!(c.host, HostConfig::default());
        let with = format!("{SAMPLE}\n[host]\nworkers = 3\npeak_mb = 2048\nport_base = 42000\nports_per_world = 8\n");
        let c = QaConfig::parse(&with, p).unwrap();
        assert_eq!(c.host.workers, Some(3));
        assert_eq!(c.host.peak_mb, Some(2048));
        assert_eq!(c.host.port_base, Some(42000));
        assert_eq!(c.host.ports_per_world, Some(8));
        for bad in [
            "workers = 0",
            "peak_mb = 0",
            "ports_per_world = 0",
            "port_base = 80",
            "worker = 2",
        ] {
            let text = format!("{SAMPLE}\n[host]\n{bad}\n");
            assert!(QaConfig::parse(&text, p).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn run_tests_section_defaults_parses_and_refuses_bad_values() {
        let p = Path::new("/tmp/x/rightkit-qa.toml");
        let c = QaConfig::parse(SAMPLE, p).unwrap();
        assert_eq!(c.run_tests, RunTestsConfig::default());
        let with = format!(
            "{SAMPLE}\n[run_tests]\nparallel = 4\nbinary_timeout_s = 600\ntotal_timeout_s = 3000\ncompile_timeout_s = 900\ntest_args = [\"--test-threads=1\", \"--nocapture\"]\ncwd = \"workspace\"\nunset_env = [\"RUSTC_WRAPPER\"]\nisolate_tmp = true\ndeny_ignored = true\nrequire_broker = true\n[run_tests.env]\nGENRIGHT_QA = \"1\"\n"
        );
        let c = QaConfig::parse(&with, p).unwrap();
        let rt = &c.run_tests;
        assert_eq!(rt.parallel, Some(4));
        assert_eq!(rt.binary_timeout_s, Some(600));
        assert_eq!(rt.total_timeout_s, Some(3000));
        assert_eq!(rt.compile_timeout_s, Some(900));
        assert_eq!(
            rt.test_args.as_deref(),
            Some(&["--test-threads=1".to_string(), "--nocapture".to_string()][..])
        );
        assert_eq!(rt.cwd.as_deref(), Some("workspace"));
        assert_eq!(rt.env.get("GENRIGHT_QA").map(String::as_str), Some("1"));
        assert_eq!(
            rt.unset_env.as_deref(),
            Some(&["RUSTC_WRAPPER".to_string()][..])
        );
        assert_eq!(rt.isolate_tmp, Some(true));
        assert_eq!(rt.deny_ignored, Some(true));
        assert_eq!(rt.require_broker, Some(true));
        for bad in [
            "parallel = 0",
            "parallel = 33",
            "binary_timeout_s = 0",
            "total_timeout_s = 0",
            "compile_timeout_s = 0",
            "cwd = \" \"",
            "unset_env = [\"A=B\"]",
            "paralel = 2",
        ] {
            let text = format!("{SAMPLE}\n[run_tests]\n{bad}\n");
            assert!(QaConfig::parse(&text, p).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn rejects_bad_app_key_mode_and_tier() {
        assert!(QaConfig::parse("app = \"GenRight\"", Path::new("/tmp/c.toml")).is_err());
        assert!(QaConfig::parse(
            "app = \"x\"\n[ui]\nmode = \"loud\"",
            Path::new("/tmp/c.toml")
        )
        .is_err());
        assert!(validate_tier_list("fast,bogus").is_err());
        assert!(validate_tier_list("fast,paid").is_ok());
    }
}

#[cfg(test)]
mod plugin_config_tests {
    use super::*;

    fn parse(extra: &str) -> Result<QaConfig> {
        let text = format!("app = \"demo\"\napp_root = \".\"\n{extra}\n");
        QaConfig::parse(&text, Path::new("/nonexistent-cfg/rightkit-qa.toml"))
    }

    #[test]
    fn a_plugin_table_becomes_a_spec_resolved_against_the_app_root() {
        let none = parse("").unwrap();
        assert!(none.plugin_spec().unwrap().is_none());
        let c = parse(
            "[plugin]\nbinary = { darwin = \"bin/p\", windows = \"bin/p.exe\" }\nargs = [\"--x\"]\nhello_timeout_ms = 2000\n",
        )
        .unwrap();
        let spec = c.plugin_spec().unwrap().expect("a declared plugin");
        assert!(spec.binary.starts_with(&c.app_root), "{:?}", spec.binary);
        assert_eq!(spec.args, vec!["--x".to_string()]);
        assert_eq!(spec.hello_timeout, std::time::Duration::from_millis(2000));
    }

    #[test]
    fn a_plugin_table_with_a_bad_timeout_or_no_binary_is_refused() {
        let slow = parse(
            "[plugin]\nbinary = { darwin = \"p\", windows = \"p.exe\" }\nhello_timeout_ms = 0\n",
        )
        .unwrap();
        assert!(slow.plugin_spec().is_err());
        // A declared table with no binary for this platform is refused, not ignored.
        let nobin = parse("[plugin]\nargs = []\n").unwrap();
        assert!(nobin.plugin_spec().is_err());
        // Unknown keys are refused by the parser.
        assert!(parse("[plugin]\nbinary = { darwin = \"p\" }\nbogus = 1\n").is_err());
    }
}

#[cfg(test)]
mod ui_declaration_tests {
    use super::*;

    const BASE: &str = "app = \"demo\"\n";

    fn parse_ui(ui: &str) -> Result<QaConfig> {
        let text = format!("{BASE}[ui]\n{ui}\n");
        QaConfig::parse(&text, Path::new("/nonexistent-cfg/rightkit-qa.toml"))
    }

    /// A temp app root holding `files` (relative path, text); the config sits beside them.
    fn app_root(files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rkqa-ui-{}", crate::util::new_id()));
        for (rel, text) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn config_in(root: &Path, ui: &str) -> QaConfig {
        let text = format!("{BASE}[ui]\n{ui}\n");
        QaConfig::parse(&text, &root.join("rightkit-qa.toml")).unwrap()
    }

    #[test]
    fn env_keys_default_to_empty_and_parse() {
        assert!(QaConfig::parse(BASE, Path::new("/nonexistent-cfg/c.toml"))
            .unwrap()
            .ui
            .env_keys
            .is_empty());
        assert!(UiConfig::default().env_keys.is_empty());
        let c = parse_ui("env_keys = [\"DEMO_PROFILE\", \"_demo2\"]").unwrap();
        assert_eq!(c.ui.env_keys, vec!["DEMO_PROFILE", "_demo2"]);
    }

    #[test]
    fn a_credential_named_env_key_is_refused_at_parse() {
        for key in [
            "DEMO_API_KEY",
            "demo_token",
            "DEMO_PASSWORD",
            "DEMO_SECRET_X",
        ] {
            let e = parse_ui(&format!("env_keys = [\"{key}\"]")).unwrap_err();
            assert!(e.0.contains("names a credential"), "{key}: {e}");
            assert!(e.0.contains(key), "{key}: {e}");
        }
    }

    #[test]
    fn a_malformed_env_key_is_refused_at_parse() {
        for key in [
            "",
            "1DEMO",
            "DEMO-MODE",
            "DEMO MODE",
            " DEMO",
            "DEMO=1",
            "D\u{e9}MO",
        ] {
            let e = parse_ui(&format!("env_keys = [\"{key}\"]")).unwrap_err();
            assert!(e.0.contains("env_keys"), "{key:?}: {e}");
        }
        // One bad key among good ones refuses the whole list.
        assert!(parse_ui("env_keys = [\"DEMO_OK\", \"bad key\"]").is_err());
    }

    #[test]
    fn installed_product_dirs_reads_the_product_name_of_the_tauri_config() {
        let root = app_root(&[(
            "src-tauri/tauri.conf.json",
            r#"{ "productName": "Demo App", "identifier": "app.demo" }"#,
        )]);
        let c = config_in(&root, "");
        assert_eq!(c.installed_product_dirs(), vec!["Demo App".to_string()]);
        // With no src-tauri config the app root's own is used; src-tauri wins when both name one.
        let flat = app_root(&[("tauri.conf.json", r#"{ "productName": "Flat" }"#)]);
        assert_eq!(config_in(&flat, "").installed_product_dirs(), vec!["Flat"]);
        let both = app_root(&[
            ("src-tauri/tauri.conf.json", r#"{ "productName": "Inner" }"#),
            ("tauri.conf.json", r#"{ "productName": "Outer" }"#),
        ]);
        assert_eq!(config_in(&both, "").installed_product_dirs(), vec!["Inner"]);
        for dir in [root, flat, both] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn an_explicit_installed_dirs_wins_over_the_tauri_config() {
        let root = app_root(&[(
            "src-tauri/tauri.conf.json",
            r#"{ "productName": "FromTauri" }"#,
        )]);
        let c = config_in(&root, "installed_dirs = [\"One\", \"Two\"]");
        assert_eq!(c.installed_product_dirs(), vec!["One", "Two"]);
        // An explicit empty list is a choice too: nothing is declared.
        let none = config_in(&root, "installed_dirs = []");
        assert!(none.installed_product_dirs().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn installed_product_dirs_is_empty_when_no_config_names_a_product() {
        let missing = app_root(&[]);
        assert!(config_in(&missing, "").installed_product_dirs().is_empty());
        for text in [
            "not json at all",
            "{}",
            r#"{ "productName": 7 }"#,
            r#"{ "productName": "  " }"#,
        ] {
            let root = app_root(&[("src-tauri/tauri.conf.json", text)]);
            assert!(
                config_in(&root, "").installed_product_dirs().is_empty(),
                "{text}"
            );
            let _ = std::fs::remove_dir_all(root);
        }
        let _ = std::fs::remove_dir_all(missing);
    }

    #[test]
    fn load_declares_the_installed_product_dirs_and_parse_does_not() {
        // A name no other test declares: the declared list is process-wide.
        let name = "RkQaCfgLoadProbe";
        let tauri = format!("{{ \"productName\": \"{name}\" }}");
        let root = app_root(&[("src-tauri/tauri.conf.json", tauri.as_str())]);
        let path = root.join("rightkit-qa.toml");
        std::fs::write(&path, BASE).unwrap();
        let declared = || crate::installed::declared_product_dirs();
        QaConfig::parse(BASE, &path).unwrap();
        assert!(!declared().iter().any(|d| d == name), "parse declared it");
        let c = QaConfig::load(&path).unwrap();
        assert_eq!(c.installed_product_dirs(), vec![name.to_string()]);
        assert!(
            declared().iter().any(|d| d == name),
            "load did not declare it"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
