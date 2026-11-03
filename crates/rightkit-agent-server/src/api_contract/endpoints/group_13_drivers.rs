use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_13_DRIVERS: [ApiEndpoint; 11] = [
    ApiEndpoint {
        method: "GET",
        path: "/drivers/{kind}/discovery",
        summary: "Discover an installed external driver, authentication state, version, schema fingerprint, and capability transport",
    },
    ApiEndpoint {
        method: "GET",
        path: "/drivers/{kind}/models",
        summary: "List the selectable model catalogue for a driver kind (Codex: live model/list; Claude: static marker)",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver-capabilities/leases",
        summary: "Issue a session-bound driver capability lease",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/driver-capabilities/tools",
        summary: "List tools allowed by a driver capability lease",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver-capabilities/call",
        summary: "Execute a governed driver capability",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver/start",
        summary: "Start the session driver process",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver/prompt",
        summary: "Send a prompt to the session driver",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver/interrupt",
        summary: "Interrupt the active driver turn",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver/resume",
        summary: "Resume a persisted driver session",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/driver/stop",
        summary: "Stop the session driver process",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/driver/status",
        summary: "Read session driver bindings and runtime status",
    },
];
