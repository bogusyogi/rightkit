//! `rightkit-contracts`: validate one document, list contracts, or check a
//! whole fixture tree. Exit codes: 0 success, 1 validation or fixture failure,
//! 2 usage error.

use std::path::PathBuf;
use std::process::ExitCode;

use rightkit_contracts::fixtures::check_tree;
use rightkit_contracts::{check_typed, Contract};

const USAGE: &str = "usage:
  rightkit-contracts validate <contract> <file>
  rightkit-contracts check-fixtures <fixtures-dir>
  rightkit-contracts list";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] => {
            for contract in Contract::ALL {
                println!("{} {}", contract.name(), contract.supported_version());
            }
            ExitCode::SUCCESS
        }
        ["validate", name, file] => validate(name, file),
        ["check-fixtures", root] => check_fixtures(PathBuf::from(root)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn validate(name: &str, file: &str) -> ExitCode {
    let Some(contract) = Contract::from_name(name) else {
        eprintln!("unknown contract `{name}`; run `rightkit-contracts list`");
        return ExitCode::from(2);
    };
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("cannot read {file}: {error}");
            return ExitCode::from(1);
        }
    };
    match check_typed(contract, &text) {
        Ok(()) => {
            println!(
                "ok {} {} {file}",
                contract.name(),
                contract.supported_version()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("invalid {file}\n{error}");
            ExitCode::from(1)
        }
    }
}

fn check_fixtures(root: PathBuf) -> ExitCode {
    match check_tree(&root) {
        Ok(report) => {
            for (category, count) in &report.checked {
                println!("{category}: {count}");
            }
            for failure in &report.failures {
                eprintln!("FAIL {failure}");
            }
            println!(
                "total {} fixtures, {} failures",
                report.total(),
                report.failures.len()
            );
            if report.is_green() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("cannot read fixtures under {}: {error}", root.display());
            ExitCode::from(1)
        }
    }
}
