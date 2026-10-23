//! Test settings that survive `rustright cargo test`.
//!
//! Managed cargo (`rustright cargo ...`) starts the test binary with a scrubbed environment and
//! refuses nested managed builds, so a Rust integration test cannot rely on `MY_APP_E2E_TIER=paid
//! rustright cargo test`. The supported pattern is:
//!
//! * **Tiers are cargo features** of the test crate (`--features full,paid`), read at compile time
//!   with `cfg!(feature = "...")` and handed to [`crate::harness::Harness::with_feature_tiers`].
//! * **Knobs are a settings file** (`KEY=VALUE` lines) read at run time through [`Settings`].
//! * **Binaries are built first** by a separate `rustright cargo build -p <bin>`, then located
//!   next to the test executable's `deps/` directory (never by building from inside a test).
//!
//! # File format
//!
//! One `KEY=VALUE` per line. Blank lines and lines starting with `#` are ignored, an optional
//! leading `export ` is stripped, and a value wrapped in matching single or double quotes is
//! unquoted. Later lines override earlier ones. Keys are case-sensitive.
//!
//! # Search order ([`Settings::discover`])
//!
//! The first file that exists wins; files are never merged:
//!
//! 1. the path in the `RIGHTKIT_QA_SETTINGS` environment variable (an error if it is set but the
//!    file does not exist, so a typo never silently runs the defaults),
//! 2. `<root>/.cache/qa/settings.env`,
//! 3. `<root>/.cache/e2e/settings.env` (the location GenRight's e2e suite already uses),
//! 4. `<root>/rightkit-qa.settings.env`.
//!
//! # Lookup order ([`Settings::get`])
//!
//! A non-empty process environment variable wins over the file, so a direct (unmanaged) run can
//! still override a single knob; under `rustright cargo` the file is what reaches the test.
use crate::util::{err, Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Environment variable naming an explicit settings file (search step 1).
pub const SETTINGS_ENV: &str = "RIGHTKIT_QA_SETTINGS";

/// Relative locations searched under the app root, in order (search steps 2-4).
pub const SEARCH_PATHS: &[&str] = &[
    ".cache/qa/settings.env",
    ".cache/e2e/settings.env",
    "rightkit-qa.settings.env",
];

/// Parsed `KEY=VALUE` settings plus where they came from.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    values: BTreeMap<String, String>,
    source: Option<PathBuf>,
    use_env: bool,
}

impl Settings {
    /// No file; lookups consult the process environment only.
    pub fn env_only() -> Self {
        Self {
            use_env: true,
            ..Self::default()
        }
    }

    /// Fixed values with no environment fallback (tests, generated configs).
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Self {
            values: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            source: None,
            use_env: false,
        }
    }

    /// Parse settings text (see the module docs for the format).
    pub fn parse(text: &str) -> Result<Self> {
        let mut values = BTreeMap::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((k, v)) = line.split_once('=') else {
                return err(format!(
                    "settings line {}: expected KEY=VALUE, got '{raw}'",
                    i + 1
                ));
            };
            let key = k.trim();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return err(format!("settings line {}: invalid key '{k}'", i + 1));
            }
            values.insert(key.to_string(), unquote(v.trim()).to_string());
        }
        Ok(Self {
            values,
            source: None,
            use_env: true,
        })
    }

    /// Load one file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error(format!("settings {}: {e}", path.display())))?;
        let mut s = Self::parse(&text).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        s.source = Some(path.to_path_buf());
        Ok(s)
    }

    /// The file [`Settings::discover`] would read for `root`, if any.
    pub fn locate(root: &Path) -> Result<Option<PathBuf>> {
        if let Some(p) = std::env::var_os(SETTINGS_ENV).filter(|v| !v.is_empty()) {
            let p = PathBuf::from(p);
            if !p.is_file() {
                return err(format!(
                    "{SETTINGS_ENV} names {} but no such file exists",
                    p.display()
                ));
            }
            return Ok(Some(p));
        }
        Ok(SEARCH_PATHS
            .iter()
            .map(|rel| root.join(rel))
            .find(|p| p.is_file()))
    }

    /// Load the first settings file in the documented search order; no file is not an error.
    pub fn discover(root: &Path) -> Result<Self> {
        match Self::locate(root)? {
            Some(p) => Self::load(&p),
            None => Ok(Self::env_only()),
        }
    }

    /// The file these settings were read from.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Enable or disable the process-environment override.
    pub fn with_env_override(mut self, on: bool) -> Self {
        self.use_env = on;
        self
    }

    /// Set or replace one value (file-level; the environment still wins when enabled).
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    /// Non-empty value for `key`: process environment first (when enabled), then the file.
    pub fn get(&self, key: &str) -> Option<String> {
        if self.use_env {
            if let Ok(v) = std::env::var(key) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        self.values.get(key).filter(|v| !v.is_empty()).cloned()
    }

    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or_else(|| default.to_string())
    }

    /// Present or an error naming the key and the file.
    pub fn require(&self, key: &str) -> Result<String> {
        self.get(key).ok_or_else(|| {
            Error(format!(
                "missing setting {key} ({})",
                self.source
                    .as_ref()
                    .map(|p| format!("settings file {}", p.display()))
                    .unwrap_or_else(|| "no settings file".into())
            ))
        })
    }

    /// Parsed value; `Ok(None)` when absent, an error when present but malformed.
    pub fn parse_value<T: FromStr>(&self, key: &str) -> Result<Option<T>>
    where
        T::Err: std::fmt::Display,
    {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .parse::<T>()
                .map(Some)
                .map_err(|e| Error(format!("setting {key}='{v}': {e}"))),
        }
    }

    /// `1/true/yes/on` or `0/false/no/off` (case-insensitive); anything else is an error.
    pub fn get_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(Some(true)),
                "0" | "false" | "no" | "off" => Ok(Some(false)),
                _ => err(format!("setting {key}='{v}' is not a boolean")),
            },
        }
    }

    pub fn get_u64(&self, key: &str) -> Result<Option<u64>> {
        self.parse_value(key)
    }

    pub fn get_f64(&self, key: &str) -> Result<Option<f64>> {
        self.parse_value(key)
    }

    /// Comma-separated list, items trimmed, empties dropped.
    pub fn get_list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A path; a relative value resolves against `base` (usually the app root).
    pub fn get_path(&self, key: &str, base: &Path) -> Option<PathBuf> {
        self.get(key).map(|v| {
            let p = PathBuf::from(v);
            if p.is_absolute() {
                p
            } else {
                base.join(p)
            }
        })
    }

    /// File-level keys (environment overrides are not listed).
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_export_quotes_and_overrides() {
        let s = Settings::parse("# knobs\n\nexport A=1\nB = \"two words\"\nC='x=y'\nA=3\nEMPTY=\n")
            .unwrap()
            .with_env_override(false);
        assert_eq!(s.get("A").as_deref(), Some("3"));
        assert_eq!(s.get("B").as_deref(), Some("two words"));
        assert_eq!(s.get("C").as_deref(), Some("x=y"));
        assert_eq!(s.get("EMPTY"), None);
        assert!(Settings::parse("no equals sign").is_err());
        assert!(Settings::parse("BAD KEY=1").is_err());
    }

    #[test]
    fn typed_getters_reject_malformed_values() {
        let s = Settings::from_pairs([
            ("ON", "Yes"),
            ("OFF", "0"),
            ("N", "42"),
            ("F", "-16.5"),
            ("BAD", "maybe"),
            ("L", " fast, paid ,,full "),
            ("P", "rel/dir"),
        ]);
        assert_eq!(s.get_bool("ON").unwrap(), Some(true));
        assert_eq!(s.get_bool("OFF").unwrap(), Some(false));
        assert_eq!(s.get_bool("MISSING").unwrap(), None);
        assert!(s.get_bool("BAD").is_err());
        assert_eq!(s.get_u64("N").unwrap(), Some(42));
        assert!(s.get_u64("F").is_err());
        assert_eq!(s.get_f64("F").unwrap(), Some(-16.5));
        assert_eq!(s.get_list("L"), vec!["fast", "paid", "full"]);
        assert_eq!(
            s.get_path("P", Path::new("/root")),
            Some(PathBuf::from("/root/rel/dir"))
        );
        assert!(s.require("MISSING").unwrap_err().0.contains("MISSING"));
        assert_eq!(s.get_or("MISSING", "d"), "d");
    }

    #[test]
    fn discover_follows_search_order() {
        // RIGHTKIT_QA_SETTINGS is not set under managed cargo; a direct run that sets it would
        // legitimately change the answer, so only assert when it is absent.
        if std::env::var_os(SETTINGS_ENV).is_some() {
            return;
        }
        let root = std::env::temp_dir().join(format!("rkqa-settings-{}", crate::util::new_id()));
        std::fs::create_dir_all(root.join(".cache/e2e")).unwrap();
        std::fs::create_dir_all(root.join(".cache/qa")).unwrap();
        assert!(Settings::discover(&root).unwrap().source().is_none());
        std::fs::write(root.join("rightkit-qa.settings.env"), "WHO=root\n").unwrap();
        assert_eq!(
            Settings::discover(&root).unwrap().get("WHO").as_deref(),
            Some("root")
        );
        std::fs::write(root.join(".cache/e2e/settings.env"), "WHO=e2e\n").unwrap();
        assert_eq!(
            Settings::discover(&root).unwrap().get("WHO").as_deref(),
            Some("e2e")
        );
        std::fs::write(root.join(".cache/qa/settings.env"), "WHO=qa\n").unwrap();
        let s = Settings::discover(&root).unwrap();
        assert_eq!(s.get("WHO").as_deref(), Some("qa"));
        assert_eq!(
            s.source(),
            Some(root.join(".cache/qa/settings.env").as_path())
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
