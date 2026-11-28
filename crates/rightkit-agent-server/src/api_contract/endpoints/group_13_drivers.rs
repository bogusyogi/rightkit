use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_13_DRIVERS: [ApiEndpoint; 31] = [
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
    ApiEndpoint {
        method: "GET",
        path: "/cli-providers",
        summary: "List vendor CLI providers (Claude Code, Codex, ...) with install, version, transport and auth state",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cli-providers/{provider}/discovery",
        summary: "Discover one vendor CLI provider's executable, version and transport",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cli-providers/{provider}/auth-status",
        summary: "Read one vendor CLI provider's sign-in state",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cli-providers/{provider}/models",
        summary: "List models a vendor CLI provider offers",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cli-providers/{provider}/attestation",
        summary: "Read the content-addressed harness attestation for a vendor CLI provider",
    },
    ApiEndpoint {
        method: "POST",
        path: "/cli-providers/{provider}/login",
        summary: "Start the vendor CLI provider's own sign-in",
    },
    ApiEndpoint {
        method: "POST",
        path: "/cli-providers/{provider}/logout",
        summary: "Sign the vendor CLI provider out",
    },
    ApiEndpoint {
        method: "POST",
        path: "/cli-providers/{provider}/verify-harness",
        summary: "Re-verify the vendor CLI harness and refresh its attestation",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/cli-delegation-policy",
        summary: "Read which vendor CLIs a session may delegate to",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/sessions/{id}/cli-delegation-policy",
        summary: "Set which vendor CLIs a session may delegate to",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/agents/{agent_id}/runtime/open",
        summary: "Open a sub-agent's vendor runtime",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/agents/{agent_id}/runtime/prompt",
        summary: "Send a prompt to a sub-agent's vendor runtime",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/agents/{agent_id}/runtime/interrupt",
        summary: "Interrupt a sub-agent's vendor runtime turn",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/agents/{agent_id}/runtime/resume",
        summary: "Resume a sub-agent's vendor runtime",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/agents/{agent_id}/runtime/stop",
        summary: "Stop a sub-agent's vendor runtime",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/agents/{agent_id}/runtime/status",
        summary: "Read a sub-agent's vendor runtime status",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/events/replay",
        summary: "Page a session's durable events after a cursor (catch-up after a stream resync)",
    },
    ApiEndpoint {
        method: "POST",
        path: "/accounts/linked",
        summary: "Link an existing Claude profile folder as an account (nothing in the folder is copied or changed)",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/accounts/linked/{provider}/{label}",
        summary: "Remove a linked profile folder from the account list (the folder is never touched)",
    },
    ApiEndpoint {
        method: "GET",
        path: "/accounts/discover",
        summary: "Find existing Claude profile folders that can be linked, with their non-secret identity",
    },
];
