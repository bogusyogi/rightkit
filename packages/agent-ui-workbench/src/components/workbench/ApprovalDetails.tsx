import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";

export function approvalDisplayDetail(_name: string, detail: string): string {
  try {
    const parsed = JSON.parse(detail) as Record<string, unknown>;
    if (typeof parsed.command === "string") return parsed.command;
    const path = parsed.path ?? parsed.filePath;
    if (typeof path === "string") return path;
  } catch {
    // Non-JSON tool details are already human-readable.
  }
  return detail;
}

export function approvalActionLabel(name: string): string {
  if (name === "shell" || name === "powershell") return "Run this command?";
  if (["edit_file", "multi_edit", "write_file", "apply_patch"].includes(name)) {
    return "Allow this file change?";
  }
  return `Allow ${name}?`;
}

export function ApprovalOriginDetails({
  pendingApproval,
  selected,
}: {
  pendingApproval: PendingApproval;
  selected: SessionRow | null;
}) {
  const activeToolset = selected?.toolset?.trim();
  const allowedTools = selected?.allowedTools ?? [];
  const disallowedTools = selected?.disallowedTools ?? [];
  const allowedStatus = allowedTools.includes(pendingApproval.name)
    ? `${pendingApproval.name} is present in the effective session allow-list`
    : allowedTools.length
      ? `${pendingApproval.name} is not listed in the effective session allow-list`
      : "No explicit session allow-list is active";
  const disallowedStatus = disallowedTools.includes(pendingApproval.name)
    ? "blocked by the effective session disallow-list"
    : "not disallowed by the session profile";

  return (
    <dl className="grid gap-1.5 rounded-lg border border-line bg-panel/60 px-3 py-2 text-[11px] text-text-3">
      <div className="flex min-w-0 items-center gap-2">
        <dt className="shrink-0 font-semibold text-text-2">Active toolset</dt>
        <dd className="min-w-0 truncate font-mono">{activeToolset || "default session tools"}</dd>
      </div>
      <div className="flex min-w-0 items-center gap-2">
        <dt className="shrink-0 font-semibold text-text-2">Policy source</dt>
        <dd className="min-w-0 truncate font-mono">
          {pendingApproval.policySource || pendingApproval.riskLabel || "permission policy"}
        </dd>
      </div>
      <div className="flex min-w-0 items-center gap-2">
        <dt className="shrink-0 font-semibold text-text-2">Session allow-list</dt>
        <dd className="min-w-0 truncate">
          {allowedStatus}; {disallowedStatus}
        </dd>
      </div>
    </dl>
  );
}

/**
 * A middleware `decision` is a free-text string from the daemon (e.g.
 * "blocked", "approved", "mutated"). Color it by what it actually says
 * instead of defaulting to a fixed accent — a blocked decision must read
 * as danger, not success.
 */
function middlewareDecisionTone(decision: string): string {
  const value = decision.toLowerCase();
  if (/(deny|denied|block|reject)/.test(value)) return "text-danger";
  if (/(approve|allow|permit)/.test(value)) return "text-accent-2";
  return "text-text-2";
}

export function ApprovalMiddlewareDetails({
  pendingApproval,
}: {
  pendingApproval: PendingApproval;
}) {
  if (pendingApproval.middlewareDecisions.length === 0) return null;

  return (
    <div className="grid gap-2 rounded-lg border border-line bg-panel/60 px-3 py-2 text-[11px] text-text-3">
      <div className="flex items-center gap-2">
        <Icon name="activity" size={12} />
        <strong className="text-text-2">Runtime middleware</strong>
      </div>
      <div className="grid gap-1.5">
        {pendingApproval.middlewareDecisions.map((middleware) => (
          <div
            className="grid grid-cols-[minmax(0,1fr)_auto] gap-2 rounded-md border border-line bg-bg px-2 py-1.5"
            key={`${middleware.middlewareId}:${middleware.decision}`}
          >
            <div className="min-w-0">
              <div className="flex min-w-0 items-center gap-2">
                <span className="min-w-0 truncate font-semibold text-text-2">
                  {middleware.title}
                </span>
                <code className="shrink-0 text-[10px] text-text-3">{middleware.effect}</code>
              </div>
              <p className="mt-0.5 truncate">{middleware.reason}</p>
            </div>
            <div className="text-right">
              <code
                className={`block text-[10px] font-semibold ${middlewareDecisionTone(middleware.decision)}`}
              >
                {middleware.decision}
              </code>
              <span className="block text-[10px] text-text-3">{middleware.phase}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
