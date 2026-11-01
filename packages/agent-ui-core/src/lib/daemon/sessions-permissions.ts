import type {
  AwaySummary,
  DaemonConfig,
  DriverKind,
  InferenceSelection,
  PermissionDenialListResponse,
  PermissionRuleEffect,
  PermissionRuleListResponse,
  PermissionRuleRow,
  PersistentPermissionRuleListResponse,
  PersistentPermissionRuleScope,
  PersistentPermissionRuleStore,
  ProtocolEvent,
  SessionRow,
  SessionSpecCompletion,
  TranscriptMessage,
  UntrustedContentAudit,
  WorkspaceTrustLevel,
  WorkspaceTrustStatus,
} from "../types";
import { requestDaemonApi } from "../generated/daemon-openapi";
import { tokenHeaders, trimUrl } from "./messages-stream-readiness";

export async function deleteSession(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "deleteSessionsById", {
    pathParams: { id: sessionId },
  });
}

export type PermissionMode = "plan" | "ask" | "accept-edits" | "auto-safe" | "bypass";

export type CommandAction =
  | { none: null }
  | { clear_chat: null }
  | { compact_context: null }
  | { switch_session: string }
  | { list_sessions: null }
  | { submit_message: string }
  | { conduct_goal: string };

export interface CommandInfo {
  name: string;
  description: string;
  usage: string;
}

export interface CommandResult {
  success: boolean;
  output: string;
  action:
    | CommandAction
    | "none"
    | "clear_chat"
    | "compact_context"
    | "list_sessions"
    | "submit_message"
    | "conduct_goal";
}

export async function listCommands(config: DaemonConfig): Promise<CommandInfo[]> {
  return requestDaemonApi<CommandInfo[]>(config, "getCommands");
}

export async function executeCommand(
  config: DaemonConfig,
  input: string,
  sessionId?: string,
): Promise<CommandResult> {
  return requestDaemonApi<CommandResult>(config, "postCommandsExecute", {
    body: { input, sessionId },
  });
}

export async function setPermissionMode(
  config: DaemonConfig,
  sessionId: string,
  mode: PermissionMode,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdPermissionMode", {
    pathParams: { id: sessionId },
    body: { mode },
  });
}

export async function listPermissionRules(
  config: DaemonConfig,
  sessionId: string,
): Promise<PermissionRuleRow[]> {
  const body = await requestDaemonApi<PermissionRuleListResponse>(
    config,
    "getSessionsByIdPermissionRules",
    { pathParams: { id: sessionId } },
  );
  return body.rules;
}

export async function listPermissionDenials(
  config: DaemonConfig,
  sessionId?: string,
  limit = 25,
): Promise<PermissionDenialListResponse> {
  return requestDaemonApi<PermissionDenialListResponse>(config, "getPermissionDenials", {
    query: { sessionId, limit },
  });
}

export async function addPermissionRule(
  config: DaemonConfig,
  sessionId: string,
  pattern: string,
  effect: PermissionRuleEffect,
): Promise<PermissionRuleRow> {
  const body = await requestDaemonApi<{ rule: PermissionRuleRow }>(
    config,
    "postSessionsByIdPermissionRules",
    {
      pathParams: { id: sessionId },
      body: { pattern, effect },
    },
  );
  return body.rule;
}

export async function deletePermissionRule(
  config: DaemonConfig,
  sessionId: string,
  index: number,
): Promise<void> {
  await requestDaemonApi(config, "deleteSessionsByIdPermissionRulesByIndex", {
    pathParams: { id: sessionId, index },
  });
}

export async function listPersistentPermissionRules(
  config: DaemonConfig,
  scope: PersistentPermissionRuleScope,
  workspaceRoot?: string,
): Promise<PersistentPermissionRuleListResponse> {
  return requestDaemonApi<PersistentPermissionRuleListResponse>(config, "getPermissionRules", {
    query: {
      scope,
      workspaceRoot: scope === "project" ? workspaceRoot?.trim() || undefined : undefined,
    },
  });
}

export async function addPersistentPermissionRule(
  config: DaemonConfig,
  body: {
    scope: PersistentPermissionRuleScope;
    pattern: string;
    effect: PermissionRuleEffect;
    workspaceRoot?: string;
  },
): Promise<void> {
  await requestDaemonApi(config, "postPermissionRules", {
    body,
  });
}

export async function deletePersistentPermissionRule(
  config: DaemonConfig,
  rule: {
    index: number;
    scope: PersistentPermissionRuleScope;
    workspaceRoot?: string | null;
  },
): Promise<void> {
  await requestDaemonApi(config, "deletePermissionRulesByIndex", {
    pathParams: { index: rule.index },
    query: {
      scope: rule.scope,
      workspaceRoot: rule.scope === "project" ? rule.workspaceRoot?.trim() || undefined : undefined,
    },
  });
}

export async function exportPermissionRules(
  config: DaemonConfig,
): Promise<PersistentPermissionRuleStore> {
  return requestDaemonApi<PersistentPermissionRuleStore>(config, "getPermissionRulesExport");
}

export async function importPermissionRules(
  config: DaemonConfig,
  store: PersistentPermissionRuleStore,
): Promise<{ imported: boolean; userRules: number; projectScopes: number }> {
  return requestDaemonApi<
    { imported: boolean; userRules: number; projectScopes: number },
    PersistentPermissionRuleStore
  >(config, "postPermissionRulesImport", { body: store });
}

export async function getWorkspaceTrust(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<WorkspaceTrustStatus> {
  return requestDaemonApi<WorkspaceTrustStatus>(config, "getWorkspaceTrust", {
    query: { workspaceRoot },
  });
}

export async function setWorkspaceTrust(
  config: DaemonConfig,
  workspaceRoot: string,
  trust: WorkspaceTrustLevel,
): Promise<WorkspaceTrustStatus> {
  return requestDaemonApi<WorkspaceTrustStatus>(config, "patchWorkspaceTrust", {
    body: { workspaceRoot, trust, approvedBy: "operator" },
  });
}

export async function createSession(
  config: DaemonConfig,
  workspaceRoot: string,
  title = "new session",
  model?: string,
  controls?: {
    allowedTools?: string[];
    disallowedTools?: string[];
    activeSkills?: string[];
    toolset?: string;
    executionBackend?: string;
    executionWorkspaceId?: string;
    driverKind?: DriverKind;
    mcpChannel?: string;
    effort?: "low" | "medium" | "high";
    thinkingBudgetTokens?: number;
    disableThinking?: boolean;
    fableMode?: boolean;
  },
): Promise<SessionRow> {
  return requestDaemonApi<SessionRow>(config, "postSessions", {
    body: {
      workspaceRoot,
      title,
      model,
      allowedTools: controls?.allowedTools,
      disallowedTools: controls?.disallowedTools,
      activeSkills: controls?.activeSkills,
      toolset: controls?.toolset,
      executionBackend: controls?.executionBackend,
      executionWorkspaceId: controls?.executionWorkspaceId,
      driverKind: controls?.driverKind,
      mcpChannel: controls?.mcpChannel,
      effort: controls?.effort,
      thinkingBudgetTokens: controls?.thinkingBudgetTokens,
      disableThinking: controls?.disableThinking,
      fableMode: controls?.fableMode,
    },
  });
}

export async function forkSession(
  config: DaemonConfig,
  sessionId: string,
  title?: string,
): Promise<SessionRow> {
  const body = await requestDaemonApi<{ session?: SessionRow; id?: string }>(
    config,
    "postSessionsByIdFork",
    {
      pathParams: { id: sessionId },
      body: title?.trim() ? { title } : {},
    },
  );
  if (body.session) return body.session;
  if (body.id) {
    return {
      id: body.id,
      title: title?.trim() || `Fork of ${sessionId}`,
      workspaceRoot: "",
      model: "",
      status: "idle",
      role: undefined,
      toolset: null,
      activeSkills: [],
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      parentSessionId: sessionId,
      forkedFromSessionId: sessionId,
    };
  }
  throw new Error("Fork response did not include a session");
}

export async function bindSessionSpec(
  config: DaemonConfig,
  sessionId: string,
  specId: string,
): Promise<SessionRow> {
  return requestDaemonApi<SessionRow>(config, "postSessionsByIdSpec", {
    pathParams: { id: sessionId },
    body: { specId },
  });
}

export async function clearSessionSpec(
  config: DaemonConfig,
  sessionId: string,
): Promise<SessionRow> {
  return requestDaemonApi<SessionRow>(config, "deleteSessionsByIdSpec", {
    pathParams: { id: sessionId },
  });
}

export async function patchSession(
  config: DaemonConfig,
  sessionId: string,
  body: {
    title?: string;
    tags?: string[];
    model?: string;
    inferenceSelection?: InferenceSelection;
    status?: string;
    driverKind?: DriverKind;
  },
): Promise<{
  updated: boolean;
  id: string;
  modelUpdated?: boolean;
  modelAppliesTo?: string | null;
  inferenceSelectionUpdated?: boolean;
  inferenceSelectionAppliesTo?: "live" | "restarted" | "resume" | null;
}> {
  return requestDaemonApi(config, "patchSessionsById", {
    pathParams: { id: sessionId },
    body,
  });
}

export async function getSessionSpecCompletion(
  config: DaemonConfig,
  sessionId: string,
): Promise<SessionSpecCompletion> {
  return requestDaemonApi<SessionSpecCompletion>(config, "getSessionsByIdSpecCompletion", {
    pathParams: { id: sessionId },
  });
}

export async function getTranscript(
  config: DaemonConfig,
  sessionId: string,
): Promise<TranscriptMessage[]> {
  return requestDaemonApi<TranscriptMessage[]>(config, "getSessionsByIdTranscript", {
    pathParams: { id: sessionId },
  });
}

export async function getSessionEventHistory(
  config: DaemonConfig,
  sessionId: string,
  afterSeq = 0,
): Promise<ProtocolEvent[]> {
  try {
    return await requestDaemonApi<ProtocolEvent[]>(config, "getSessionsByIdEventsHistory", {
      pathParams: { id: sessionId },
      query: { after: Math.max(0, afterSeq) },
    });
  } catch (err) {
    // "No events after this cursor" is the normal state on session open and on
    // every catch-up poll once the transcript is current: the daemon answers
    // `after >= maxSeq` with 404, not an empty 200. Treating that 404 as an
    // empty page turns a red "Event history failed" toast — fired on every
    // healthy open of a session with no unseen events — back into what it is:
    // nothing new. Any other status still propagates.
    if (err instanceof Error && / failed: 404\b/.test(err.message)) {
      return [];
    }
    throw err;
  }
}

export async function getUntrustedContent(
  config: DaemonConfig,
  sessionId: string,
): Promise<UntrustedContentAudit> {
  return requestDaemonApi<UntrustedContentAudit>(config, "getSessionsByIdUntrustedContent", {
    pathParams: { id: sessionId },
  });
}

export async function getAwaySummary(
  config: DaemonConfig,
  sessionId: string,
  sinceIndex = 0,
): Promise<AwaySummary> {
  return requestDaemonApi<AwaySummary>(config, "getSessionsByIdAwaySummary", {
    pathParams: { id: sessionId },
    query: { sinceIndex: Math.max(0, sinceIndex) },
  });
}

export async function compactSession(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdCompact", {
    pathParams: { id: sessionId },
  });
}

export async function resolveToolApproval(
  config: DaemonConfig,
  sessionId: string,
  toolCallId: string,
  approved: boolean,
  reason?: string,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdToolApproval", {
    pathParams: { id: sessionId },
    body: { toolCallId, approved, reason },
  });
}

export interface StageDiffHunkRequest {
  toolCallId: string;
  decision: "approve" | "reject";
  hunkId: string;
  hunkIndex: number;
  filePath: string | null;
  hunkHeader: string;
  patch: string;
}

export async function stageDiffHunk(
  config: DaemonConfig,
  sessionId: string,
  body: StageDiffHunkRequest,
): Promise<void> {
  const url = new URL(
    `${trimUrl(config.url)}/sessions/${encodeURIComponent(sessionId)}/tool-approval/hunks`,
  );
  const headers = new Headers(tokenHeaders(config));
  headers.set("content-type", "application/json");
  const response = await fetch(url.toString(), {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`stageDiffHunk failed: ${response.status}`);
  }
}

export async function resumeSession(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdResume", {
    pathParams: { id: sessionId },
  });
}

export async function answerQuestion(
  config: DaemonConfig,
  sessionId: string,
  questionId: string,
  answers: Record<string, string | string[]>,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdAnswerQuestion", {
    pathParams: { id: sessionId },
    body: { questionId, answers },
  });
}

export async function resolvePlan(
  config: DaemonConfig,
  sessionId: string,
  planId: string,
  approved: boolean,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdResolvePlan", {
    pathParams: { id: sessionId },
    body: { planId, approved },
  });
}
