use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_10: [ApiEndpoint; 9] = [
    ApiEndpoint {
        method: "GET",
        path: "/triggers",
        summary: "List redacted webhook trigger configurations",
    },
    ApiEndpoint {
        method: "POST",
        path: "/triggers",
        summary: "Create a policy-gated webhook trigger",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/triggers/{id}",
        summary: "Update a policy-gated webhook trigger",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/triggers/{id}",
        summary: "Delete a webhook trigger",
    },
    ApiEndpoint {
        method: "GET",
        path: "/triggers/audit",
        summary: "List secret- and payload-redacted webhook delivery audit rows",
    },
    ApiEndpoint {
        method: "POST",
        path: "/triggers/{id}/deliver/github",
        summary: "Accept an HMAC-authenticated GitHub webhook delivery",
    },
    ApiEndpoint {
        method: "POST",
        path: "/triggers/{id}/deliver/generic",
        summary: "Accept an HMAC-authenticated generic webhook delivery",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/executed-bakeoff",
        summary: "Run a live routed-orchestration versus same-cost single-agent bakeoff",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/corpus-bakeoff",
        summary: "Run a Harbor/private corpus through the isolated live bakeoff seam",
    },
];
