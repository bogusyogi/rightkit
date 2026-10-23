//! Test fixture: a step plugin binary for the plugin round-trip test in `suite.rs` (ADR-0001 item 4,
//! ruling D-b). It speaks protocol v1 on stdio and nothing else; `cargo test` builds it next to the
//! test binary, and the test starts it as a real process, so it runs on Windows as well as Unix.
use rightkit_qa::plugin::{serve_stdio, PluginCx, StepOutput, StepPlugin};
use serde_json::{Map, Value};

struct Fixture;

impl StepPlugin for Fixture {
    fn name(&self) -> String {
        "step-plugin-fixture".into()
    }

    fn version(&self) -> String {
        "1".into()
    }

    fn kinds(&self) -> Vec<String> {
        vec!["fixture_echo".into()]
    }

    fn step(
        &mut self,
        _cx: &mut PluginCx<'_>,
        kind: &str,
        params: &Map<String, Value>,
        _vars: &Map<String, Value>,
    ) -> Result<StepOutput, String> {
        if kind != "fixture_echo" {
            return Err(format!("the fixture does not implement {kind}"));
        }
        Ok(StepOutput::value(Value::Object(params.clone()))
            .with_hold("fixture secret", "fixture-secret-value"))
    }
}

fn main() {
    if let Err(e) = serve_stdio(&mut Fixture) {
        eprintln!("step plugin fixture: {e}");
        std::process::exit(1);
    }
}
