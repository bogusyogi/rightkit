use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_14_INFERENCE: [ApiEndpoint; 9] = [
    ApiEndpoint {
        method: "GET",
        path: "/inference/connections",
        summary: "List Direct inference connections",
    },
    ApiEndpoint {
        method: "POST",
        path: "/inference/connections/direct",
        summary: "Create a Direct inference connection",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/inference/connections/direct/{id}",
        summary: "Update a Direct inference connection",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/inference/connections/direct/{id}",
        summary: "Delete a Direct inference connection",
    },
    ApiEndpoint {
        method: "PUT",
        path: "/inference/connections/direct/{id}/secret",
        summary: "Store a Direct connection secret in OS keychain",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/inference/connections/direct/{id}/secret",
        summary: "Delete a Direct connection secret from OS keychain",
    },
    ApiEndpoint {
        method: "POST",
        path: "/inference/connections/{authority}/{id}/probe",
        summary: "Probe and qualify an inference connection",
    },
    ApiEndpoint {
        method: "GET",
        path: "/inference/catalog",
        summary: "List selectable inference catalog groups",
    },
    ApiEndpoint {
        method: "GET",
        path: "/inference/load",
        summary: "Read generic inference scheduler load",
    },
];
