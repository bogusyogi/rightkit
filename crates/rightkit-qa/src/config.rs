//! Per-app config: `rightkit-qa.toml`, the TOML replacement for `right-qa.config.mjs`.
//!
//! ```toml
//! app = "genright"
//! app_root = "."                         # relative to this file
//! evidence_dir = ".cache/rightkit-qa"
//! scenarios = ["tests/scenarios"]        # dirs scanned for *.scenario.toml
//!
//! [env]                                  # every process the harness starts
//! GENRIGHT_NO_KEYCHAIN = "1"
//!
//! [engine]
//! binary = { darwin = "target/debug/genright-engine", windows = "target/debug/genright-engine.exe" }
//! package = "genright-engine"
//! build = ["rightkit", "cargo", "build", "-p", "genright-engine"]
//! data_dir_env = "GENRIGHT_APP_DATA"
//!
//! [ui]
//! binaries = { darwin = "target/debug/genright", windows = "target/debug/genright.exe" }
//! window_label = "main"
//! mode = "hidden"
//! feature = "qa-native"
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
    pub data_dir_env: Option<String>,
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
    evidence_dir: Option<String>,
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
    pub evidence_root: PathBuf,
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
        let text = std::fs::read_to_string(path).map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text, path)
    }

    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let raw: Raw = toml::from_str(text)?;
        let app = require_text(&raw.app, "app")?;
        if !app.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false) || !app.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
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
            evidence_root: app_root.join(raw.evidence_dir.as_deref().unwrap_or(".cache/rightkit-qa")),
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
        Ok(self.engine.binary.for_current()?.map(|p| self.app_root.join(p)))
    }
    pub fn ui_binary(&self) -> Result<Option<PathBuf>> {
        Ok(self.ui.binaries.for_current()?.map(|p| self.app_root.join(p)))
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
        assert_eq!(c.engine.binary.windows.as_deref(), Some("target/debug/genright-engine.exe"));
        assert_eq!(c.ui.binaries.darwin.as_deref(), Some("a"));
        assert_eq!(c.default_tier, "fast");
    }

    #[test]
    fn rejects_bad_app_key_mode_and_tier() {
        assert!(QaConfig::parse("app = \"GenRight\"", Path::new("/tmp/c.toml")).is_err());
        assert!(QaConfig::parse("app = \"x\"\n[ui]\nmode = \"loud\"", Path::new("/tmp/c.toml")).is_err());
        assert!(validate_tier_list("fast,bogus").is_err());
        assert!(validate_tier_list("fast,paid").is_ok());
    }
}
