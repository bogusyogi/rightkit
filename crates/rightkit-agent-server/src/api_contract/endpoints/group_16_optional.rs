use super::ApiEndpoint;

// Endpoints of routes a host mounts only when it turns an option on. `endpoints.rs` inserts each
// slice into the manifest only when its option is on, so the contract never documents a route
// the daemon does not serve.

/// Guest links (`host_options::guest_shares`, `guest_shares.rs`). Inserted before
/// `GET /sessions/{id}/rewind/points`.
pub(super) const ENDPOINT_GROUP_16_GUEST_SHARES: [ApiEndpoint; 4] = [
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/shares",
        summary: "Mint a guest link for one live session (read, optionally approve); returns its token once",
    },
    ApiEndpoint {
        method: "GET",
        path: "/shares",
        summary: "List live guest links (never their tokens) and the LAN listener address",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/shares/{id}",
        summary: "Revoke a guest link",
    },
    ApiEndpoint {
        method: "GET",
        path: "/guest/whoami",
        summary: "Whether the caller is the owner or a guest, and a guest's session and rights",
    },
];

/// Browser consent (`host_options::browser_consent_routes`, `api_browser_consent.rs`). Inserted
/// before `GET /delivery/status`.
pub(super) const ENDPOINT_GROUP_16_BROWSER_CONSENT: [ApiEndpoint; 4] = [
    ApiEndpoint {
        method: "GET",
        path: "/browser/consent/sources",
        summary: "Read the default browser and its profiles for the consent screen",
    },
    ApiEndpoint {
        method: "GET",
        path: "/browser/consents",
        summary: "List browser consent receipts",
    },
    ApiEndpoint {
        method: "POST",
        path: "/browser/consents",
        summary: "Grant per-domain browser consent",
    },
    ApiEndpoint {
        method: "POST",
        path: "/browser/consents/{id}/revoke",
        summary: "Revoke browser consent and purge the imported copy",
    },
];
