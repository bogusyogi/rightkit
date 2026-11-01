use serde::{Deserialize, Serialize};

/// A single tool invocation requested by the model. `arguments_json` is the raw,
/// untrusted JSON the model produced — it is parsed/validated at dispatch time, never trusted here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}
