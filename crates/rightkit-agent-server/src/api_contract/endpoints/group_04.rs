use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_04: [ApiEndpoint; 29] = [
    ApiEndpoint {
        method: "PATCH",
        path: "/execution/backends/{id}",
        summary: "Update workspace execution backend settings",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/execution/network-profiles/{id}",
        summary: "Save workspace execution network profile",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/execution/network-profiles/{id}",
        summary: "Delete workspace execution network profile",
    },
    ApiEndpoint {
        method: "POST",
        path: "/execution/backends/{id}/doctor",
        summary: "Run execution backend doctor checks",
    },
    ApiEndpoint {
        method: "GET",
        path: "/execution/workspaces",
        summary: "List execution backend workspaces",
    },
    ApiEndpoint {
        method: "POST",
        path: "/execution/workspaces",
        summary: "Create execution backend workspace",
    },
    ApiEndpoint {
        method: "GET",
        path: "/execution/processes",
        summary: "List live execution processes",
    },
    ApiEndpoint {
        method: "POST",
        path: "/execution/processes",
        summary: "Create interactive execution process",
    },
    ApiEndpoint {
        method: "GET",
        path: "/execution/processes/{id}",
        summary: "Read live execution process detail",
    },
    ApiEndpoint {
        method: "GET",
        path: "/execution/processes/{id}/logs",
        summary: "Read live execution process logs",
    },
    ApiEndpoint {
        method: "POST",
        path: "/execution/processes/{id}/stdin",
        summary: "Write stdin to live execution process",
    },
    ApiEndpoint {
        method: "POST",
        path: "/execution/processes/{id}/resize",
        summary: "Resize live execution PTY",
    },
    ApiEndpoint {
        method: "POST",
        path: "/execution/processes/{id}/kill",
        summary: "Kill live execution process",
    },
    ApiEndpoint {
        method: "GET",
        path: "/recipes",
        summary: "List installed recipes",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/install",
        summary: "Install recipe",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/install-file",
        summary: "Install local recipe file",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/install-url",
        summary: "Install GitHub recipe file",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/validate",
        summary: "Validate recipe document",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/render",
        summary: "Render recipe parameters",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/deeplink/parse",
        summary: "Parse recipe deeplink",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/{id}/run",
        summary: "Start local recipe run",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/{id}/schedule",
        summary: "Schedule recipe run",
    },
    ApiEndpoint {
        method: "POST",
        path: "/recipes/{id}/deeplink",
        summary: "Create recipe deeplink",
    },
    ApiEndpoint {
        method: "GET",
        path: "/recipes/{id}/runs",
        summary: "List recipe run history",
    },
    ApiEndpoint {
        method: "GET",
        path: "/schedules",
        summary: "List workspace schedules",
    },
    ApiEndpoint {
        method: "POST",
        path: "/schedules",
        summary: "Create workspace schedule",
    },
    ApiEndpoint {
        method: "GET",
        path: "/schedules/audit",
        summary: "List workspace schedule admin audit events",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/schedules/{id}",
        summary: "Patch workspace schedule",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/schedules/{id}",
        summary: "Delete workspace schedule",
    },
];
