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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{COMPILE_SDK, MIN_SDK, NDK_VERSION, SUPPORTED_ABIS};

    #[test]
    fn parser_skips_comments_and_splits_on_first_equals() {
        let m = parse_versions("# c\n\nA=1\n B = x=y \n");
        assert_eq!(m.get("A").map(String::as_str), Some("1"));
        assert_eq!(m.get("B").map(String::as_str), Some("x=y"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn the_pin_file_agrees_with_the_constants() {
        assert_eq!(pinned("NDK_VERSION").as_deref(), Some(NDK_VERSION));
        assert_eq!(pinned("MIN_SDK"), Some(MIN_SDK.to_string()));
        assert_eq!(pinned("PLATFORM_API"), Some(COMPILE_SDK.to_string()));
        let abis = pinned("ABIS").unwrap();
        let names: Vec<&str> = abis.split_whitespace().collect();
        let expect: Vec<&str> = SUPPORTED_ABIS.iter().map(|a| a.android_name()).collect();
        assert_eq!(names, expect);
        let targets = pinned("RUST_TARGETS").unwrap();
        let triples: Vec<&str> = targets.split_whitespace().collect();
        let expect: Vec<&str> = SUPPORTED_ABIS.iter().map(|a| a.rust_triple()).collect();
        assert_eq!(triples, expect);
    }

    #[test]
    fn toolchain_doc_names_every_pin() {
        let doc = include_str!("../TOOLCHAIN.md");
        for key in ["NDK_VERSION", "CARGO_NDK_VERSION", "BUILD_TOOLS_VERSION"] {
            let value = pinned(key).unwrap();
            assert!(
                doc.contains(&value),
                "TOOLCHAIN.md does not mention {key}={value}"
            );
        }
    }

    #[test]
    fn every_pin_is_set() {
        for key in [
            "NDK_VERSION",
            "NDK_RELEASE",
            "CARGO_NDK_VERSION",
            "PLATFORM_API",
            "BUILD_TOOLS_VERSION",
            "MIN_SDK",
            "ABIS",
            "RUST_TARGETS",
            "JDK_MAJOR",
        ] {
            assert!(
                pinned(key).is_some_and(|v| !v.is_empty()),
                "{key} is not pinned"
            );
        }
    }
}
