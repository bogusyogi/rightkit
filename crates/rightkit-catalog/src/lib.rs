//! One catalog source, three generated tables, one completeness check (rightkit#960).
//!
//! # Source format (decided once)
//!
//! A catalog folder holds one JSON file per locale, named `<locale>.json` (`en.json`, `de.json`,
//! `zh-hant.json`). Each file is a JSON object whose leaves are strings; nested objects are keys joined
//! by `.` (`{"menu":{"open":"Open"}}` is the key `menu.open`). `en.json` is the reference.
//!
//! PO and Fluent were not chosen. The JSON files are what `@rightkit/i18n` already loads and what
//! `rightkit-i18n` already reads, so the source needs no converter and no second format; plurals and
//! contexts are expressed as separate keys (`files.one`, `files.other`) that the completeness check
//! compares like any other. A placeholder is `{name}`; the check requires a locale to use exactly the
//! placeholders its English string uses.
//!
//! # Outputs
//!
//! [`generate`] emits a TypeScript module, a Rust module or a Swift file with every locale's flattened
//! table, sorted by key so the same source gives the same bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// `locale id -> key -> message`
pub type Catalogs = BTreeMap<String, BTreeMap<String, String>>;

pub const REFERENCE_LOCALE: &str = "en";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    TypeScript,
    Rust,
    Swift,
}

impl Target {
    pub fn parse(name: &str) -> Option<Target> {
        match name {
            "ts" | "typescript" => Some(Target::TypeScript),
            "rust" | "rs" => Some(Target::Rust),
            "swift" => Some(Target::Swift),
            _ => None,
        }
    }
}

/// One problem found by [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// The key is in English but not in the locale (or is an empty string there).
    Missing { locale: String, key: String },
    /// The key is in the locale but not in English.
    Extra { locale: String, key: String },
    /// The locale's message uses other placeholders than English does.
    Placeholders {
        locale: String,
        key: String,
        english: Vec<String>,
        found: Vec<String>,
    },
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Finding::Missing { locale, key } => write!(f, "{locale}: missing {key}"),
            Finding::Extra { locale, key } => {
                write!(f, "{locale}: {key} is not in {REFERENCE_LOCALE}")
            }
            Finding::Placeholders {
                locale,
                key,
                english,
                found,
            } => {
                write!(
                    f,
                    "{locale}: {key} uses {found:?}, {REFERENCE_LOCALE} uses {english:?}"
                )
            }
        }
    }
}

fn flatten(
    prefix: &str,
    value: &serde_json::Value,
    out: &mut BTreeMap<String, String>,
    file: &str,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(text) => {
            if prefix.is_empty() {
                return Err(format!("{file}: the top level must be an object"));
            }
            out.insert(prefix.to_owned(), text.clone());
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                if key.is_empty() || key.contains('.') {
                    return Err(format!("{file}: key {key:?} must be non-empty and contain no '.'"));
                }
                let full = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                flatten(&full, inner, out, file)?;
            }
            Ok(())
        }
        _ => Err(format!("{file}: {prefix}: only strings and objects are allowed (function messages exist only in TypeScript)")),
    }
}

/// Parse one locale's JSON text into a flat key map.
pub fn parse_catalog(file: &str, json: &str) -> Result<BTreeMap<String, String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("{file}: {e}"))?;
    if !value.is_object() {
        return Err(format!("{file}: the top level must be an object"));
    }
    let mut out = BTreeMap::new();
    flatten("", &value, &mut out, file)?;
    Ok(out)
}

/// Read every `<locale>.json` in `dir`. Fails when `en.json` is absent.
pub fn load_dir(dir: &Path) -> Result<Catalogs, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut catalogs = Catalogs::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(locale) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        catalogs.insert(locale, parse_catalog(&path.display().to_string(), &text)?);
    }
    if !catalogs.contains_key(REFERENCE_LOCALE) {
        return Err(format!(
            "{}: {REFERENCE_LOCALE}.json is required (the reference catalog)",
            dir.display()
        ));
    }
    Ok(catalogs)
}

/// The `{name}` placeholders of a message, sorted and unique. `{{` and `}}` are literal braces.
pub fn placeholders(message: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let chars: Vec<char> = message.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' if chars.get(i + 1) == Some(&'{') => i += 2,
            '}' if chars.get(i + 1) == Some(&'}') => i += 2,
            '{' => {
                let mut j = i + 1;
                let mut name = String::new();
                while j < chars.len() && chars[j] != '}' && chars[j] != ',' {
                    name.push(chars[j]);
                    j += 1;
                }
                let name = name.trim();
                if !name.is_empty() {
                    out.insert(name.to_owned());
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    out.into_iter().collect()
}

/// Completeness of every locale against English: missing or empty keys, keys English lacks, and
/// placeholder differences. Empty result means complete.
pub fn check(catalogs: &Catalogs) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Some(english) = catalogs.get(REFERENCE_LOCALE) else {
        return findings;
    };
    for (locale, table) in catalogs {
        if locale == REFERENCE_LOCALE {
            continue;
        }
        for (key, reference) in english {
            match table.get(key).filter(|m| !m.trim().is_empty()) {
                None => findings.push(Finding::Missing {
                    locale: locale.clone(),
                    key: key.clone(),
                }),
                Some(message) => {
                    let (want, got) = (placeholders(reference), placeholders(message));
                    if want != got {
                        findings.push(Finding::Placeholders {
                            locale: locale.clone(),
                            key: key.clone(),
                            english: want,
                            found: got,
                        });
                    }
                }
            }
        }
        for key in table.keys().filter(|k| !english.contains_key(*k)) {
            findings.push(Finding::Extra {
                locale: locale.clone(),
                key: key.clone(),
            });
        }
    }
    findings
}

fn ident(locale: &str) -> String {
    locale
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn swift_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                let _ = write!(out, "\\u{{{:X}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn ts_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

const HEADER: &str = "Generated by rightkit-catalog from the catalog folder; edit the JSON catalogs, then run `rightkit-catalog generate`.";

/// Emit every locale's table for `target`. Deterministic.
pub fn generate(catalogs: &Catalogs, target: Target) -> String {
    let mut out = String::new();
    match target {
        Target::TypeScript => {
            let _ = writeln!(out, "// {HEADER}");
            out.push_str("export const messages = {\n");
            for (locale, table) in catalogs {
                let _ = writeln!(out, "  {}: {{", ts_string(locale));
                for (key, message) in table {
                    let _ = writeln!(out, "    {}: {},", ts_string(key), ts_string(message));
                }
                out.push_str("  },\n");
            }
            out.push_str("} as const;\n\n");
            out.push_str("export type Locale = keyof typeof messages;\n");
            let _ = writeln!(
                out,
                "export type MessageKey = keyof (typeof messages)[{}];",
                ts_string(REFERENCE_LOCALE)
            );
        }
        Target::Rust => {
            let _ = writeln!(out, "// {HEADER}");
            out.push_str("pub type Table = &'static [(&'static str, &'static str)];\n\n");
            for (locale, table) in catalogs {
                let _ = writeln!(out, "pub static MESSAGES_{}: Table = &[", ident(locale));
                for (key, message) in table {
                    let _ = writeln!(out, "    ({key:?}, {message:?}),");
                }
                out.push_str("];\n\n");
            }
            out.push_str("/// Every locale id in the catalog folder, sorted.\npub static LOCALES: &[&str] = &[");
            out.push_str(
                &catalogs
                    .keys()
                    .map(|l| format!("{l:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push_str("];\n\n");
            out.push_str("/// The table of `locale` (exact id), or `None`.\npub fn messages(locale: &str) -> Option<Table> {\n    match locale {\n");
            for locale in catalogs.keys() {
                let _ = writeln!(
                    out,
                    "        {locale:?} => Some(MESSAGES_{}),",
                    ident(locale)
                );
            }
            out.push_str("        _ => None,\n    }\n}\n");
        }
        Target::Swift => {
            let _ = writeln!(out, "// {HEADER}");
            out.push_str("public enum Catalog {\n");
            out.push_str("    /// Every locale id in the catalog folder, sorted.\n");
            let _ = writeln!(
                out,
                "    public static let locales: [String] = [{}]",
                catalogs
                    .keys()
                    .map(|l| swift_string(l))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            out.push('\n');
            for (locale, table) in catalogs {
                let _ = writeln!(
                    out,
                    "    private static let messages_{}: [String: String] = [",
                    ident(locale)
                );
                if table.is_empty() {
                    out.push_str("        :\n");
                }
                for (key, message) in table {
                    let _ = writeln!(
                        out,
                        "        {}: {},",
                        swift_string(key),
                        swift_string(message)
                    );
                }
                out.push_str("    ]\n\n");
            }
            out.push_str("    /// Every locale's table by locale id.\n    public static let messages: [String: [String: String]] = [\n");
            for locale in catalogs.keys() {
                let _ = writeln!(
                    out,
                    "        {}: messages_{},",
                    swift_string(locale),
                    ident(locale)
                );
            }
            out.push_str("    ]\n}\n");
        }
    }
    out
}
