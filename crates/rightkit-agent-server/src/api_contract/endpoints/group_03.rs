use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_03: [ApiEndpoint; 31] = [
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/goal/pause",
        summary: "Pause a persistent session goal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/goal/resume",
        summary: "Resume a persistent session goal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/goal/edit",
        summary: "Edit a persistent session goal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/rewind",
        summary: "Rewind the session conversation and/or workspace changes",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/rewind/points",
        summary: "List rewindable user prompts",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/clone",
        summary: "Clone a session transcript into an independent session",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/export",
        summary: "Export a session transcript as Markdown or HTML",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/orchestrate",
        summary: "Start a conductor orchestration run",
    },
    ApiEndpoint {
        method: "GET",
        path: "/tool-schemas",
        summary: "List registered tool schemas",
    },
    ApiEndpoint {
        method: "POST",
        path: "/tool-schemas/validate",
        summary: "Validate tool arguments",
    },
    ApiEndpoint {
        method: "GET",
        path: "/toolsets",
        summary: "List built-in toolset profiles",
    },
    ApiEndpoint {
        method: "GET",
        path: "/toolsets/gateways",
        summary: "List toolset gateway status",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/tool-invocations",
        summary: "List normalized tool invocation records",
    },
    ApiEndpoint {
        method: "GET",
        path: "/tool-invocations/{id}",
        summary: "Read normalized tool invocation record",
    },
    ApiEndpoint {
        method: "POST",
        path: "/tool-invocations/{id}/cancel",
        summary: "Cancel a pending tool invocation approval",
    },
    ApiEndpoint {
        method: "GET",
        path: "/tool-invocations/{id}/events",
        summary: "Read normalized tool invocation events",
    },
    ApiEndpoint {
        method: "GET",
        path: "/specs",
        summary: "List feature specs",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs",
        summary: "Create feature spec",
    },
    ApiEndpoint {
        method: "GET",
        path: "/specs/{id}",
        summary: "Read feature spec with tasks",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/specs/{id}",
        summary: "Patch feature spec metadata",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/clarify",
        summary: "Generate feature spec clarification questions",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/plan",
        summary: "Generate mapped feature spec implementation tasks",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/tasks",
        summary: "Add feature spec tasks",
    },
    ApiEndpoint {
        method: "GET",
        path: "/specs/{id}/constitution/decisions",
        summary: "List feature spec constitution decisions",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/constitution/decisions",
        summary: "Record feature spec constitution decision",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/checklist",
        summary: "Build feature spec readiness checklist",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/analyze",
        summary: "Run spec coverage analysis",
    },
    ApiEndpoint {
        method: "POST",
        path: "/specs/{id}/implement",
        summary: "Start a spec-bound implementation session",
    },
    ApiEndpoint {
        method: "GET",
        path: "/specs/{id}/trace",
        summary: "Read requirement/task/evidence trace",
    },
    ApiEndpoint {
        method: "GET",
        path: "/execution/backends",
        summary: "List execution backends",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/execution/backends/default",
        summary: "Set workspace default execution backend",
    },
];
