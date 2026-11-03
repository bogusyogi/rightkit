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
