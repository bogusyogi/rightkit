//! Runs every fixture under `rightkit/contracts/fixtures/` through the
//! library and through the `rightkit-contracts` binary.

#![cfg(feature = "schema")]

use std::path::PathBuf;
use std::process::Command;

use rightkit_contracts::fixtures::{check_tree, CATEGORIES};
use rightkit_contracts::Contract;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures")
}

#[test]
fn every_fixture_meets_its_expectation() {
    let report = check_tree(&fixtures_root()).expect("fixture tree readable");
    assert!(
        report.is_green(),
        "fixture failures:\n{}",
        report.failures.join("\n")
    );
}

#[test]
fn every_category_is_exercised_for_every_contract() {
    let root = fixtures_root();
    for contract in Contract::ALL {
        for category in CATEGORIES {
            let directory = root.join(contract.name()).join(category);
            let count = std::fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json")
                })
                .count();
            assert!(count > 0, "{} has no fixtures", directory.display());
        }
    }
}

#[test]
fn cli_validates_each_valid_fixture() {
    let root = fixtures_root();
    for contract in Contract::ALL {
        let directory = root.join(contract.name()).join("valid");
        for entry in std::fs::read_dir(&directory).expect("valid directory") {
            let path = entry.expect("entry").path();
            let output = Command::new(env!("CARGO_BIN_EXE_rightkit-contracts"))
                .args(["validate", contract.name()])
                .arg(&path)
                .output()
                .expect("binary runs");
            assert!(
                output.status.success(),
                "{} rejected: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn cli_refuses_incompatible_major_with_exit_one() {
    let path = fixtures_root().join("record-event/version-rejected/event-v2.json");
    let output = Command::new(env!("CARGO_BIN_EXE_rightkit-contracts"))
        .args(["validate", "record-event"])
        .arg(&path)
        .output()
        .expect("binary runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("incompatible major version"),
        "stderr: {stderr}"
    );
}

#[test]
fn cli_check_fixtures_reports_green_tree() {
    let output = Command::new(env!("CARGO_BIN_EXE_rightkit-contracts"))
        .args(["check-fixtures"])
        .arg(fixtures_root())
        .output()
        .expect("binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("0 failures"), "stdout: {stdout}");
}

#[test]
fn cli_usage_errors_exit_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_rightkit-contracts"))
        .args(["validate", "no-such-contract", "x.json"])
        .output()
        .expect("binary runs");
    assert_eq!(output.status.code(), Some(2));
}
