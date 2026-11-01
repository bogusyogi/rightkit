use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_07: [ApiEndpoint; 27] = [
    ApiEndpoint {
        method: "GET",
        path: "/runtime/rollouts",
        summary: "List runtime model rollouts",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/finetuning",
        summary: "List runtime fine-tuning readiness",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/compare",
        summary: "Run a blind runtime comparison",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/compare/judge",
        summary: "Judge a blind runtime comparison",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/middleware",
        summary: "List runtime middleware registry",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/runtime/middleware/{id}/enabled",
        summary: "Enable or disable runtime middleware",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/spans",
        summary: "List local runtime spans",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/spans/{id}",
        summary: "Read local runtime span",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/observations",
        summary: "List local runtime observations",
    },
    ApiEndpoint {
        method: "GET",
        path: "/plugins",
        summary: "List plugins",
    },
    ApiEndpoint {
        method: "POST",
        path: "/plugins/reload",
        summary: "Reload plugins",
    },
    ApiEndpoint {
        method: "POST",
        path: "/plugins/install",
        summary: "Install a local plugin",
    },
    ApiEndpoint {
        method: "GET",
        path: "/plugins/marketplace",
        summary: "List plugin marketplace entries",
    },
    ApiEndpoint {
        method: "POST",
        path: "/plugins/marketplace/sync",
        summary: "Sync the plugin marketplace catalog",
    },
    ApiEndpoint {
        method: "POST",
        path: "/plugins/marketplace/{id}/install",
        summary: "Install a plugin from the marketplace",
    },
    ApiEndpoint {
        method: "POST",
        path: "/plugins/{id}/enable",
        summary: "Enable a plugin",
    },
    ApiEndpoint {
        method: "POST",
        path: "/plugins/{id}/disable",
        summary: "Disable a plugin",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills",
        summary: "List managed skills",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/install",
        summary: "Install a managed skill",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/provenance-review",
        summary: "Review external skill pack provenance",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills/{slug}",
        summary: "Read a managed skill",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/skills/{slug}",
        summary: "Save a managed skill",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/skills/{slug}",
        summary: "Delete a managed skill",
    },
    ApiEndpoint {
        method: "GET",
        path: "/skills/proposals",
        summary: "List skill governance proposals",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/proposals",
        summary: "Create a skill governance proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/proposals/{id}/quarantine",
        summary: "Quarantine a skill proposal into a managed skill",
    },
    ApiEndpoint {
        method: "POST",
        path: "/skills/proposals/{id}/benchmark",
        summary: "Record a skill proposal benchmark verdict",
    },
];
