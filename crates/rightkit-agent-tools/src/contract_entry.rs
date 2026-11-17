//! Read-only CLI entrypoints. This does not authorize or dispatch product tools.
use std::io::{self, Write};

use crate::app_contract::AppContract;

/// Handle `contract` or `contract --json` before initializing an engine or store.
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
    if first != "contract" {
        return None;
    }
    let valid = args.len() == 1 || (args.len() == 2 && args[1] == "--json");
    Some((|| {
        if !valid {
            writeln!(out, "{{\"error\":{{\"code\":\"usage\",\"message\":\"invalid reserved invocation\",\"retryable\":false}}}}")?;
            return Ok(2);
        }
        let declaration = match contract().and_then(|c| c.export()) {
            Ok(value) => value,
            Err(_) => {
                writeln!(out, "{{\"error\":{{\"code\":\"invalid_contract\",\"message\":\"contract declaration is invalid\",\"retryable\":false}}}}")?;
                return Ok(2);
            }
        };
        out.write_all(declaration.as_bytes())?;
        Ok(0)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_contract::{
        APP_CONTRACT_MAX_BYTES, APP_CONTRACT_SCHEMA_ID, APP_CONTRACT_VERSION,
    };
    use serde_json::json;

    #[test]
    fn maximum_export_round_trips_without_an_extra_byte() {
        let mut contract = AppContract {
            contract: "app-cli".into(),
            contract_version: APP_CONTRACT_VERSION,
            schema: APP_CONTRACT_SCHEMA_ID.into(),
            app: "app".into(),
            app_version: "1".into(),
            bundle_id: "app.example".into(),
            platforms: vec!["windows".into()],
            exit_codes: json!({}),
            conventions: json!({"padding": ""}),
            tools: vec![],
        };
        let size = contract.export().unwrap().len();
        contract.conventions["padding"] = json!("x".repeat(APP_CONTRACT_MAX_BYTES - size));
        let mut output = Vec::new();
        assert_eq!(
            handle_read_only_entry(
                &["contract".into(), "--json".into()],
                || Ok(contract),
                &mut output
            )
            .unwrap()
            .unwrap(),
            0
        );
        assert_eq!(output.len(), APP_CONTRACT_MAX_BYTES);
        assert!(AppContract::parse(std::str::from_utf8(&output).unwrap()).is_ok());
    }

    #[test]
    fn help_is_left_to_the_app() {
        let mut output = Vec::new();
        assert!(handle_read_only_entry(
            &["--help".into()],
            || panic!("supplier called"),
            &mut output
        )
        .is_none());
    }
}
