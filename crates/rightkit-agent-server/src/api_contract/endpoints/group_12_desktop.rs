use super::ApiEndpoint;

macro_rules! endpoint {
    ($method:literal, $path:literal, $summary:literal) => {
        ApiEndpoint {
            method: $method,
            path: $path,
            summary: $summary,
        }
    };
}

pub(super) const ENDPOINT_GROUP_12_DESKTOP: [ApiEndpoint; 63] = [
    endpoint!("GET", "/workspace/tree", "List a workspace directory"),
    endpoint!(
        "POST",
        "/workspace/tree/expand",
        "Expand a workspace directory"
    ),
    endpoint!("GET", "/workspace/file", "Read a bounded workspace file"),
    endpoint!(
        "PUT",
        "/workspace/file",
        "Save a fingerprint-guarded workspace file"
    ),
    endpoint!(
        "POST",
        "/workspace/search",
        "Search workspace names or content"
    ),
    endpoint!("GET", "/workspace/artifacts", "List workspace artifacts"),
    endpoint!(
        "GET",
        "/workspace/recent",
        "List recently opened workspace files"
    ),
    endpoint!(
        "GET",
        "/workspace/watch",
        "Watch coalesced workspace changes"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace",
        "Read shared browser state"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/open",
        "Open an isolated browser session"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/close",
        "Close a browser session"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/navigate",
        "Navigate a browser session"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/reload",
        "Reload a browser session"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/back",
        "Navigate backward in browser history"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/forward",
        "Navigate forward in browser history"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/viewport",
        "Read browser viewport"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/viewport",
        "Set browser viewport"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/zoom",
        "Set browser zoom"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/screenshot",
        "Capture browser screenshot"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/console",
        "Read browser console"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/console",
        "Read filtered browser console"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/network",
        "Read browser network summary"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/errors",
        "Read browser errors"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/errors",
        "Read filtered browser errors"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/dev-servers",
        "Discover local development servers"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/action-log",
        "Read browser action log"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/pause",
        "Pause agent browser control"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/resume",
        "Resume agent browser control"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/take-control",
        "Give browser control to the operator"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/release-control",
        "Return browser control to the agent"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/annotations",
        "List browser annotations"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/annotations",
        "Create a browser annotation"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/annotations/{ann_id}/send",
        "Send a browser annotation to the agent"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/host-policy",
        "Read browser host policy"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/host-policy",
        "Set browser host policy"
    ),
    endpoint!(
        "GET",
        "/sessions/{id}/browser/workspace/profile",
        "Read browser profile mode"
    ),
    endpoint!(
        "POST",
        "/sessions/{id}/browser/workspace/profile",
        "Consent to a browser profile mode"
    ),
    endpoint!("GET", "/delivery/status", "Read delivery capability status"),
    endpoint!("GET", "/delivery/branches", "List repository branches"),
    endpoint!("GET", "/delivery/worktrees", "List repository worktrees"),
    endpoint!(
        "GET",
        "/worktrees/pool/status",
        "Read pre-warmed worktree pool readiness per workspace"
    ),
    endpoint!(
        "POST",
        "/worktrees/pool/acquire",
        "Acquire a pre-warmed worktree leased to a session"
    ),
    endpoint!(
        "POST",
        "/worktrees/pool/{id}/release",
        "Release a leased pooled worktree back to the warm pool"
    ),
    endpoint!("POST", "/delivery/worktree", "Create a repository worktree"),
    endpoint!(
        "DELETE",
        "/delivery/worktree",
        "Remove a repository worktree"
    ),
    endpoint!("POST", "/delivery/stage", "Stage repository paths"),
    endpoint!("DELETE", "/delivery/stage", "Unstage repository paths"),
    endpoint!("POST", "/delivery/commit", "Commit staged changes"),
    endpoint!("POST", "/delivery/push", "Push the current branch"),
    endpoint!("POST", "/delivery/pull", "Pull the current branch"),
    endpoint!("POST", "/delivery/pr", "Create a pull request"),
    endpoint!("GET", "/delivery/pr/{id}", "Read pull request metadata"),
    endpoint!(
        "GET",
        "/delivery/pr/{id}/checks",
        "List pull request checks"
    ),
    endpoint!(
        "POST",
        "/delivery/pr/{id}/checks/{checkId}/rerun",
        "Rerun a pull request check"
    ),
    endpoint!(
        "GET",
        "/delivery/pr/{id}/checks/{checkId}/logs",
        "Read pull request check logs"
    ),
    endpoint!(
        "GET",
        "/delivery/pr/{id}/threads",
        "List pull request review threads"
    ),
    endpoint!(
        "POST",
        "/delivery/command",
        "Run a visible delivery command"
    ),
    endpoint!("POST", "/delivery/cancel", "Cancel a delivery command"),
    endpoint!(
        "GET",
        "/environments",
        "List configured execution environments"
    ),
    endpoint!(
        "POST",
        "/environments/select",
        "Select an execution environment"
    ),
    endpoint!(
        "POST",
        "/environments/{id}/test",
        "Test an execution environment"
    ),
    endpoint!("GET", "/environments/scripts", "List project setup scripts"),
    endpoint!(
        "POST",
        "/environments/scripts/run",
        "Run a project setup script"
    ),
];

pub(super) const ENDPOINT_GROUP_12_DESKTOP_TAIL: [ApiEndpoint; 15] = [
    endpoint!(
        "POST",
        "/environments/scripts/runs/{runId}/cancel",
        "Report cancellation state for a synchronous setup-script run"
    ),
    endpoint!(
        "GET",
        "/environments/scripts/runs/{runId}/logs",
        "Read project setup logs"
    ),
    endpoint!(
        "POST",
        "/environments/scripts/runs/{runId}/rerun",
        "Rerun a project setup script"
    ),
    endpoint!("GET", "/attention/items", "List typed attention items"),
    endpoint!(
        "PATCH",
        "/attention/items/{id}/state",
        "Update an attention item"
    ),
    endpoint!(
        "POST",
        "/attention/items/{id}/snooze",
        "Snooze an attention item"
    ),
    endpoint!(
        "POST",
        "/attention/items/{id}/unsnooze",
        "Unsnooze an attention item"
    ),
    endpoint!("GET", "/attention/density", "Read transcript density"),
    endpoint!("PATCH", "/attention/density", "Set transcript density"),
    endpoint!("GET", "/attention/policy", "Read notification policy"),
    endpoint!("PATCH", "/attention/policy", "Set notification policy"),
    endpoint!("GET", "/attention/quiet-hours", "Read quiet hours"),
    endpoint!("PATCH", "/attention/quiet-hours", "Set quiet hours"),
    endpoint!(
        "POST",
        "/attention/deeplink",
        "Resolve an attention deep link"
    ),
    endpoint!(
        "GET",
        "/attention/banner",
        "Read the highest priority attention banner"
    ),
];
