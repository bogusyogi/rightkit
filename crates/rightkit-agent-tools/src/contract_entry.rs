//! Read-only CLI entrypoints. This does not authorize or dispatch product tools.
use std::io::{self, Write};

use crate::app_contract::AppContract;

/// Handle `contract`, `contract --json`, or `--help` before initializing an engine or store.
/// Both contract forms emit the same JSON declaration. `None` leaves an unrelated invocation
/// to the caller; malformed reserved invocations return a JSON usage error and exit code 2.
/// The declaration supplier is called only for a valid reserved invocation and must itself be
/// independent of engine and store initialization.
pub fn handle_read_only_entry(
    args: &[String],
    contract: impl FnOnce() -> Result<AppContract, String>,
    out: &mut dyn Write,
) -> Option<io::Result<i32>> {
    let first = args.first()?.as_str();
    if first != "contract" && first != "--help" {
        return None;
    }
    let valid = match first {
        "contract" => args.len() == 1 || (args.len() == 2 && args[1] == "--json"),
        "--help" => args.len() == 1,
        _ => false,
    };
    Some((|| {
        if !valid {
            writeln!(out, "{{\"error\":{{\"code\":\"usage\",\"message\":\"invalid reserved invocation\",\"retryable\":false}}}}")?;
            return Ok(2);
        }
        let declaration = match contract().and_then(|c| c.export().map(|json| (c, json))) {
            Ok(value) => value,
            Err(_) => {
                writeln!(out, "{{\"error\":{{\"code\":\"invalid_contract\",\"message\":\"contract declaration is invalid\",\"retryable\":false}}}}")?;
                return Ok(2);
            }
        };
        if first == "contract" {
            writeln!(out, "{}", declaration.1)?;
        } else {
            writeln!(
                out,
                "{}: contract [--json] | --help",
                declaration.0.contract
            )?;
            for tool in &declaration.0.tools {
                writeln!(out, "  {}  {}", tool.name, tool.summary)?;
            }
        }
        Ok(0)
    })())
}
