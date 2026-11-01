use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_02: [ApiEndpoint; 33] = [
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/goal",
        summary: "Read a persistent session goal snapshot",
    },
    ApiEndpoint {
        method: "POST",
        path: "/permission-rules/import",
        summary: "Import persistent permission rules",
    },
    ApiEndpoint {
        method: "GET",
        path: "/permission-denials",
        summary: "List recorded permission denials",
    },
    ApiEndpoint {
        method: "POST",
        path: "/notifications/{id}/dismiss",
        summary: "Dismiss a daemon notification",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions",
        summary: "List sessions",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions",
        summary: "Create session",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/search",
        summary: "Search persisted session events",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/sessions/{id}",
        summary: "Delete session",
    },
    ApiEndpoint {
        method: "PATCH",
        path: "/sessions/{id}",
        summary: "Patch session metadata",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/spec",
        summary: "Bind a session to a feature spec",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/sessions/{id}/spec",
        summary: "Clear a session feature spec binding",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/spec/completion",
        summary: "Read bound spec completion gate",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/transcript",
        summary: "Read session transcript",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/untrusted-content",
        summary: "Audit untrusted transcript content",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/away-summary",
        summary: "Read return digest for recent session activity",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/messages",
        summary: "Submit user message",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/fork",
        summary: "Fork a session with parent lineage",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/events",
        summary: "Stream session events",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/events/history",
        summary: "Replay session events",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/handoff",
        summary: "Create a session handoff attach payload",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/compact",
        summary: "Compact session context",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/compaction-archives",
        summary: "List durable pre-compaction transcript archives",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/compaction-archives/{archive_id}",
        summary: "Retrieve an exact pre-compaction transcript archive",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/context-fold",
        summary: "Fold a recoverable session sub-trajectory",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/context-folds/{archive_ref}",
        summary: "Retrieve a session context-fold archive",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/tool-approval",
        summary: "Resolve pending tool approval",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/answer-question",
        summary: "Resolve a pending session question",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/resolve-plan",
        summary: "Resolve a pending session plan approval",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/resume",
        summary: "Resume a dormant session",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/interrupt",
        summary: "Interrupt a running session",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/steer",
        summary: "Inject steering text into a running session",
    },
    ApiEndpoint {
        method: "POST",
        path: "/sessions/{id}/goal",
        summary: "Set a persistent session goal",
    },
    ApiEndpoint {
        method: "DELETE",
        path: "/sessions/{id}/goal",
        summary: "Clear a persistent session goal",
    },
];
