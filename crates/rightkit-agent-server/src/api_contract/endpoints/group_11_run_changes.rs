// I1 — Authoritative review / run-scoped changes contract fragment.
//
// Standalone fragment: imports the typed endpoint records from
// `api_run_changes` and re-exports them with the `ApiEndpoint`
// shape the integrator (I2 / Agent 4 lead) appends to
// `api_contract/endpoints.rs`. Keeping this as its own file lets
// the I1 lane own the OpenAPI surface without editing
// `endpoints.rs` (forbidden until I2 structural composition lands).

use super::ApiEndpoint;

pub(super) const ENDPOINT_GROUP_11_RUN_CHANGES: [ApiEndpoint; 9] = [
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/capture",
        summary: "Persist a run-scoped inventory snapshot and reconcile the captured entries against the live workspace + git state",
    },
    ApiEndpoint {
        method: "GET",
        path: "/run-changes/inventory",
        summary: "Return the full durable inventory (entries + classifications + git baseline) for a run/workspace pair",
    },
    ApiEndpoint {
        method: "GET",
        path: "/run-changes/summary",
        summary: "Return a compact summary (counts by classification, first paths, truncation flag) for the post-synthesis card",
    },
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/undo",
        summary: "Replay the captured inverse op against the live workspace; rejects if the observed fingerprint no longer matches the live file",
    },
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/proposal",
        summary: "Keep or discard a change a sub-agent proposed to this chat",
    },
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/hunk",
        summary: "Keep or undo one hunk of a file change (a held sub-agent change keeps it into the project; an applied change undoes it in the working file)",
    },
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/revise",
        summary: "Record a structured revision / inline / question / resolution comment against a mutation anchor",
    },
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/diff-body",
        summary: "Return a virtualized (unified or split) diff body window for the ReviewPane, with optional search",
    },
    ApiEndpoint {
        method: "POST",
        path: "/run-changes/reset",
        summary: "Drop the durable inventory for a run after auth checks; foreground-only",
    },
];
