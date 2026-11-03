//! The cross-language settings manifest: one JSON list of every setting, the same document
//! `AppSettingsManifest.decode(json:)` reads in RightKitSwift, with generators for Swift, Rust
//! and TypeScript and the parity checks that keep the three honest.
//!
//! ```json
//! { "settings": [
//!   { "key": "theme", "kind": "string", "default": "dark", "protected": false, "label": "Theme" }
//! ] }
//! ```
//!
//! `kind` is `bool`, `int`, `double` or `string`; `default` must fit it; `protected` (default
//! false) marks a key only the owner path may change (a change request relayed through
//! [`crate::ChangeRequestQueue`] is refused for it); `label`, `section` and `note` are
//! documentation the generators carry and Swift ignores. An unknown field is an error, so a
//! typo cannot silently drop a setting. A key is an ASCII identifier (`[A-Za-z][A-Za-z0-9_]*`),
//! so it can name a constant in every target, and two keys whose generated names collide are
//! refused.
//!
//! Generators are pure functions of the manifest, so an app runs them from its own `build.rs`
//! or a release step and writes the text where it wants:
//! [`Manifest::generate`] with a [`Target`], and [`Manifest::is_current`] to fail a check when a
//! committed generated file is stale. Parity: [`Manifest::check_defaults`] compares the manifest
//! with the defaults of the app's own Rust settings type (`T::default()`), and
//! [`Manifest::check_protected`] with a hand-kept protected-key list (the one the app's owner
//! path enforces), so the Swift, Rust and TypeScript tables and the Rust store cannot drift.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The value type of a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Double,
    String,
}

impl Kind {
    /// The manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Double => "double",
            Kind::String => "string",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    key: String,
    kind: Kind,
    default: Value,
    #[serde(default)]
    protected: bool,
    label: Option<String>,
    section: Option<String>,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    settings: Vec<RawSetting>,
}

/// One declared setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub key: String,
    pub kind: Kind,
    /// A JSON bool, integer, number (always a float for `Double`) or string, fitting `kind`.
    pub default: Value,
    pub protected: bool,
    pub label: Option<String>,
    pub section: Option<String>,
    pub note: Option<String>,
}

/// What a generator writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// `GeneratedSettings` enum with `Key` constants and a `manifest()` for
    /// RightKitSwift's `AppSettingsManifest`.
    Swift,
    /// `pub mod keys`, `PROTECTED_KEYS` and `DEFAULTS_JSON`.
    Rust,
    /// `settingsManifest` (const) and the `SettingKey` type.
    TypeScript,
}

/// Why a manifest was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is not valid JSON of the expected shape: {0}")]
    Json(String),
    #[error("setting key {0:?} is not an ASCII identifier ([A-Za-z][A-Za-z0-9_]*)")]
    BadKey(String),
    #[error("setting key {0:?} is listed twice")]
    DuplicateKey(String),
    #[error("keys {0:?} and {1:?} generate the same name in a target")]
    NameCollision(String, String),
    #[error("the default of {key:?} is not a {kind}")]
    BadDefault { key: String, kind: &'static str },
    #[error("the default of {0:?} is outside what a generated file can carry")]
    Unrepresentable(String),
}

/// A validated manifest, in file order.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    settings: Vec<Setting>,
    source: String,
}

/// One difference found by a parity check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// The manifest declares the key; the Rust type does not have it.
    MissingInRust(String),
    /// The Rust type has the key; the manifest does not declare it.
    MissingInManifest(String),
    /// The Rust default is not a value of the declared kind.
    KindDiffers { key: String, declared: Kind },
    /// The Rust default differs from the manifest default.
    DefaultDiffers {
        key: String,
        manifest: String,
        rust: String,
    },
    /// Protected in one list, not in the other.
    ProtectedDiffers { key: String, in_manifest: bool },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::MissingInRust(k) => write!(
                f,
                "{k}: declared in the manifest, absent from the Rust settings type"
            ),
            Mismatch::MissingInManifest(k) => write!(
                f,
                "{k}: in the Rust settings type, absent from the manifest"
            ),
            Mismatch::KindDiffers { key, declared } => {
                write!(f, "{key}: the Rust default is not a {}", declared.as_str())
            }
            Mismatch::DefaultDiffers {
                key,
                manifest,
                rust,
            } => {
                write!(f, "{key}: manifest default {manifest}, Rust default {rust}")
            }
            Mismatch::ProtectedDiffers { key, in_manifest } => write!(
                f,
                "{key}: protected in the {} only",
                if *in_manifest {
                    "manifest"
                } else {
                    "Rust list"
                },
            ),
        }
    }
}

fn identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `fooBarBaz` -> `FOO_BAR_BAZ` (a run of capitals stays one word: `URLBase` -> `URL_BASE`).
fn upper_snake(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 && chars[i - 1] != '_' {
            let prev_lower = chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit();
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev_lower || next_lower {
                out.push('_');
            }
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

fn fits(kind: Kind, value: &Value) -> bool {
    match kind {
        Kind::Bool => value.is_boolean(),
        Kind::Int => value.is_i64(),
        Kind::Double => value.is_number(),
        Kind::String => value.is_string(),
    }
}

impl Manifest {
    /// Parse and validate the JSON text. `source` names the file in generated headers.
    pub fn parse(json: &str, source: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest =
            serde_json::from_str(json).map_err(|e| ManifestError::Json(e.to_string()))?;
        let mut settings = Vec::with_capacity(raw.settings.len());
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for r in raw.settings {
            if !identifier(&r.key) {
                return Err(ManifestError::BadKey(r.key));
            }
            if settings.iter().any(|s: &Setting| s.key == r.key) {
                return Err(ManifestError::DuplicateKey(r.key));
            }
            if !fits(r.kind, &r.default) {
                return Err(ManifestError::BadDefault {
                    key: r.key,
                    kind: r.kind.as_str(),
                });
            }
            // A double default is carried as a float whatever way it was written.
            let default = match (r.kind, r.default.as_f64()) {
                (Kind::Double, Some(f)) => serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .ok_or_else(|| ManifestError::Unrepresentable(r.key.clone()))?,
                _ => r.default,
            };
            if let Value::String(text) = &default {
                if text.contains("\"####") || text.chars().any(|c| c == '\0') {
                    return Err(ManifestError::Unrepresentable(r.key));
                }
            }
            let generated = upper_snake(&r.key);
            if let Some(other) = seen.insert(generated, r.key.clone()) {
                return Err(ManifestError::NameCollision(other, r.key));
            }
            settings.push(Setting {
                key: r.key,
                kind: r.kind,
                default,
                protected: r.protected,
                label: r.label,
                section: r.section,
                note: r.note,
            });
        }
        Ok(Self {
            settings,
            source: source.to_owned(),
        })
    }

    pub fn settings(&self) -> &[Setting] {
        &self.settings
    }

    pub fn setting(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// The keys marked protected, in file order.
    pub fn protected_keys(&self) -> Vec<&str> {
        self.settings
            .iter()
            .filter(|s| s.protected)
            .map(|s| s.key.as_str())
            .collect()
    }

    /// The defaults as one JSON object (what a store of defaults would serialize to).
    pub fn defaults_object(&self) -> Map<String, Value> {
        self.settings
            .iter()
            .map(|s| (s.key.clone(), s.default.clone()))
            .collect()
    }

    /// Generated source text for `target`. The text is deterministic and ends with a newline.
    pub fn generate(&self, target: Target) -> String {
        match target {
            Target::Swift => self.swift(),
            Target::Rust => self.rust(),
            Target::TypeScript => self.typescript(),
        }
    }

    /// True when `existing` is what [`Manifest::generate`] writes now (line endings ignored),
    /// so a release step can refuse a stale committed file.
    pub fn is_current(&self, target: Target, existing: &str) -> bool {
        existing.replace("\r\n", "\n") == self.generate(target)
    }

    fn header(&self, comment: &str) -> String {
        format!(
            "{comment} Generated by rightkit-app-settings from {}. Do not edit; change the manifest and regenerate.\n",
            self.source
        )
    }

    fn swift(&self) -> String {
        let mut out = self.header("//");
        out.push_str(
            "\nimport RightKitStore\n\npublic enum GeneratedSettings {\n    public enum Key {\n",
        );
        for s in &self.settings {
            let _ = writeln!(
                out,
                "        public static let `{}` = {}",
                s.key,
                swift_string(&s.key)
            );
        }
        out.push_str("    }\n\n    public static func manifest() throws -> AppSettingsManifest {\n        try AppSettingsManifest([\n");
        for s in &self.settings {
            let _ = writeln!(
                out,
                "            AppSettingDefinition(key: {}, kind: .{}, defaultValue: {}, isProtected: {}),",
                swift_string(&s.key),
                s.kind.as_str(),
                swift_value(s),
                s.protected
            );
        }
        out.push_str("        ])\n    }\n}\n");
        out
    }

    fn rust(&self) -> String {
        let mut out = self.header("//");
        out.push_str("\npub mod keys {\n");
        for s in &self.settings {
            let _ = writeln!(
                out,
                "    pub const {}: &str = {};",
                upper_snake(&s.key),
                json_string(&s.key)
            );
        }
        out.push_str(
            "}\n\n/// Keys only the owner path may change.\npub const PROTECTED_KEYS: &[&str] = &[",
        );
        let protected: Vec<String> = self
            .protected_keys()
            .iter()
            .map(|k| json_string(k))
            .collect();
        out.push_str(&protected.join(", "));
        out.push_str(
            "];\n\n/// Every default as one JSON object.\npub const DEFAULTS_JSON: &str = r####\"",
        );
        out.push_str(&Value::Object(self.defaults_object()).to_string());
        out.push_str("\"####;\n");
        out
    }

    fn typescript(&self) -> String {
        let mut out = self.header("//");
        out.push_str("\nexport const settingsManifest = [\n");
        for s in &self.settings {
            let _ = writeln!(
                out,
                "  {{ key: {}, kind: {}, default: {}, protected: {} }},",
                json_string(&s.key),
                json_string(s.kind.as_str()),
                s.default,
                s.protected
            );
        }
        out.push_str(
            "] as const;\n\nexport type SettingKey = (typeof settingsManifest)[number][\"key\"];\n",
        );
        out
    }

    /// Compare the manifest with the defaults of the app's Rust settings type: the same key set,
    /// the same kind and the same default value. An empty result means parity.
    pub fn check_defaults<T: Serialize + DeserializeOwned + Default>(&self) -> Vec<Mismatch> {
        let rust = match serde_json::to_value(T::default()) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let mut out = Vec::new();
        for s in &self.settings {
            match rust.get(&s.key) {
                None => out.push(Mismatch::MissingInRust(s.key.clone())),
                Some(v) if !fits(s.kind, v) => out.push(Mismatch::KindDiffers {
                    key: s.key.clone(),
                    declared: s.kind,
                }),
                Some(v) => {
                    let same = match s.kind {
                        Kind::Double => v.as_f64() == s.default.as_f64(),
                        _ => *v == s.default,
                    };
                    if !same {
                        out.push(Mismatch::DefaultDiffers {
                            key: s.key.clone(),
                            manifest: s.default.to_string(),
                            rust: v.to_string(),
                        });
                    }
                }
            }
        }
        for key in rust.keys() {
            if self.setting(key).is_none() {
                out.push(Mismatch::MissingInManifest(key.clone()));
            }
        }
        out
    }

    /// Compare the manifest's protected keys with the list the app's owner path enforces.
    pub fn check_protected(&self, rust_list: &[&str]) -> Vec<Mismatch> {
        let mut out = Vec::new();
        for key in self.protected_keys() {
            if !rust_list.contains(&key) {
                out.push(Mismatch::ProtectedDiffers {
                    key: key.to_owned(),
                    in_manifest: true,
                });
            }
        }
        for key in rust_list {
            if !self.setting(key).is_some_and(|s| s.protected) {
                out.push(Mismatch::ProtectedDiffers {
                    key: (*key).to_owned(),
                    in_manifest: false,
                });
            }
        }
        out
    }
}

/// A JSON string literal, which is also a valid Rust and TypeScript string literal for the
/// manifest's accepted text (serde_json escapes only `"`, `\` and control characters).
fn json_string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

fn swift_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn swift_value(s: &Setting) -> String {
    match (&s.kind, &s.default) {
        (Kind::Bool, Value::Bool(b)) => format!(".bool({b})"),
        (Kind::Int, v) => format!(".int({v})"),
        (Kind::Double, v) => format!(".double({v})"),
        (Kind::String, Value::String(t)) => format!(".string({})", swift_string(t)),
        _ => ".null".to_owned(),
    }
}
