//! The test executables a `cargo test --no-run --message-format=json` compile produced.
//!
//! Only `compiler-artifact` records of the test profile that name an executable count: that is every
//! unit-test binary (`lib`, `bin`), every integration test (`[[test]]`) and every bench or example that
//! cargo itself would run under `cargo test`. Build scripts, proc-macros, libraries and plain example
//! binaries are never test executables. The result is sorted, so the same compile always yields the same
//! run order whatever order cargo finished its units in.
use super::lines::for_each_line;
use crate::journeys::build_record::package_name_of;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::BufRead;
use std::path::PathBuf;

/// Longest listing line read. `compiler-artifact` records are far shorter; an enormous
/// `compiler-message` is not needed and is skipped.
const MAX_LINE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TestBinary {
    /// The cargo package (`CARGO_PKG_NAME`).
    pub package: String,
    /// The cargo target name (`lib`-style unit tests carry the crate's name).
    pub target: String,
    /// The target's kinds (`lib`, `bin`, `test`, `bench`, ...), as cargo prints them.
    pub kind: Vec<String>,
    pub executable: PathBuf,
    /// The package directory: where cargo runs the binary from. Empty when the record names none.
    pub manifest_dir: PathBuf,
}

impl TestBinary {
    /// `package::target`, the name a summary line and a log file use.
    pub fn label(&self) -> String {
        format!("{}::{}", self.package, self.target)
    }
}

/// What the listing said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub binaries: Vec<TestBinary>,
    /// `Some(true)` when every `build-finished` record said success, `Some(false)` when one said
    /// otherwise, `None` when the listing has none (a compile that was cut short).
    pub build_finished: Option<bool>,
    /// `compiler-artifact` records read, test or not.
    pub artifacts: usize,
}

/// Read a cargo JSON listing. Lines that are not JSON objects (a broker banner, a stray progress line)
/// and reasons this module does not use are ignored.
pub fn parse_listing<R: BufRead>(reader: R) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for_each_line(reader, MAX_LINE, |line| {
        let line = line.trim_start();
        if !line.starts_with('{') {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match v["reason"].as_str() {
            Some("build-finished") => {
                let ok = v["success"].as_bool().unwrap_or(false);
                listing.build_finished = Some(listing.build_finished.unwrap_or(true) && ok);
            }
            Some("compiler-artifact") => {
                listing.artifacts += 1;
                if v["profile"]["test"].as_bool() != Some(true) {
                    return;
                }
                let Some(exe) = v["executable"].as_str().filter(|e| !e.is_empty()) else {
                    return;
                };
                let executable = PathBuf::from(exe);
                if !seen.insert(executable.clone()) {
                    return;
                }
                let manifest_dir = v["manifest_path"]
                    .as_str()
                    .map(PathBuf::from)
                    .and_then(|m| m.parent().map(PathBuf::from))
                    .unwrap_or_default();
                let target = v["target"]["name"].as_str().unwrap_or("").to_string();
                let kind = v["target"]["kind"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|k| k.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let package = v["package_id"]
                    .as_str()
                    .and_then(package_name_of)
                    .unwrap_or_else(|| target.clone());
                listing.binaries.push(TestBinary {
                    package,
                    target,
                    kind,
                    executable,
                    manifest_dir,
                });
            }
            _ => {}
        }
    })?;
    listing.binaries.sort();
    Ok(listing)
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! A recorded cargo listing, shaped like what `cargo test --no-run --message-format=json` prints for
    //! a package with a library, a binary, two integration tests, an example and a build script.
    pub const LISTING: &str = r##"
{"reason":"compiler-message","package_id":"path+file:///w/crates/fx#fx@0.1.0","message":{"rendered":"warning: unused"}}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["custom-build"],"name":"build-script-build"},"profile":{"test":false},"executable":null,"fresh":true}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["lib"],"name":"fx"},"profile":{"test":false},"executable":null,"fresh":true}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["lib"],"name":"fx"},"profile":{"test":true},"executable":"/t/debug/deps/fx-aaaa","fresh":false}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["bin"],"name":"fx-cli"},"profile":{"test":true},"executable":"/t/debug/deps/fx_cli-bbbb","fresh":false}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["bin"],"name":"fx-cli"},"profile":{"test":false},"executable":"/t/debug/fx-cli","fresh":false}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["example"],"name":"demo"},"profile":{"test":false},"executable":"/t/debug/examples/demo","fresh":false}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["test"],"name":"ui"},"profile":{"test":true},"executable":"/t/debug/deps/ui-cccc","fresh":false}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/fx#fx@0.1.0","manifest_path":"/w/crates/fx/Cargo.toml","target":{"kind":["test"],"name":"ui"},"profile":{"test":true},"executable":"/t/debug/deps/ui-cccc","fresh":true}
{"reason":"compiler-artifact","package_id":"fx-e2e 0.2.0 (path+file:///w/e2e)","manifest_path":"/w/e2e/Cargo.toml","target":{"kind":["test"],"name":"scenarios"},"profile":{"test":true},"executable":"/t/debug/deps/scenarios-dddd","fresh":false}
this line is a broker banner, not json
{"reason":"build-finished","success":true}
"##;
}

#[cfg(test)]
mod tests {
    use super::fixtures::LISTING;
    use super::*;

    fn parse(text: &str) -> Listing {
        parse_listing(text.as_bytes()).unwrap()
    }

    #[test]
    fn takes_only_test_profile_artifacts_with_an_executable() {
        let l = parse(LISTING);
        let got: Vec<(String, String, Vec<String>, String)> = l
            .binaries
            .iter()
            .map(|b| {
                (
                    b.package.clone(),
                    b.target.clone(),
                    b.kind.clone(),
                    b.executable.display().to_string(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "fx".into(),
                    "fx".into(),
                    vec!["lib".into()],
                    "/t/debug/deps/fx-aaaa".into()
                ),
                (
                    "fx".into(),
                    "fx-cli".into(),
                    vec!["bin".into()],
                    "/t/debug/deps/fx_cli-bbbb".into()
                ),
                (
                    "fx".into(),
                    "ui".into(),
                    vec!["test".into()],
                    "/t/debug/deps/ui-cccc".into()
                ),
                (
                    "fx-e2e".into(),
                    "scenarios".into(),
                    vec!["test".into()],
                    "/t/debug/deps/scenarios-dddd".into()
                ),
            ]
        );
        // The normal bin, the plain example, the lib build and the build script are not tests.
        assert!(!l
            .binaries
            .iter()
            .any(|b| b.executable.ends_with("fx-cli") || b.executable.ends_with("demo")));
    }

    #[test]
    fn the_package_directory_is_the_manifest_directory() {
        let l = parse(LISTING);
        let ui = l.binaries.iter().find(|b| b.target == "ui").unwrap();
        assert_eq!(ui.manifest_dir, PathBuf::from("/w/crates/fx"));
        let e2e = l.binaries.iter().find(|b| b.target == "scenarios").unwrap();
        assert_eq!(e2e.manifest_dir, PathBuf::from("/w/e2e"));
    }

    #[test]
    fn a_repeated_executable_is_listed_once_and_the_order_is_stable() {
        let l = parse(LISTING);
        assert_eq!(
            l.binaries.iter().filter(|b| b.target == "ui").count(),
            1,
            "the fresh duplicate of ui is the same executable"
        );
        // Reversing the listing's lines changes nothing: the result is sorted.
        let reversed: String = LISTING.lines().rev().map(|l| format!("{l}\n")).collect();
        assert_eq!(parse(&reversed).binaries, l.binaries);
    }

    #[test]
    fn build_finished_is_the_conjunction_of_every_record() {
        assert_eq!(parse(LISTING).build_finished, Some(true));
        assert_eq!(parse(LISTING).artifacts, 9);
        assert_eq!(parse("").build_finished, None);
        let mixed = "{\"reason\":\"build-finished\",\"success\":true}\n{\"reason\":\"build-finished\",\"success\":false}\n";
        assert_eq!(parse(mixed).build_finished, Some(false));
        assert_eq!(
            parse("{\"reason\":\"build-finished\"}\n").build_finished,
            Some(false),
            "a build-finished record that does not say success is not one"
        );
    }

    #[test]
    fn noise_and_unknown_reasons_are_ignored() {
        let l = parse("not json\n{\"reason\":\"build-script-executed\"}\n{broken\n\n");
        assert!(l.binaries.is_empty());
        assert_eq!(l.artifacts, 0);
        assert_eq!(l.build_finished, None);
    }

    #[test]
    fn labels_name_the_package_and_target() {
        let l = parse(LISTING);
        assert_eq!(l.binaries[0].label(), "fx::fx");
    }
}
