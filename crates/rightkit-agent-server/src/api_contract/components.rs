fn api_contract_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ApiEndpoint".to_string(),
        json!({
            "type": "object",
            "required": ["method", "path", "summary"],
            "properties": {
                "method": {"type": "string"},
                "path": {"type": "string"},
                "summary": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ApiContractResponse".to_string(),
        json!({
            "type": "object",
            "required": ["name", "version", "endpoints"],
            "properties": {
                "name": {"type": "string", "const": crate::host_identity::prefixed("daemon")},
                "version": {"type": "integer"},
                "endpoints": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ApiEndpoint"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert("OpenApiDocument".to_string(), json!({"type": "object"}));
    schemas.insert("ProtocolEvent".to_string(), protocol_event_schema());
    schemas
}

/// Every `$kind` a `ProtocolEvent` can carry (crates/protocol `protocol_event.rs`; the test below
/// keeps this list equal to the enum's `serde(rename)` names).
const PROTOCOL_EVENT_KINDS: [&str; 57] = [
    "sessionStarted",
    "sessionTitleUpdated",
    "assistantDelta",
    "assistantMessageCompleted",
    "toolCallStarted",
    "toolApprovalRequested",
    "toolApprovalResolved",
    "toolApprovalReviewed",
    "toolPolicyEvent",
    "toolResultAdded",
    "sessionTurnFinished",
    "streamResyncRequired",
    "persistenceFailed",
    "overloaded",
    "providerRetrying",
    "sessionCompactionStarted",
    "verificationFinished",
    "taskGraphUpdated",
    "taskStatusUpdated",
    "taskArtifactUpdated",
    "artifactLineageUpdated",
    "runInterrupted",
    "runFailed",
    "sessionCompacted",
    "sessionContextFolded",
    "usageUpdated",
    "turnUsage",
    "promptCacheBreak",
    "toolOutputDelta",
    "toolArgumentsPartial",
    "backgroundShellExited",
    "budgetExceeded",
    "providerRouteUpdated",
    "accountRateLimited",
    "accountUsageReported",
    "dispatchDecided",
    "orchestrationObservationRecorded",
    "agentQueryIssued",
    "agentQueryResponded",
    "humanSteeringInjected",
    "subAgentSpawned",
    "subAgentOutput",
    "subAgentCompleted",
    "subAgentCancelled",
    "userMessageSubmitted",
    "userQuestionAsked",
    "mcpElicitationRequested",
    "credentialRequested",
    "credentialResolved",
    "userQuestionAnswered",
    "planPresented",
    "agentRosterUpdated",
    "checkRunCompleted",
    "goalProgressUpdated",
    "selfReviewProposalReceived",
    "modelRouteDecision",
    "runGovernanceRejected",
];

/// The event union one `data:` frame of `GET /sessions/{id}/events` holds. Every event carries
/// the four envelope fields; the rest depends on `$kind` and is open.
fn protocol_event_schema() -> Value {
    json!({
        "type": "object",
        "description": "One session event. `$kind` names the variant; `seq` is the durable cursor (a live-only projection event has none to replay).",
        "required": ["$kind", "sessionId", "seq", "createdAt"],
        "properties": {
            "$kind": {"type": "string", "enum": PROTOCOL_EVENT_KINDS.to_vec()},
            "sessionId": {"type": "string"},
            "seq": {"type": "integer", "format": "int64"},
            "createdAt": {"type": "string"}
        },
        "discriminator": {"propertyName": "$kind"},
        "additionalProperties": true
    })
}

fn api_contract_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ApiContractResponse"})
}

fn openapi_document_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/OpenApiDocument"})
}
