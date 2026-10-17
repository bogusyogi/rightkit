//! The fixture contract shared by the test suite and the `check-fixtures` CLI
//! command. Each contract directory under the fixtures root has four
//! categories, and each category has one expectation:
//!
//! * `valid/` validates against the schema and deserialises into the typed struct.
//! * `invalid/` fails JSON Schema validation.
//! * `semantic-invalid/` passes the schema and fails a cross-field rule.
//! * `version-rejected/` is refused by version negotiation before any schema work.
//!
//! Running `check-fixtures` on Mac, XPS and Hetzner against the same tree is the
//! "three installed engines validate the same fixtures" acceptance.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::{check_typed, validate_str, Contract, ContractError};

/// The four fixture categories and the directory name each uses.
pub const CATEGORIES: [&str; 4] = ["valid", "invalid", "semantic-invalid", "version-rejected"];

/// Outcome of checking a fixture tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixtureReport {
    /// Number of fixture files checked, per category.
    pub checked: BTreeMap<&'static str, usize>,
    /// One line per failed expectation, naming the file.
    pub failures: Vec<String>,
}

impl FixtureReport {
    /// Total number of fixture files checked.
    pub fn total(&self) -> usize {
        self.checked.values().sum()
    }

    /// True when every fixture met its category's expectation.
    pub fn is_green(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Checks every fixture under `root`. Each contract must have every category
/// with at least one file, so an empty category cannot pass silently.
pub fn check_tree(root: &Path) -> io::Result<FixtureReport> {
    let mut report = FixtureReport::default();
    for category in CATEGORIES {
        report.checked.insert(category, 0);
    }
    for contract in Contract::ALL {
        for category in CATEGORIES {
            if category == "semantic-invalid" && !contract.has_semantic_rules() {
                continue;
            }
            let directory = root.join(contract.name()).join(category);
            let mut files: Vec<_> = match fs::read_dir(&directory) {
                Ok(entries) => entries
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(|entry| entry.path())
                    .collect(),
                Err(error) => {
                    report.failures.push(format!(
                        "{}: missing fixture category `{category}`: {error}",
                        directory.display()
                    ));
                    continue;
                }
            };
            files.retain(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            });
            files.sort();
            if files.is_empty() {
                report.failures.push(format!(
                    "{}: fixture category `{category}` has no .json files",
                    directory.display()
                ));
            }
            for file in files {
                let text = fs::read_to_string(&file)?;
                *report.checked.entry(category).or_insert(0) += 1;
                if let Err(failure) = expect(contract, category, &text) {
                    report
                        .failures
                        .push(format!("{}: {failure}", file.display()));
                }
            }
        }
    }
    Ok(report)
}

/// Applies one category's expectation to one fixture's text.
pub fn expect(contract: Contract, category: &str, text: &str) -> Result<(), String> {
    match category {
        "valid" => {
            check_typed(contract, text).map_err(|error| format!("expected valid, got: {error}"))
        }
        "invalid" => match validate_str(contract, text) {
            Err(ContractError::Schema { .. }) => Ok(()),
            Err(other) => Err(format!("expected a schema failure, got: {other}")),
            Ok(_) => Err("expected a schema failure, but the fixture validated".into()),
        },
        "semantic-invalid" => match validate_str(contract, text) {
            Err(ContractError::Semantic { .. }) => Ok(()),
            Err(other) => Err(format!("expected a semantic failure, got: {other}")),
            Ok(_) => Err("expected a semantic failure, but the fixture validated".into()),
        },
        "version-rejected" => match validate_str(contract, text) {
            Err(error) if error.is_version_refusal() => Ok(()),
            Err(other) => Err(format!("expected a version refusal, got: {other}")),
            Ok(_) => Err("expected a version refusal, but the fixture validated".into()),
        },
        other => Err(format!("unknown fixture category `{other}`")),
    }
}
