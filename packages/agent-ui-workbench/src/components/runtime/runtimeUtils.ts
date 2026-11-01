import type {
  ArtifactRow,
  RecipeParameter,
  RecipeValidationRun,
  ScheduleAdminAuditEvent,
  ScheduleRunEvent,
} from "@rightkit/agent-ui-core/lib/types";

export function formatIsoTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

export function formatEpochMinute(value: number) {
  return new Date(value * 60_000).toLocaleString();
}

export function isRecipeBooleanChecked(value: string) {
  return ["1", "true", "yes", "on"].includes(value.trim().toLowerCase());
}

export function parseRecipeParamsDraft(draft: string): Record<string, string> {
  try {
    const parsed = JSON.parse(draft || "{}") as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).map(([key, value]) => [key, value == null ? "" : String(value)]),
    );
  } catch {
    return {};
  }
}

export function recipeParameterValue(
  parameter: RecipeParameter,
  values: Record<string, string>,
): string {
  return values[parameter.key] ?? parameter.value ?? parameter.default ?? "";
}

export function recipeParamsDraftWithValue(draft: string, key: string, value: string) {
  const next = parseRecipeParamsDraft(draft);
  next[key] = value;
  return JSON.stringify(next, null, 2);
}

// NOTE: unused duplicate of ./recipeVerdictClass.ts — RecipesRuntimePanel imports
// from the standalone module, not this export. Flagged, not removed (pre-existing
// dead code, out of scope for a visual-only pass); kept token-consistent regardless.
export function recipeVerdictClass(verdict: RecipeValidationRun["verdict"] | "warn") {
  if (verdict === "pass") return "border-accent-2/40 text-accent-2";
  if (verdict === "block") return "border-danger/40 text-danger";
  return "border-warning/40 text-warning";
}

export function phaseLabel(phase: string) {
  return phase.replace(/([A-Z])/g, " $1").replace(/^./, (value) => value.toUpperCase());
}

export function formatScheduleLastClaimed(value?: number | null) {
  if (value === null || value === undefined) return "Last claimed: never";
  return `Last claimed: ${formatEpochMinute(value)}`;
}

export function formatScheduleNextDue(schedule: {
  nextDueError?: string | null;
  nextDueEpochMinute?: number | null;
}): string {
  if (schedule.nextDueError) return `Next due unavailable: ${schedule.nextDueError}`;
  if (schedule.nextDueEpochMinute === null || schedule.nextDueEpochMinute === undefined) {
    return "Next due: none found";
  }
  return `Next due: ${formatEpochMinute(schedule.nextDueEpochMinute)}`;
}

export function formatScheduleRun(run?: { status: string; epochMinute: number } | null) {
  if (!run) return "No run history";
  return `${run.status} - ${formatEpochMinute(run.epochMinute)}`;
}

export function formatScheduleAdminAuditAt(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

export function formatScheduleAdminAuditDetails(event: ScheduleAdminAuditEvent) {
  const details = event.details;
  if (!details) return "no details";
  const parts: string[] = [];
  if (details.mode) parts.push(details.mode);
  if (details.toolset) parts.push(`toolset ${details.toolset}`);
  if (details.executionBackend) parts.push(`backend ${details.executionBackend}`);
  if (details.timeZone) parts.push(details.timeZone);
  if (details.changedFields?.length) {
    parts.push(`changed ${details.changedFields.join(", ")}`);
  }
  if (details.deliveryKinds?.length) {
    parts.push(`delivery ${details.deliveryKinds.join(", ")}`);
  }
  if (details.manual) parts.push("manual");
  if (typeof details.paused === "boolean") {
    parts.push(details.paused ? "paused" : "active");
  }
  if (typeof details.skillCount === "number") {
    parts.push(`${details.skillCount} skills`);
  }
  if (typeof details.scriptAllowlistCount === "number") {
    parts.push(`${details.scriptAllowlistCount} scripts`);
  }
  return parts.length ? parts.join(" / ") : "redacted schedule metadata";
}

export function formatScheduleRunContext(run?: ScheduleRunEvent | null) {
  if (!run) return "No run context";
  const parts: string[] = [run.mode || "agent"];
  if (run.toolset) parts.push(`toolset ${run.toolset}`);
  if (run.executionBackend) parts.push(`backend ${run.executionBackend}`);
  if (run.resolvedExecutionBackend && run.resolvedExecutionBackend !== run.executionBackend) {
    parts.push(`resolved backend ${run.resolvedExecutionBackend}`);
  }
  if (run.executionWorkspaceId) parts.push(`workspace ${run.executionWorkspaceId}`);
  if (run.executionWorkspacePath) parts.push(`workspace path ${run.executionWorkspacePath}`);

  const limits = formatScheduleRunResourceLimits(run.resourceLimits ?? undefined);
  if (limits) parts.push(limits);

  if (run.workdir) parts.push(`workdir ${run.workdir}`);
  if (run.skills?.length) parts.push(`skills ${run.skills.join(", ")}`);
  if (run.deliveryKinds?.length) parts.push(`delivery ${run.deliveryKinds.join(", ")}`);
  if (run.sourceRecipeId) parts.push(`recipe ${run.sourceRecipeId}`);
  return parts.join(" - ");
}

export function formatScheduleRunResourceLimits(
  limits?: {
    cpu?: number | null;
    memoryMb?: number | null;
    diskMb?: number | null;
  } | null,
) {
  if (!limits) return "";
  const parts: string[] = [];
  if (limits.cpu) parts.push(`${limits.cpu} cpu`);
  if (limits.memoryMb) parts.push(`${limits.memoryMb} MB memory`);
  if (limits.diskMb) parts.push(`${limits.diskMb} MB disk`);
  return parts.length ? `limits ${parts.join(", ")}` : "";
}

export function formatScheduleMetadata(schedule: {
  mode?: string;
  toolset?: string | null;
  executionBackend?: string | null;
  skills?: string[] | null;
  delivery?: { kind: string }[] | null;
  maxRuntimeSeconds?: number | null;
}) {
  const parts: string[] = [schedule.mode || "agent"];
  if (schedule.toolset) parts.push(`toolset ${schedule.toolset}`);
  if (schedule.executionBackend) parts.push(`backend ${schedule.executionBackend}`);
  if (schedule.skills?.length) parts.push(`skills ${schedule.skills.join(", ")}`);
  if (schedule.delivery?.length) {
    parts.push(`delivery ${schedule.delivery.map((target) => target.kind).join(", ")}`);
  }
  if (typeof schedule.maxRuntimeSeconds === "number" && schedule.maxRuntimeSeconds) {
    parts.push(`timeout ${schedule.maxRuntimeSeconds}s`);
  }
  return parts.join(" - ");
}

export function formatTeamPresets(team: { maxWorkers?: number | null; budgetUsd?: number | null }) {
  const parts: string[] = [];
  if (team.maxWorkers) parts.push(`max workers ${team.maxWorkers}`);
  if (typeof team.budgetUsd === "number") parts.push(`budget $${team.budgetUsd}`);
  return parts.join(" - ");
}

// Artifact trust helpers rely on ArtifactRow shape; keep same logic as RuntimePane for mechanical extraction.
export function artifactTrustState(artifact: ArtifactRow): {
  label: string;
  tone: "good" | "warn" | "danger" | "neutral";
  title: string;
} {
  const verdict = artifact.verdict;
  const verdictBound = verdict?.boundHash === artifact.contentHash;
  const approvalCount = artifact.approvals.length;
  const validApprovals = artifact.approvals.filter(
    (approval) => approval.boundHash === artifact.contentHash,
  ).length;

  if (artifact.shippable) {
    return {
      label: "Shippable",
      tone: "good",
      title: `Finalized artifact. Approval count: ${approvalCount}; hash: ${artifact.contentHash}`,
    };
  }

  if (verdict && !verdictBound) {
    return {
      label: "Stale verdict",
      tone: "warn",
      title: `Verdict was bound to ${verdict.boundHash}, current hash is ${artifact.contentHash}. Approval count: ${approvalCount}.`,
    };
  }

  if (verdict?.passed) {
    return {
      label: "Passed",
      tone: "good",
      title: `${verdict.juror}: ${verdict.summary}. Approval count: ${approvalCount}; current-hash approvals: ${validApprovals}.`,
    };
  }

  if (verdict) {
    return {
      label: "Rejected",
      tone: "danger",
      title: `${verdict.juror}: ${verdict.summary}. Approval count: ${approvalCount}.`,
    };
  }

  if (validApprovals > 0) {
    return {
      label: "Approval pending verdict",
      tone: "warn",
      title: `${validApprovals} current-hash approval${validApprovals === 1 ? "" : "s"} recorded without a verdict.`,
    };
  }

  return {
    label: "Open",
    tone: "neutral",
    title: `No verdict or Approval recorded. Hash: ${artifact.contentHash}`,
  };
}
