//! The pinned toolchain, read from `scripts/toolchain.versions` (the one file both bootstrap
//! scripts and the build scripts read).

use std::collections::BTreeMap;

/// The raw pin file.
pub const VERSIONS_FILE: &str = include_str!("../scripts/toolchain.versions");

/// Parses `KEY=value` lines; blank lines and `#` comments are skipped; the first `=` splits.
pub fn parse_versions(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

/// A pinned value, e.g. `pinned("NDK_VERSION")`.
pub fn pinned(key: &str) -> Option<String> {
    parse_versions(VERSIONS_FILE).remove(key)
}
