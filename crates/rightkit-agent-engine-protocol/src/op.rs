use serde::{Deserialize, Serialize};

/// An input command from a thin driver (CLI or UI) into the runtime.
///
/// Drivers NEVER call the engine directly — they only submit Ops through
/// `SessionConduit`. Discriminated union; forward-compatible — later gates add
/// more Op kinds without changing this contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "$kind")]
pub enum Op {
    /// Send a new user turn into a session.
    #[serde(rename = "submitUserMessage", rename_all = "camelCase")]
    SubmitUserMessage {
        op_id: String,
        session_id: String,
        text: String,
    },

    /// Interrupt the active run for a session (cancel the in-flight model/tool loop).
    #[serde(rename = "interruptRun", rename_all = "camelCase")]
    InterruptRun { op_id: String, session_id: String },

    /// Manually compact the session's context now — summarize older turns behind a boundary.
    #[serde(rename = "requestCompaction", rename_all = "camelCase")]
    RequestCompaction { op_id: String, session_id: String },

    /// Inject a human steering instruction to be applied at the next turn boundary.
    #[serde(rename = "injectSteeringMessage", rename_all = "camelCase")]
    InjectSteeringMessage {
        op_id: String,
        session_id: String,
        content: String,
        injected_at: String,
        injected_by: String,
    },

    /// Resolve a pending tool-call approval (the P0.5 runtime pause).
    #[serde(rename = "resolveToolApproval", rename_all = "camelCase")]
    ResolveToolApproval {
        op_id: String,
        session_id: String,
        tool_call_id: String,
        approved: bool,
        reason: Option<String>,
    },
}
