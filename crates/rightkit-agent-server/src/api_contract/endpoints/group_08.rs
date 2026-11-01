use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_08: [ApiEndpoint; 21] = [
    ApiEndpoint {
        method: "POST",
        path: "/skills/proposals/{id}/run-benchmark",
        summary: "Run a benchmark and record a skill proposal verdict",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills/training-runs",
        summary: "List skill training runs",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/training-runs",
        summary: "Create skill training run",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills/training-runs/{id}",
        summary: "Read skill training run",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/training-runs/{id}/resume",
        summary: "Resume skill training run",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/training-runs/{id}/execute",
        summary: "Execute skill training run epoch",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/training-runs/{id}/propose-candidate",
        summary: "Generate skill training run candidate proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/training-runs/{id}/cancel",
        summary: "Cancel skill training run",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills/training-runs/{id}/history",
        summary: "List skill training run history",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills/training-runs/{id}/rejected-edits",
        summary: "List skill training run rejected edits",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/{slug}/quarantine",
        summary: "Quarantine a managed skill",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/{slug}/activate",
        summary: "Activate a managed skill after benchmark gate",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/{slug}/retire",
        summary: "Retire a managed skill",
    },
    ApiEndpoint {
        method: "GET",
        path: "/mcp",
        summary: "List configured MCP servers",
    },
    ApiEndpoint {
        method: "GET",
        path: "/mcp/server/status",
        summary: "Read MCP server mode status",
    },
    ApiEndpoint {
        method: "POST",
        path: "/mcp/servers",
        summary: "Save an MCP server",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/mcp/servers/{name}",
        summary: "Delete an MCP server",
    },
    ApiEndpoint {
        method: "POST",
        path: "/mcp/servers/{name}/probe",
        summary: "Probe an MCP server",
    },
    ApiEndpoint {
        method: "POST",
        path: "/mcp/servers/{name}/login",
        summary: "Trigger OAuth login for an MCP server",
    },
    ApiEndpoint {
        method: "GET",
        path: "/lsp",
        summary: "Read workspace LSP status",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/lsp",
        summary: "Save workspace LSP configuration",
    },
];
