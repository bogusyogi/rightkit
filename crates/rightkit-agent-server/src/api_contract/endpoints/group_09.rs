use super::ApiEndpoint;

// The /memright/* routes were removed with the Membrane CLI cutover (#26): memory and recall
// now live in Pulse's store behind the `membrane` CLI. The group now holds the workspace skill
// activation routes (mcp R13); the manifest's group order and offsets are unchanged.
pub(super) const ENDPOINT_GROUP_09: [ApiEndpoint; 3] = [
    ApiEndpoint {
        method: "GET",
        path: "/workspace/skills",
        summary: "List a trusted workspace's project skill candidates with their activation state",
    },
    ApiEndpoint {
        method: "POST",
        path: "/workspace/skills/activate",
        summary: "Activate one project skill for a trusted workspace (keyed by content digest)",
    },
    ApiEndpoint {
        method: "POST",
        path: "/workspace/skills/deactivate",
        summary: "Deactivate one project skill for a workspace",
    },
];
