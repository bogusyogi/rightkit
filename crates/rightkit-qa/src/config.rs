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
//! build = ["rightkit", "cargo", "build", "-p", "genright-engine"]
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
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            binaries: PerPlatform::default(),
            window_label: default_label(),
            mode: default_mode(),
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
    /// `[tiers]` maps a tier name to the tiers it also selects ("all" = every tier).
    #[serde(default)]
    default_tier: Option<String>,
}

fn dot() -> String {
    ".".into()
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
    pub default_tier: String,
    pub config_path: PathBuf,
}

pub const TIERS: [&str; 3] = ["fast", "local-models", "paid"];

impl QaConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text, path)
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
        let app_root = app_root.canonicalize().unwrap_or(app_root);
        if raw.ui.window_label.trim().is_empty() {
            return err("ui.window_label is required");
        }
        crate::control::Mode::parse(&raw.ui.mode)?;
        let default_tier = raw.default_tier.unwrap_or_else(|| "fast".into());
        validate_tier_list(&default_tier)?;
        Ok(Self {
            scenario_dirs: raw.scenarios.iter().map(|s| app_root.join(s)).collect(),
            app,
            app_root,
            env: raw.env,
            engine: raw.engine,
            ui: raw.ui,
            audio: raw.audio,
            lock: raw.lock,
            default_tier,
            config_path: path.to_path_buf(),
        })
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
    /// the `ui.binaries` path. See [`crate::binary`]. Errors when none of them holds a valid file.
    pub fn resolve_ui_binary(&self) -> Result<crate::binary::ResolvedBinary> {
        self.resolve_ui_binary_with(
            Some(crate::binary::broker_root_of(
                &crate::storage::managed_root(None),
            )),
            &|k| std::env::var(k).ok(),
        )
    }

    /// [`Self::resolve_ui_binary`] with the broker root and env supplied (tests, odd layouts).
    pub fn resolve_ui_binary_with(
        &self,
        broker_root: Option<PathBuf>,
        env: &dyn Fn(&str) -> Option<String>,
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
        crate::binary::resolve(&crate::binary::BinaryQuery {
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
    fn resolves_ui_binary_from_config_without_the_real_broker() {
        let base = std::env::temp_dir().join(format!("rkqa-cfg-{}", crate::util::new_id()));
        let exe = if cfg!(windows) { "myapp.exe" } else { "myapp" };
        let built = base.join("target/debug").join(exe);
        std::fs::create_dir_all(built.parent().unwrap()).unwrap();
        std::fs::write(&built, b"x").unwrap();
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
