use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_05: [ApiEndpoint; 39] = [
    ApiEndpoint {
        method: "GET",
        path: "/schedules/{id}/runs",
        summary: "List workspace schedule run history",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/schedules/{id}/time-zone",
        summary: "Update workspace schedule timezone",
    },
    ApiEndpoint {
        method: "POST",
        path: "/schedules/{id}/run",
        summary: "Run workspace schedule immediately",
    },
    ApiEndpoint {
        method: "POST",
        path: "/schedules/{id}/pause",
        summary: "Pause workspace schedule",
    },
    ApiEndpoint {
        method: "POST",
        path: "/schedules/{id}/resume",
        summary: "Resume workspace schedule",
    },
    ApiEndpoint {
        method: "GET",
        path: "/bots",
        summary: "List saved Bot profiles",
    },
    ApiEndpoint {
        method: "POST",
        path: "/bots",
        summary: "Create saved Bot profile",
    },
    ApiEndpoint {
        method: "POST",
        path: "/bots/import",
        summary: "Import authority-free Bot package",
    },
    ApiEndpoint {
        method: "GET",
        path: "/bots/{id}",
        summary: "Read saved Bot profile",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/bots/{id}",
        summary: "Update saved Bot profile",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/bots/{id}",
        summary: "Delete saved Bot profile & bound schedules",
    },
    ApiEndpoint {
        method: "POST",
        path: "/bots/{id}/run",
        summary: "Run saved Bot as isolated task",
    },
    ApiEndpoint {
        method: "GET",
        path: "/bots/{id}/export",
        summary: "Export authority-free Bot package",
    },
    ApiEndpoint {
        method: "GET",
        path: "/local-models/hardware",
        summary: "Read local hardware/runtime probe",
    },
    ApiEndpoint {
        method: "GET",
        path: "/local-models/catalog",
        summary: "Read local model cookbook catalog",
    },
    ApiEndpoint {
        method: "POST",
        path: "/local-models/probe",
        summary: "Probe OpenAI-compatible local endpoint",
    },
    ApiEndpoint {
        method: "POST",
        path: "/local-models/chat-smoke",
        summary: "Smoke-test OpenAI-compatible local chat completions",
    },
    ApiEndpoint {
        method: "POST",
        path: "/local-models/add-provider",
        summary: "Persist and apply local provider route config",
    },
    ApiEndpoint {
        method: "GET",
        path: "/hooks/status",
        summary: "Read redacted hook readiness diagnostics",
    },
    ApiEndpoint {
        method: "GET",
        path: "/hooks/list",
        summary:
            "List registered hooks with env-var id, event, kind, configured, and revoked markers",
    },
    ApiEndpoint {
        method: "POST",
        path: "/hooks/test",
        summary: "Dry-run matching hook against a synthetic event without mutating state",
    },
    ApiEndpoint {
        method: "POST",
        path: "/hooks/revoke",
        summary: "Revoke or restore a hook id (env var name) for the current daemon session",
    },
    ApiEndpoint {
        method: "GET",
        path: "/remote/channels",
        summary: "List remote channel gateway status",
    },
    ApiEndpoint {
        method: "POST",
        path: "/remote/channels/{id}/profile",
        summary: "Save remote channel gateway profile override",
    },
    ApiEndpoint {
        method: "POST",
        path: "/remote/channels/{id}/fanout",
        summary: "Fan out a remote channel message to bound sessions",
    },
    ApiEndpoint {
        method: "POST",
        path: "/remote/channels/{id}/outbound",
        summary: "Fan out an outbound remote channel delivery to targets",
    },
    ApiEndpoint {
        method: "GET",
        path: "/remote/telegram/status",
        summary: "Read Telegram remote status",
    },
    ApiEndpoint {
        method: "GET",
        path: "/remote/telegram/test",
        summary: "Run Telegram remote readiness checks",
    },
    ApiEndpoint {
        method: "GET",
        path: "/remote/telegram/users",
        summary: "Discover Telegram users from redacted updates",
    },
    ApiEndpoint {
        method: "POST",
        path: "/remote/bindings",
        summary: "Record a remote channel binding (SQL dual-write)",
    },
    ApiEndpoint {
        method: "GET",
        path: "/commands",
        summary: "List slash commands",
    },
    ApiEndpoint {
        method: "POST",
        path: "/commands/execute",
        summary: "Execute slash command",
    },
    ApiEndpoint {
        method: "GET",
        path: "/settings/orchestration",
        summary: "Load orchestration settings",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/settings/orchestration",
        summary: "Save orchestration settings",
    },
    ApiEndpoint {
        method: "GET",
        path: "/settings/config-status",
        summary: "Read settings config file freshness metadata",
    },
    ApiEndpoint {
        method: "POST",
        path: "/settings/reload",
        summary: "Reload orchestration settings from config files",
    },
    ApiEndpoint {
        method: "GET",
        path: "/settings/toolsets",
        summary: "List workspace custom toolsets",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/settings/toolsets/{id}",
        summary: "Save a workspace custom toolset",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/settings/toolsets/{id}",
        summary: "Delete a workspace custom toolset",
    },
];
