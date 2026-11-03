//! Rust runtime for the shared `@rightkit/i18n` catalogs.
//!
//! Loads the JSON catalog files the TypeScript package loads, resolves locale tags the way its
//! `normalizeLocale` does, merges a locale over the English catalog the way its `mergeCatalog`
//! does, and looks keys up the way its `t` does: the locale's message, then the English message,
//! then the caller's fallback, then the key itself.
//!
//! A catalog is a JSON object. Its messages are strings; a function message exists only in the
//! TypeScript package and is not represented here.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;

use serde_json::{Map, Value};

/// A catalog: a JSON object of messages, nested objects allowed.
pub type Catalog = Map<String, Value>;

/// One entry of a language picker, as `LocaleInfo` in `@rightkit/i18n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleInfo {
    pub id: String,
    pub name: String,
    pub english_name: String,
    /// Extra tags that resolve to `id`, such as `zh-TW` for `zh-hant`.
    pub aliases: Vec<String>,
}

/// Why a catalog could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I18nError {
    /// The catalog text is not valid JSON.
    Parse(String),
    /// The catalog JSON is valid but not an object.
    NotAnObject,
}

impl fmt::Display for I18nError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(reason) => write!(f, "catalog is not valid JSON: {reason}"),
            Self::NotAnObject => f.write_str("catalog is not a JSON object"),
        }
    }
}

impl std::error::Error for I18nError {}

/// Lower-cases a locale tag and maps `_` to `-` (the TypeScript `normalizeTag`).
pub fn normalize_tag(value: &str) -> String {
    value.trim().replace('_', "-").to_lowercase()
}

/// `mergeCatalog`: `override_value` merged over `base`, recursively. Objects merge key by key;
/// any other override value replaces the base value. An override that is not an object leaves
/// `base` unchanged.
pub fn merge_catalog(base: &Catalog, override_value: &Value) -> Catalog {
    match override_value {
        Value::Object(over) => merge_objects(base, over),
        _ => base.clone(),
    }
}

fn merge_objects(left: &Catalog, right: &Catalog) -> Catalog {
    let mut result = left.clone();
    for (key, value) in right {
        let merged = match (result.get(key), value) {
            (Some(Value::Object(l)), Value::Object(r)) => Value::Object(merge_objects(l, r)),
            _ => value.clone(),
        };
        result.insert(key.clone(), merged);
    }
    result
}

/// `lookupMessage`: an exact top-level key first (a source string may contain dots), else a
/// dotted path into the catalog. A value that is not a string counts as missing.
fn lookup_message<'a>(catalog: &'a Catalog, key: &str) -> Option<&'a str> {
    if let Some(direct) = catalog.get(key) {
        return direct.as_str();
    }
    let mut parts = key.split('.');
    let mut node = catalog.get(parts.next()?)?;
    for part in parts {
        node = node.as_object()?.get(part)?;
    }
    node.as_str()
}

/// The runtime for one app: the English catalog, the loaded locale catalogs and the known
/// locale tags. `createI18n` in the TypeScript package.
#[derive(Debug, Clone)]
pub struct I18n {
    default_locale: String,
    english: Catalog,
    catalogs: HashMap<String, Catalog>,
    /// Lower-cased ids and aliases mapped to the canonical id.
    known: HashMap<String, String>,
}

impl I18n {
    /// Creates the runtime. `locales` is the language picker list; `None` leaves the default
    /// locale as the only known tag until [`Self::register_locale`] adds one.
    pub fn new(english: Catalog, default_locale: &str, locales: Option<&[LocaleInfo]>) -> Self {
        let mut known = HashMap::new();
        known.insert(normalize_tag(default_locale), default_locale.to_string());
        for info in locales.unwrap_or(&[]) {
            known.insert(normalize_tag(&info.id), info.id.clone());
            for alias in &info.aliases {
                known.insert(normalize_tag(alias), info.id.clone());
            }
        }
        let mut catalogs = HashMap::new();
        catalogs.insert(default_locale.to_string(), english.clone());
        Self {
            default_locale: default_locale.to_string(),
            english,
            catalogs,
            known,
        }
    }

    /// The default locale tag.
    pub fn default_locale(&self) -> &str {
        &self.default_locale
    }

    /// Makes `id` a known locale tag, as the TypeScript package does for loader keys when no
    /// `locales` list is given.
    pub fn register_locale(&mut self, id: &str) {
        self.known.insert(normalize_tag(id), id.to_string());
    }

    /// Loads a locale's catalog from its JSON text (`loadLocale`), replacing an earlier one.
    /// Only a JSON object is accepted.
    pub fn load_locale(&mut self, locale: &str, json: &str) -> Result<(), I18nError> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| I18nError::Parse(e.to_string()))?;
        match value {
            Value::Object(catalog) => {
                self.catalogs.insert(locale.to_string(), catalog);
                Ok(())
            }
            _ => Err(I18nError::NotAnObject),
        }
    }

    /// Whether a locale's catalog is loaded. The default locale always is.
    pub fn is_loaded(&self, locale: &str) -> bool {
        self.catalogs.contains_key(locale)
    }

    /// The catalog to show for `locale`: the English catalog merged with the locale's own
    /// catalog when one is loaded. An unloaded locale gives the English catalog.
    pub fn catalog(&self, locale: &str) -> Catalog {
        match self.catalogs.get(locale) {
            Some(own) => merge_objects(&self.english, own),
            None => self.english.clone(),
        }
    }

    /// `normalizeLocale`: the exact id or alias first, then the longest matching prefix
    /// (`zh-CN` resolves to `zh`, `zh-hant-TW` to `zh-hant`). `None` when nothing matches.
    pub fn normalize_locale(&self, value: &str) -> Option<String> {
        let normalized = normalize_tag(value);
        let parts: Vec<&str> = normalized.split('-').filter(|p| !p.is_empty()).collect();
        for n in (1..=parts.len()).rev() {
            if let Some(hit) = self.known.get(&parts[..n].join("-")) {
                return Some(hit.clone());
            }
        }
        None
    }

    /// `t(locale, key, { fallback })`, synchronous and loading nothing: the locale's message
    /// for `key`, else the English message, else `fallback`, else `key` itself. An unloaded
    /// locale resolves like the English catalog.
    pub fn t(&self, locale: &str, key: &str, fallback: Option<&str>) -> String {
        self.catalogs
            .get(locale)
            .and_then(|own| lookup_message(own, key))
            .or_else(|| lookup_message(&self.english, key))
            .or(fallback)
            .unwrap_or(key)
            .to_string()
    }
}
