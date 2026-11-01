//! The `apple` command for `rightkit-engine-host` registries.

use std::sync::Arc;

use rightkit_engine_host::{CommandRegistry, CommandSpec, EngineError, Handler};
use serde_json::{json, Value};

use crate::gate::{Gate, GateError};

/// The command name.
pub const NAME: &str = "apple";

/// The registry entry: a long-running command (operations spawn processes and call APIs).
pub fn spec() -> CommandSpec {
    CommandSpec::new(NAME, "Apple development and App Store Connect operations")
        .positional(["operation"])
}

/// Register [`spec`] on `registry`.
pub fn register(registry: CommandRegistry) -> CommandRegistry {
    registry.register(spec())
}

/// Run `input` (`{operation, arguments?, policyContext?}`) through the gate.
pub fn run(gate: &dyn Gate, input: &Value) -> Result<Value, EngineError> {
    let mut request = input.clone();
    if request.get("operation").is_none() {
        request["operation"] = json!("catalog");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| EngineError::new("runtime", e.to_string()))?;
    runtime
        .block_on(crate::gate::invoke(gate, &request))
        .map(crate::cli::mark_validity)
        .map_err(|e| match e {
            GateError::Invalid(m) => EngineError::new("invalid", m),
            GateError::Denied(m) => EngineError::new("denied", m),
        })
}

/// A handler that answers [`NAME`] through `gate` and forwards every other command to `inner`.
pub fn wrap(gate: Arc<dyn Gate>, inner: Handler) -> Handler {
    Arc::new(
        move |name: &str, input: &Value, dry_run: bool| -> Result<Value, EngineError> {
            if name == NAME {
                run(gate.as_ref(), input)
            } else {
                inner(name, input, dry_run)
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{Call, Effect};

    struct Deny;

    impl Gate for Deny {
        fn authorize(&self, _: &Value, _: &Call<'_>) -> Result<Vec<Value>, String> {
            Err("denied".into())
        }
        fn validate_scope(&self, _: &Value, _: &[Effect]) -> Result<(), String> {
            Err("denied".into())
        }
    }

    #[test]
    fn catalog_needs_no_authorisation() {
        let value = run(&Deny, &json!({})).unwrap();
        assert_eq!(value["backend"], "rightkit-apple-ops");
    }

    #[test]
    fn execution_is_denied_without_the_gate() {
        let err = run(
            &Deny,
            &json!({"operation": "project.clean", "arguments": {"execute": true}}),
        );
        // Either the adapter rejects the dry plan or the gate denies; never a result.
        assert!(err.is_err());
    }

    #[test]
    fn spec_is_registered() {
        let registry = register(CommandRegistry::new(Arc::new(
            |_: &str, _: &Value, _: bool| -> Result<Value, EngineError> { Ok(Value::Null) },
        )));
        assert!(registry.contains(NAME));
    }
}
