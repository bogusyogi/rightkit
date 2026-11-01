//! Daemon-side slash executors.
//!
//! P1-1: these are the executable identities carried by [`COMMAND_SPECS`]
//! rows (`daemon_handler`). Every function here is referenced by exactly one
//! row of that table; adding a command means writing one handler and adding
//! its one row — there is no second registration list to keep in sync.
//!
//! Handlers whose real work happens elsewhere in the daemon
//! (`execute_daemon_command` in `api_session_context_runtime.rs` handles
//! e.g. `status`, `model`, `export` before this registry is consulted)
//! return the same honest guidance text they always have as the fallback
//! when that richer path is unavailable.

use super::*;

/// Success result carrying pre-rendered output.
fn ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

macro_rules! static_handler {
    ($name:ident, $text:expr) => {
        pub(super) fn $name(_args: &[String]) -> CommandResult {
            ok($text)
        }
    };
}

// ── Action handlers ─────────────────────────────────────────────────────────

pub(super) fn help_handler(args: &[String]) -> CommandResult {
    let show_advanced = args.iter().any(|arg| arg.eq_ignore_ascii_case("--all"));
    let mut output = String::from("**Available commands:**");

    for group in [HelpGroup::Common, HelpGroup::Advanced] {
        if group == HelpGroup::Advanced && !show_advanced {
            continue;
        }
        for spec in entries_for_surface(false).filter(|entry| entry.help_group == group) {
            output.push_str("\n- `");
            output.push_str(spec.usage);
            output.push_str("` — ");
            output.push_str(spec.summary);
        }
    }

    ok(output)
}

pub(super) fn clear_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Chat cleared.".into(),
        action: CommandAction::ClearChat,
    }
}

pub(super) fn compact_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Compacting context…".into(),
        action: CommandAction::CompactContext,
    }
}

pub(super) fn sessions_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Listing sessions…".into(),
        action: CommandAction::ListSessions,
    }
}

pub(super) fn memory_handler(_args: &[String]) -> CommandResult {
    ok("Memory context is managed by the daemon. Use the memory panel to view entries.")
}

pub(super) fn model_handler(_args: &[String]) -> CommandResult {
    ok(
        "When executed through the daemon, shows or updates the active session model. External clients can use `/model <session-id> [model]`.",
    )
}

pub(super) fn dream_handler(args: &[String]) -> CommandResult {
    if args.first().map(String::as_str) == Some("now") {
        CommandResult {
            success: true,
            output: "Starting memory dream consolidation now.".into(),
            action: CommandAction::DreamNow("today".into()),
        }
    } else {
        CommandResult {
            success: false,
            output: "Usage: `/dream now`".into(),
            action: CommandAction::None,
        }
    }
}

pub(super) fn review_handler(args: &[String]) -> CommandResult {
    let goal = review_conductor_goal(args);
    CommandResult {
        success: true,
        output: "Starting read-only auditor review of the current workspace changes.".into(),
        action: CommandAction::ConductGoal(goal),
    }
}

pub(super) fn audit_handler(args: &[String]) -> CommandResult {
    let (fix, _) = parse_audit_fix_mode(args);
    let goal = audit_conductor_goal(args);
    CommandResult {
        success: true,
        output: if fix {
            "Starting whole-repo auditor pass, then fixing what it finds.".into()
        } else {
            "Starting whole-repo auditor pass for the current workspace.".into()
        },
        action: CommandAction::ConductGoal(goal),
    }
}

pub(super) fn blueprint_handler(args: &[String]) -> CommandResult {
    let goal = blueprint_conductor_goal(args);
    CommandResult {
        success: true,
        output: "Starting repository blueprint mapping for the current workspace.".into(),
        action: CommandAction::ConductGoal(goal),
    }
}

pub(super) fn architect_handler(args: &[String]) -> CommandResult {
    let goal = architect_conductor_goal(args);
    CommandResult {
        success: true,
        output: "Starting architect method: explore → ADR (2-3 options + decision) → challenger gate → TDD file-by-file plan → critical files.".into(),
        action: CommandAction::ConductGoal(goal),
    }
}

pub(super) fn commit_handler(args: &[String]) -> CommandResult {
    let goal = commit_conductor_goal(args);
    CommandResult {
        success: true,
        output:
            "Starting diff-scoped auditor pass plus commit+push gate for the current workspace."
                .into(),
        action: CommandAction::ConductGoal(goal),
    }
}

// ── Guidance handlers (the work belongs to another surface; say so) ────────

static_handler!(
    context_handler,
    "When executed through the daemon, summarizes live sessions, workspace instruction sources, memory, tasks, schedules, tools, skills, plugins, and MCP state."
);
static_handler!(
    install_handler,
    "Install or update the always-on daemon service from a terminal:\n\n\
     - `host install` or `host install-service`\n\
     - `host uninstall` or `host uninstall-service`\n\n\
     For local development, run `cd apps/host-tauri && npm run tauri dev`."
);
static_handler!(
    caps_handler,
    "When executed through the daemon, lists the runtime tool registry grouped by capability area."
);
static_handler!(
    status_handler,
    "When executed through the daemon, reports live sessions, provider routing, plugin/MCP state, and diagnostics verdict."
);
static_handler!(
    doctor_handler,
    "When executed through the daemon, renders the runtime diagnostics shared with `rightkit-agent doctor` and `/health/diagnostics`."
);
static_handler!(
    diff_handler,
    "Ask: `review the current git diff`. The agent has `git_diff` and `git_read` tools available for staged/unstaged changes."
);
static_handler!(
    permissions_handler,
    "When executed through the daemon, shows the active session's live permission mode and rules. External clients can use `/permissions <session-id>`."
);
static_handler!(
    instructions_handler,
    "When executed through the daemon, reports discovered AGENTS.md/CLAUDE.md/right.md/.claude instruction sources for the workspace."
);
static_handler!(
    mcp_handler,
    "MCP servers load from `~/.config/rightkit-agent/mcp.json` or workspace `.mcp.json` (desktop also honours an operator override). Use `list_mcp_tools` in-session to discover configured tools."
);
static_handler!(
    plugin_handler,
    "Plugin state is available in Settings -> Plugins. Use `/plugin marketplace [query]` to inspect the configured marketplace catalog."
);
static_handler!(
    plugins_handler,
    "Plugin state is available in Settings -> Plugins. Use `/plugins marketplace [query]` to inspect the configured marketplace catalog."
);
static_handler!(
    teams_handler,
    "Team definitions are available in Runtime -> Teams."
);
static_handler!(
    messages_handler,
    "Inter-session message history is available in Runtime -> Messages."
);
static_handler!(
    usage_handler,
    "When executed through the daemon, reports the latest usageUpdated totals for the active session. External clients can use `/usage <session-id>`."
);
static_handler!(
    schedules_panel_handler,
    "Workspace schedules are available in Runtime -> Schedules."
);
static_handler!(
    skills_handler,
    "Local skills are loaded from configured skill directories and can be invoked by the `invoke_skill` tool."
);
static_handler!(
    spec_handler,
    "When executed through the daemon, manages the explicit spec-governed development store. Examples: `/spec list`, `/spec new Add login`, `/spec analyze spec-...`, `/spec trace spec-...`."
);
static_handler!(
    tasks_handler,
    "Open the Tasks pane or Runtime -> Task graph to inspect active orchestration tasks."
);
static_handler!(
    benchmarks_handler,
    "Benchmark reports are available through the daemon runtime history."
);
static_handler!(
    terminal_bench_handler,
    "Terminal-Bench task catalog and run commands are available through the daemon."
);
static_handler!(
    summary_handler,
    "When executed through the daemon, returns deterministic transcript counts, last user/assistant turns, and recent tool outcomes for the active session. External clients can use `/summary <session-id>`."
);
static_handler!(
    export_handler,
    "Export the active session transcript as markdown, or JSON with `--json`. External clients can use `/export <session-id> [--json]`."
);
static_handler!(
    handoff_handler,
    "Create a versioned attach payload for the active session. External clients can use `/handoff <session-id>`."
);

// Session-local commands genuinely execute inside the running the host app
// process (CLI/TUI), not on the daemon; the daemon answers honestly rather
// than pretending to have run them.
static_handler!(
    quit_handler,
    "/quit exits the Agent session process where it is typed; the daemon cannot end another client's session."
);
static_handler!(
    onboard_handler,
    "Onboarding runs inside the Agent session process (`/onboard`) or a terminal (`rightkit-agent onboard`); the daemon cannot prompt for credentials."
);
static_handler!(
    models_handler,
    "The live model catalog renders inside the Agent session (`/models`). Over HTTP, GET /inference/catalog returns the same catalog data."
);
static_handler!(
    fork_handler,
    "Forking runs through the Agent session (`/fork`) or `POST /sessions/<id>/fork`; no active session is attached to this request."
);
static_handler!(
    resume_handler,
    "Resume a dormant session from a terminal (`rightkit-agent resume <id>`) or `POST /sessions/<id>/resume`; interactive stdin/stdout forwarding needs the attaching process."
);
static_handler!(
    hooks_handler,
    "Hook readiness is reported by the daemon at GET /hooks/status; the TUI surfaces it via /hooks."
);

fn review_conductor_goal(args: &[String]) -> String {
    let focus = args.join(" ");
    let focus = focus.trim();
    let mut goal = "Run a read-only auditor pass for the current workspace changes. Create a task categorized as `code-review` so Agent routes this to `auditor`, not the plan-review jury/council. Review git status, git diff, and relevant files as evidence. Findings should lead, ordered by severity, with concrete file/line references wherever possible. Keep summaries brief. If there are no actionable issues, say that clearly and mention any remaining test gaps or residual risk.".to_string();
    if !focus.is_empty() {
        goal.push_str("\n\nAdditional focus: ");
        goal.push_str(focus);
    }
    goal
}

/// Split a leading `fix` argument off the focus text.
///
/// `/audit fix` asks for the fix loop; `/audit fix the login flow` is a focus
/// that merely begins with the word. Only a bare leading `fix` (or `--fix`)
/// switches modes, so the ordinary focus path keeps working.
fn parse_audit_fix_mode(args: &[String]) -> (bool, String) {
    let mut rest = args;
    let mut fix = false;
    if let Some(first) = args.first() {
        let head = first.trim().to_ascii_lowercase();
        if head == "fix" || head == "--fix" {
            fix = true;
            rest = &args[1..];
        }
    }
    (fix, rest.join(" ").trim().to_string())
}

const AUDIT_EVIDENCE: &str = "First gather deterministic evidence: project instructions, git status, repository tree, package/workspace manifests, build/test configuration, dependency and security-sensitive surfaces, generated artifacts, public docs/claims, and stale or parked code paths. Then execute the auditor's repo-wide passes: doc drift, architecture contradictions, AI slop, naming drift, dead files, schema/API drift, security smells, public-claim verification, and evidence-led readiness. Run available deterministic scanners/checks when reasonable; if a tool is unavailable or skipped, report that as skipped, never as clean. Findings should lead, ordered by severity, with concrete file/line references wherever possible.";

fn audit_conductor_goal(args: &[String]) -> String {
    let (fix, focus) = parse_audit_fix_mode(args);

    let mut goal = if fix {
        format!(
            "Run the whole-repo auditor flow for the current workspace, then FIX what it finds. \
Two phases, executed in order.\n\n\
Phase 1 — audit. Create a task categorized as `audit` so Host routes this to the existing `auditor` role, not the plan-review jury/council. This phase is read-only. {AUDIT_EVIDENCE}\n\n\
Phase 2 — fix. Hand the findings to the implementation role (`coder`) as a task categorized as `implementation`. You ARE permitted to modify files in this phase. Work the findings in severity order and stop at the ones that are safely fixable without a product decision; leave anything needing Adrian's judgement as a reported finding, and say why. Constraints, all of them binding: make the smallest change that resolves each finding; preserve existing behaviour and public contracts; never edit or delete a test to make it pass, and never weaken an assertion — if a test is genuinely wrong, report it rather than changing it; one finding per edit so the diff stays reviewable. After each fix, re-run the project's own checks (its configured build, test, lint and format commands, discovered from its manifests and project instructions — not assumed). If a check cannot be run, say which and why, and treat the fix as UNVERIFIED rather than done.\n\n\
Then confirm the fix achieves its stated EFFECT, not merely that the edit landed. Many real fixes have no check that would catch a no-op, so verify the outcome directly and in the project's own terms. Concretely: a file added to `.gitignore` does nothing if git already tracks it (`git ls-files --error-unmatch <path>` — it needs `git rm --cached` as well); a config key does nothing if a higher-precedence layer overrides it; a guard does nothing if the path it guards is unreachable; a corrected doc claim is only fixed if it now matches what the code or data actually says. State the check you ran and its result for every fix. If a fix cannot be shown to take effect, say so plainly — an edit that looks right and changes nothing is worse than an open finding, because it closes the item. Do NOT commit or push; leave the working tree dirty for review.\n\n\
Report: the findings, what was fixed, what was left and why, the exact commands used to verify, and their real output. If a verification command failed, quote the failure — never substitute a guess at the cause for what the tool actually said."
        )
    } else {
        format!("Run the whole-repo auditor flow for the current workspace. Create a task categorized as `audit` so Agent routes this to the existing `auditor` role, not the plan-review jury/council or an implementation role. Stay read-only. {AUDIT_EVIDENCE} Keep summaries brief. If there are no actionable issues, say that clearly and mention remaining test gaps, skipped checks, or residual risk.")
    };

    if !focus.is_empty() {
        goal.push_str("\n\nAdditional focus: ");
        goal.push_str(&focus);
    }
    goal
}

fn blueprint_conductor_goal(args: &[String]) -> String {
    let focus = args.join(" ");
    let focus = focus.trim();
    let mut goal = "Run the blueprint-map flow for the current repository. Create tasks categorized as `research` and `architecture` so Agent uses the existing `researcher` and `architect` roles, with the conductor synthesizing their results into one repo-understanding map. Stay read-only. Read project instructions first, then inspect the repository tree, manifests, workspace members, docs linked from the GTM/architecture entrypoints, primary binaries, frontend app roots, daemon/engine crates, integration boundaries, tests, and current git state. Build an evidence-backed map of modules, ownership boundaries, data/control flow, important commands, extension points, persisted state, external services, UI surfaces, and known parked/legacy areas. Call out stale docs, missing contracts, architectural risks, and questions that would change implementation choices. Output a concise but practical blueprint with file references and a 'critical files' section suitable for later OKF `.rightkit/understanding/` emission; do not modify files.".to_string();
    if !focus.is_empty() {
        goal.push_str("\n\nAdditional focus: ");
        goal.push_str(focus);
    }
    goal
}

fn architect_conductor_goal(args: &[String]) -> String {
    let focus = args.join(" ");
    let focus = focus.trim();
    let mut goal = "Run the architect method for the current workspace. Create a task categorized as `architecture` so Agent routes this to the existing `architect` role and invokes the challenger as the adversarial gate. Four phases, executed in order. Phase 1 — Explore: read the codebase first. Inspect the affected surface — files, modules, interfaces, data flows, tests, existing contracts, and constraints. Gather evidence before proposing anything. Understand the blast radius. Phase 2 — ADR: produce an Architecture Decision Record with 2-3 concrete options and a single decision. Each option must include Context (forces/constraints), Option (the approach, described concretely enough to implement), and Consequences (what breaks, what improves, what becomes harder). Then state the Decision and why. Prefer the smallest change that preserves existing contracts. The challenger reviews the ADR before the plan is accepted — if the challenger rejects, revise and re-submit. Phase 3 — TDD File-by-File Plan: for the chosen option, produce a dependency-ordered, file-by-file plan. Each step must list the exact file(s) to create or modify, what changes in each file, the test(s) that verify the step, and its dependency on prior steps. Must be executable by an engineer or coder without guessing. Phase 4 — Critical Files: list the files most important to get right — where a mistake causes the most damage. For each, state why it is critical and what to watch for. Deliverable: ADR + TDD plan + Critical Files list. If the task is small enough that this ceremony is unnecessary, say so and recommend the simpler path. Do not add a service, dependency, or abstraction the task does not need.".to_string();
    if !focus.is_empty() {
        goal.push_str("\n\nAdditional focus: ");
        goal.push_str(focus);
    }
    goal
}

fn commit_conductor_goal(args: &[String]) -> String {
    let focus = args.join(" ");
    let focus = focus.trim();
    let mut goal = "Run the diff-scoped audit then commit+push gate for the current workspace. Create a task categorized as `commit` so Agent routes this to the existing `auditor` role, not the plan-review jury/council or an implementation role. Two phases, executed in order. Phase 1 — diff-scoped audit lenses: stay read-only and gather evidence first (git status, the staged and unstaged and branch-tip diff, and the files referenced by the diff), then run the auditor's lens set scoped to the changed paths only — code correctness, naming drift, doc drift against the touched area, security smells in the diff, schema/API drift for any contract the diff crosses, AI slop, and any leftover dead code in the touched files. Reuse the existing audit-lens engine — do NOT shell out to external binaries for lint/format/security scans; invoke the project's audit toolchain through the auditor role. Phase 2 — commit+push gate: only when the lens pass clears (no actionable findings, or all findings triaged or resolved) proceed to commit and push. Run the git operations through `shell`. Only `git_diff` and `git_read` exist as dedicated tools and both are read-only, so committing and pushing go through `shell` — which is already the gated, logged path: `git commit` and `git push` are not on the AutoSafe allowlist, so each one is surfaced for approval and recorded in the tool ledger. If the lens pass surfaces blocking findings, surface them and do not commit. Findings should lead, ordered by severity, with concrete file/line references wherever possible. Keep summaries brief. If there are no actionable issues, say that clearly and mention remaining test gaps or residual risk.".to_string();
    if !focus.is_empty() {
        goal.push_str("\n\nAdditional focus: ");
        goal.push_str(focus);
    }
    goal
}
