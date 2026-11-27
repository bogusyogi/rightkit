//! Process-wide command-line config flags: `-c key=value`, `--ignore-user-config`, `--strict-config`
//! and `--ignore-rules`.
//!
//! The CLI sets them once at startup, before anything loads configuration. The loader
//! ([`crate::ConfigLoader::load_files_for_workspace_granted`], the one place every layered load
//! goes through) reads them, so a flag reaches every subcommand, the terminal UI and the in-process
//! host alike. They are not persisted and never reach a daemon that is already running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::HostConfig;

static IGNORE_USER_CONFIG: AtomicBool = AtomicBool::new(false);
static IGNORE_RULES: AtomicBool = AtomicBool::new(false);
static STRICT_CONFIG: AtomicBool = AtomicBool::new(false);
static OVERRIDES: Mutex<Vec<(String, toml::Value)>> = Mutex::new(Vec::new());

/// `--ignore-user-config`: do not read `<state root>/config.toml`.
pub fn set_ignore_user_config(on: bool) {
    IGNORE_USER_CONFIG.store(on, Ordering::SeqCst);
}

pub fn ignore_user_config() -> bool {
    IGNORE_USER_CONFIG.load(Ordering::SeqCst)
}

/// `--ignore-rules`: load no instruction files (`CLAUDE.md`, `AGENTS.md`, `right.md` and the rule
/// folders) into the prompt. Permission rules are a safety layer and are never skipped.
pub fn set_ignore_rules(on: bool) {
    IGNORE_RULES.store(on, Ordering::SeqCst);
}

pub fn ignore_rules() -> bool {
    IGNORE_RULES.load(Ordering::SeqCst)
}

/// `--strict-config`: a key no setting reads is an error, not a note.
pub fn set_strict_config(on: bool) {
    STRICT_CONFIG.store(on, Ordering::SeqCst);
}

pub fn strict_config() -> bool {
    STRICT_CONFIG.load(Ordering::SeqCst)
}

/// Split `key.path=value` into the dotted key and the value. The value is TOML (`"text"`, `3`,
/// `true`, `[1, 2]`); text that is not valid TOML is taken as a plain string, so
/// `-c model.default-model=gpt-x` works without quotes.
pub fn parse_override(pair: &str) -> Result<(String, toml::Value), String> {
    let Some((key, raw)) = pair.split_once('=') else {
        return Err(format!("`{pair}` is not key=value"));
    };
    let key = key.trim();
    let valid_key = !key.is_empty()
        && key.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    if !valid_key {
        return Err(format!(
            "`{key}` is not a config key (dotted names such as `model.small-model`)"
        ));
    }
    let raw = raw.trim();
    let value = toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut table| table.remove("v"))
        .unwrap_or_else(|| toml::Value::String(raw.to_string()));
    Ok((key.to_string(), value))
}

/// The TOML text of the `-c` pairs: one document, later pairs winning over earlier ones.
fn overrides_text(pairs: &[(String, toml::Value)]) -> String {
    let mut root = toml::Table::new();
    for (key, value) in pairs {
        let mut table = &mut root;
        let mut parts = key.split('.').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                table.insert(part.to_string(), value.clone());
            } else {
                let entry = table
                    .entry(part.to_string())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if !entry.is_table() {
                    *entry = toml::Value::Table(toml::Table::new());
                }
                table = entry.as_table_mut().expect("table");
            }
        }
    }
    toml::to_string(&root).unwrap_or_default()
}

/// Validate and install the `-c key=value` pairs. A pair that is not `key=value`, or whose value
/// does not fit the setting's type, is an error naming it (the CLI exits 2).
pub fn set_overrides(pairs: &[String]) -> Result<(), String> {
    let mut parsed = Vec::new();
    for pair in pairs {
        parsed.push(parse_override(pair).map_err(|e| format!("-c: {e}"))?);
    }
    // Each pair alone must fit its setting: the error then names the pair, not the whole document.
    for (index, entry) in parsed.iter().enumerate() {
        let text = overrides_text(std::slice::from_ref(entry));
        toml::from_str::<HostConfig>(&text)
            .map_err(|e| format!("-c {}: {}", pairs[index], e.message()))?;
    }
    *OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = parsed;
    Ok(())
}

/// Add one override from code (not from a `-c` pair): `--search` sets `tools.eager-tools` this way.
/// A key already given with `-c` is replaced, so an explicit `-c` list and `--search` do not both
/// apply to the same key.
pub fn add_override(key: &str, value: toml::Value) {
    let mut guard = OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.retain(|(existing, _)| existing != key);
    guard.push((key.to_string(), value));
}

/// The `-c` layer as TOML text, or `None` when no `-c` was given.
pub(crate) fn overrides_layer_text() -> Option<String> {
    let guard = OVERRIDES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (!guard.is_empty()).then(|| overrides_text(&guard))
}
