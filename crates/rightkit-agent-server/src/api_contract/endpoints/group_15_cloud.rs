use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_15_CLOUD: [ApiEndpoint; 11] = [
    ApiEndpoint {
        method: "GET",
        path: "/forges",
        summary: "List forge connections and sign-in state",
    },
    ApiEndpoint {
        method: "POST",
        path: "/forges/github/connect",
        summary: "Start GitHub device sign-in on the daemon",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/forges/github/connect",
        summary: "Disconnect the daemon GitHub sign-in",
    },
    ApiEndpoint {
        method: "GET",
        path: "/forges/github/installations",
        summary: "List GitHub App installations for the signed-in user",
    },
    ApiEndpoint {
        method: "GET",
        path: "/forges/github/repositories",
        summary: "List repositories the GitHub App and the user can both reach",
    },
    ApiEndpoint {
        method: "POST",
        path: "/cloud-workspaces",
        summary: "Create a cloud workspace and clone its repository",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cloud-workspaces",
        summary: "List cloud workspaces",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cloud-workspaces/{id}",
        summary: "Read one cloud workspace",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/cloud-workspaces/{id}",
        summary: "Delete a cloud workspace checkout",
    },
    ApiEndpoint {
        method: "GET",
        path: "/cloud-workspaces/{id}/events",
        summary: "Read the cloud workspace transition journal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/cloud-workspaces/{id}/sessions",
        summary: "Create a session bound to a ready cloud workspace",
    },
];
