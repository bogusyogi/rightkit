use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_06: [ApiEndpoint; 36] = [
    ApiEndpoint {
        method: "GET",
        path: "/tasks",
        summary: "List task records",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/runtime/artifacts",
        summary: "List session artifacts",
    },
    ApiEndpoint {
        method: "GET",
        path: "/sessions/{id}/runtime/taskgraph",
        summary: "Read session task graph",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/learning",
        summary: "List learning proposals",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/learning/{id}/approve",
        summary: "Approve a runtime learning proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/learning/{id}/reject",
        summary: "Reject a runtime learning proposal",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/context/status",
        summary: "Read authenticated Membrane Hub context snapshot",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/context/adapt/status",
        summary: "Read authenticated Adapt status",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/context/adapt/review-taste",
        summary: "Review a Taste proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/context/adapt/adjudicate-taste",
        summary: "Adjudicate a reviewed Taste proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/context/adapt/apply-insights",
        summary: "Apply reviewed Adapt insights",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/context/adapt/insights",
        summary: "Mine exported session transcripts for report-only Adapt insights",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/context/adapt/apply-taste",
        summary: "Admit an adjudicated Taste proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/context/adapt/recall",
        summary: "Recall Adapt context",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/skillopt/runs",
        summary: "List SkillOpt optimization runs",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/skillopt/runs",
        summary: "Create SkillOpt optimization run",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/skillopt/runs/{id}",
        summary: "Read SkillOpt optimization run",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/skillopt/runs/{id}/resume",
        summary: "Resume SkillOpt optimization run from checkpoint",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/skillopt/runs/{id}/execute",
        summary: "Execute one SkillOpt optimization epoch",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/skillopt/runs/{id}/propose-candidate",
        summary: "Generate a SkillOpt candidate proposal",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/skillopt/runs/{id}/cancel",
        summary: "Cancel SkillOpt optimization run",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/skillopt/runs/{id}/history",
        summary: "List SkillOpt optimization run history",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/skillopt/runs/{id}/rejected-edits",
        summary: "List SkillOpt optimization rejected edits",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/harness-radar",
        summary: "List harness-radar watchlist and capabilities",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/harness-radar/proposals/{id}/promote",
        summary: "Promote a harness-radar proposal into the learning queue",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/harness-radar/schedule",
        summary: "Create or update the weekly harness-radar refresh schedule",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/benchmarks",
        summary: "List benchmark reports",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/benchmarks/proof",
        summary: "Summarize benchmark proof numbers",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/benchmark",
        summary: "Run a role benchmark",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/promote",
        summary: "Run a champion challenger promotion benchmark",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/terminal-bench/tasks",
        summary: "List terminal-bench tasks",
    },
    ApiEndpoint {
        method: "POST",
        path: "/runtime/terminal-bench/run",
        summary: "Run a terminal-bench task",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/metrics",
        summary: "List runtime role metrics",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/audit",
        summary: "Read runtime audit log",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/taste",
        summary: "List runtime taste scores",
    },
    ApiEndpoint {
        method: "GET",
        path: "/runtime/proposals",
        summary: "List runtime taste proposals",
    },
];
