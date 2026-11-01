use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_01: [ApiEndpoint; 35] = [
    ApiEndpoint {
        method: "GET",
        path: "/health",
        summary: "Connectivity probe",
    },
    ApiEndpoint {
        method: "GET",
        path: "/health/diagnostics",
        summary: "Redacted daemon diagnostics",
    },
    ApiEndpoint {
        method: "GET",
        path: "/health/readiness",
        summary: "Read runtime readiness snapshot",
    },
    ApiEndpoint {
        method: "GET",
        path: "/machine/status",
        summary: "Read machine runtime status",
    },
    ApiEndpoint {
        method: "GET",
        path: "/machine/info",
        summary: "Read machine runtime info",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sandbox/settings",
        summary: "Read workspace core sandbox settings",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/sandbox/settings",
        summary: "Save workspace core sandbox settings",
    },
    ApiEndpoint {
        method: "POST",
        path: "/diagnostics/bundle",
        summary: "Create a redacted diagnostics support bundle",
    },
    ApiEndpoint {
        method: "GET",
        path: "/diagnostics/bundles/{id}",
        summary: "Download a redacted diagnostics support bundle",
    },
    ApiEndpoint {
        method: "GET",
        path: "/provider-keys/{provider}",
        summary: "Read provider key configuration status",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/provider-keys/{provider}",
        summary: "Save a provider API key",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/provider-keys/{provider}",
        summary: "Remove a stored provider API key",
    },
    ApiEndpoint {
        method: "GET",
        path: "/provider-auth/{provider}",
        summary: "Read provider auth status",
    },
    ApiEndpoint {
        method: "POST",
        path: "/provider-auth/{provider}/login",
        summary: "Start provider OAuth login",
    },
    ApiEndpoint {
        method: "POST",
        path: "/provider-auth/{provider}/login/{challenge}/submit",
        summary: "Submit provider OAuth callback or device code",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/provider-auth/{provider}",
        summary: "Log out provider auth",
    },
    ApiEndpoint {
        method: "GET",
        path: "/api/contract",
        summary: "Machine-readable daemon API contract",
    },
    ApiEndpoint {
        method: "GET",
        path: "/api/openapi.json",
        summary: "Generated OpenAPI document",
    },
    ApiEndpoint {
        method: "GET",
        path: "/notifications",
        summary: "List daemon notification outbox",
    },
    ApiEndpoint {
        method: "GET",
        path: "/instructions",
        summary: "Inspect layered workspace instructions",
    },
    ApiEndpoint {
        method: "GET",
        path: "/teams",
        summary: "List configured agent teams",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/teams/{name}",
        summary: "Delete a configured agent team",
    },
    ApiEndpoint {
        method: "GET",
        path: "/messages",
        summary: "List inter-session messages",
    },
    ApiEndpoint {
        method: "GET",
        path: "/memory",
        summary: "List durable memory entries",
    },
    ApiEndpoint {
        method: "POST",
        path: "/memory",
        summary: "Add a durable memory entry",
    },
    ApiEndpoint {
        method: "GET",
        path: "/workspace/trust",
        summary: "Read workspace trust decision",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/workspace/trust",
        summary: "Persist workspace trust decision",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/permission-mode",
        summary: "Set live session permission mode",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/permission-rules",
        summary: "List live session permission rules",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/permission-rules",
        summary: "Add live session permission rule",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/sessions/{id}/permission-rules/{index}",
        summary: "Delete live session permission rule",
    },
    ApiEndpoint {
        method: "GET",
        path: "/permission-rules",
        summary: "List persistent permission rules",
    },
    ApiEndpoint {
        method: "POST",
        path: "/permission-rules",
        summary: "Add persistent permission rule",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/permission-rules/{index}",
        summary: "Delete persistent permission rule",
    },
    ApiEndpoint {
        method: "GET",
        path: "/permission-rules/export",
        summary: "Export persistent permission rules",
    },
];
