//! Command registration shared by the CLI and the sidecar, with the contract export.
//!
//! An engine registers every named command once ([`CommandSpec`]) together with one handler
//! ([`Handler`]). The CLI maps words to a command name ([`CommandRegistry::from_words`]), the
//! sidecar serves the same names, and [`CommandRegistry::contract`] /
//! [`CommandRegistry::typescript`] export the surface for shells and generated clients.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::wire::EngineError;

/// Runs one named command: `(name, input, dry_run)`.
pub type Handler = Arc<dyn Fn(&str, &Value, bool) -> Result<Value, EngineError> + Send + Sync>;

/// One registered command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Dotted command name (`job.status`).
    pub name: String,
    /// One-line summary for help and the contract.
    pub summary: String,
    /// Runs on its own thread in the sidecar (anything that is not a quick local read).
    pub long: bool,
    /// Input keys the CLI fills from positional words after the command words, in order
    /// (`job status <job_id>`).
    pub positional: Vec<String>,
}

impl CommandSpec {
    /// A long-running command with no positional keys.
    pub fn new(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            summary: summary.into(),
            long: true,
            positional: Vec::new(),
        }
    }

    /// Mark the command quick: the sidecar runs it inline on the reader thread.
    pub fn quick(mut self) -> Self {
        self.long = false;
        self
    }

    /// Name the input keys positional words fill.
    pub fn positional<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.positional = keys.into_iter().map(Into::into).collect();
        self
    }
}

/// Every command an engine serves and the one handler that runs them.
#[derive(Clone)]
pub struct CommandRegistry {
    specs: Vec<CommandSpec>,
    handler: Handler,
}

impl std::fmt::Debug for CommandRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandRegistry")
            .field("specs", &self.specs)
            .finish_non_exhaustive()
    }
}

impl CommandRegistry {
    /// A registry whose commands all run through `handler`.
    pub fn new(handler: Handler) -> Self {
        Self {
            specs: Vec::new(),
            handler,
        }
    }

    /// Register a command. A second registration of the same name replaces the first.
    pub fn register(mut self, spec: CommandSpec) -> Self {
        self.specs.retain(|s| s.name != spec.name);
        self.specs.push(spec);
        self
    }

    /// The registered commands, in registration order.
    pub fn specs(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// The registered command names.
    pub fn names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }

    /// The spec of `name`.
    pub fn get(&self, name: &str) -> Option<&CommandSpec> {
        self.specs.iter().find(|s| s.name == name)
    }

    /// Whether `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Run `name`; an unregistered name is `unknown_command`.
    pub fn dispatch(&self, name: &str, input: &Value, dry_run: bool) -> Result<Value, EngineError> {
        if !self.contains(name) {
            return Err(EngineError::new(
                "unknown_command",
                format!("unknown command: {name}"),
            ));
        }
        (self.handler)(name, input, dry_run)
    }

    /// Map CLI words to a command: the longest run of leading words whose dotted form (`-`
    /// becomes `_`) is a registered name wins; the words after it fill the spec's positional keys
    /// in order. Extra words are a usage error. `input` that is not an object becomes `{}`.
    pub fn from_words(
        &self,
        words: &[String],
        input: Value,
    ) -> Result<(String, Value), EngineError> {
        if words.is_empty() {
            return Err(EngineError::new("usage", "no command given"));
        }
        let mut input = if input.is_object() { input } else { json!({}) };
        for take in (1..=words.len()).rev() {
            let name = words[..take]
                .iter()
                .map(|w| w.replace('-', "_"))
                .collect::<Vec<_>>()
                .join(".");
            let Some(spec) = self.get(&name) else {
                continue;
            };
            let rest = &words[take..];
            if rest.len() > spec.positional.len() {
                return Err(EngineError::new(
                    "usage",
                    format!("too many arguments for {name}: {}", rest.join(" ")),
                ));
            }
            for (key, value) in spec.positional.iter().zip(rest) {
                input[key.as_str()] = Value::String(value.clone());
            }
            return Ok((name, input));
        }
        Err(EngineError::new(
            "unknown_command",
            format!("unknown command: {}", words.join(" ")),
        ))
    }

    /// The machine-readable contract: engine name and version, the contract version and every
    /// command with its summary, positional keys and threading.
    pub fn contract(&self, engine: &str, version: &str, contract_version: u32) -> Value {
        json!({
            "engine": engine,
            "version": version,
            "contract_version": contract_version,
            "commands": self.specs.iter().map(|s| json!({
                "name": s.name,
                "summary": s.summary,
                "long": s.long,
                "positional": s.positional,
            })).collect::<Vec<_>>(),
        })
    }

    /// A TypeScript module naming every command: a string-literal union type `type_name` and a
    /// `readonly` array constant of the same names, sorted.
    pub fn typescript(&self, type_name: &str) -> String {
        let names: BTreeSet<&str> = self.specs.iter().map(|s| s.name.as_str()).collect();
        let quoted: Vec<String> = names
            .iter()
            .map(|n| serde_json::to_string(n).unwrap_or_default())
            .collect();
        let mut out = String::from("// Generated by rightkit-engine-host. Do not edit.\n");
        if quoted.is_empty() {
            out.push_str(&format!("export type {type_name} = never;\n"));
        } else {
            out.push_str(&format!(
                "export type {type_name} =\n  | {};\n",
                quoted.join("\n  | ")
            ));
        }
        out.push_str(&format!(
            "export const {type_name}List: readonly {type_name}[] = [{}];\n",
            quoted.join(", ")
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> CommandRegistry {
        CommandRegistry::new(Arc::new(
            |name: &str, input: &Value, dry: bool| -> Result<Value, EngineError> {
                Ok(json!({"name": name, "input": input, "dry": dry}))
            },
        ))
        .register(
            CommandSpec::new("job.status", "Job status")
                .quick()
                .positional(["job_id"]),
        )
        .register(CommandSpec::new("library.rebuild_index", "Rebuild"))
        .register(CommandSpec::new("doctor", "Health"))
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn words_map_to_the_longest_registered_name() {
        let r = registry();
        let (name, input) = r
            .from_words(&words(&["job", "status", "j1"]), json!(null))
            .unwrap();
        assert_eq!(name, "job.status");
        assert_eq!(input["job_id"], "j1");
        let (name, _) = r
            .from_words(&words(&["library", "rebuild-index"]), json!({}))
            .unwrap();
        assert_eq!(name, "library.rebuild_index");
        assert_eq!(
            r.from_words(&words(&["job", "status", "a", "b"]), json!({}))
                .unwrap_err()
                .code,
            "usage"
        );
        assert_eq!(
            r.from_words(&words(&["nope"]), json!({})).unwrap_err().code,
            "unknown_command"
        );
    }

    #[test]
    fn dispatch_refuses_unknown_names() {
        let r = registry();
        assert_eq!(r.dispatch("doctor", &json!({}), true).unwrap()["dry"], true);
        assert_eq!(
            r.dispatch("x", &json!({}), false).unwrap_err().code,
            "unknown_command"
        );
    }

    #[test]
    fn contract_and_typescript_list_every_command() {
        let r = registry();
        let c = r.contract("demo-engine", "1.0.0", 2);
        assert_eq!(c["commands"].as_array().unwrap().len(), 3);
        let ts = r.typescript("EngineCommand");
        assert!(ts.contains("| \"job.status\""));
        assert!(ts.contains("export const EngineCommandList"));
    }
}
