import type {
  DaemonConfig,
  LspStatusView,
  LspWorkspaceServer,
  McpChannelPermissions,
  McpListResponse,
  McpProbeResult,
  McpServerConfig,
  McpServerModeStatus,
  McpServerScope,
  ScheduleAdminAuditResponse,
  ScheduleListResponse,
  ScheduleRunsResponse,
  ScheduleWriteRequest,
  ScheduleWriteResponse,
  SkillBenchmarkVerdict,
  SkillGovernanceResponse,
  SkillItem,
  SkillListResponse,
  SkillProposalBenchmarkRunRequest,
  SkillProposalBenchmarkRunResponse,
  SkillProposalRecord,
  SkillProvenanceReviewRequest,
  SkillProvenanceReviewResponse,
  SkillReadResponse,
  SkillSaveResponse,
} from "../types.js";
import { requestDaemonApi } from "../generated/daemon-openapi.js";

export interface BotProfileView {
  schemaVersion: 1;
  id: string;
  name: string;
  glyph?: string;
  description?: string;
  version: number;
  rolePack: string;
  model: string;
  fallbackModels?: string[];
  instructions?: string;
  memoryPolicy?: string;
  project?: string | null;
  defaultProject?: string | null;
  taskTemplates?: string[];
  tools?: string[];
  toolGrants?: string[];
  permissions?: string[];
  browser?: { mode: string; profile?: string | null; allowNetwork?: boolean };
  budget?: { maxTokens?: number | null; maxSeconds?: number | null; maxCostMicros?: number | null };
  verifier?: { rolePack?: string | null; model?: string | null; required?: boolean };
  triggers?: Array<{ id: string; schedule: string; expression?: string | null; paused?: boolean }>;
  concurrency: string;
  concurrencyCap: number;
  enabled: boolean;
}

export interface BotListItem {
  id: string;
  name: string;
  updatedAt: string;
}

export async function listBots(config: DaemonConfig): Promise<BotListItem[]> {
  const response = await requestDaemonApi<{ bots: BotListItem[] }>(config, "getBots");
  return response.bots;
}

export function getBot(config: DaemonConfig, id: string): Promise<BotProfileView> {
  return requestDaemonApi<BotProfileView>(config, "getBotsById", { pathParams: { id } });
}

export function createBot(
  config: DaemonConfig,
  profile: BotProfileView,
  workspaceRoot: string,
): Promise<BotProfileView> {
  return requestDaemonApi<BotProfileView, BotProfileView>(config, "postBots", {
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
    body: profile,
  });
}

export function updateBot(
  config: DaemonConfig,
  profile: BotProfileView,
  workspaceRoot = "",
): Promise<BotProfileView> {
  return requestDaemonApi<BotProfileView, BotProfileView>(config, "patchBotsById", {
    pathParams: { id: profile.id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
    body: profile,
  });
}

export async function deleteBot(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<void> {
  await requestDaemonApi(config, "deleteBotsById", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export function runBot(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<{ submitted: boolean; botId: string; scheduleId: string }> {
  return requestDaemonApi<{ submitted: boolean; botId: string; scheduleId: string }>(
    config,
    "postBotsByIdRun",
    {
      pathParams: { id },
      query: { workspaceRoot: workspaceRoot.trim() || undefined },
    },
  );
}

export function importBot(config: DaemonConfig, packageJson: string): Promise<BotProfileView> {
  return requestDaemonApi<BotProfileView, unknown>(config, "postBotsImport", {
    body: JSON.parse(packageJson) as unknown,
  });
}

export function exportBot(config: DaemonConfig, id: string): Promise<Record<string, unknown>> {
  return requestDaemonApi<Record<string, unknown>>(config, "getBotsByIdExport", {
    pathParams: { id },
  });
}

export async function deleteTeam(config: DaemonConfig, name: string): Promise<void> {
  await requestDaemonApi(config, "deleteTeamsByName", {
    pathParams: { name },
  });
}

export async function listSchedules(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<ScheduleListResponse> {
  return requestDaemonApi<ScheduleListResponse>(config, "getSchedules", {
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function listScheduleAdminAudit(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<ScheduleAdminAuditResponse> {
  return requestDaemonApi<ScheduleAdminAuditResponse>(config, "getSchedulesAudit", {
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

// POST /schedules requires cronExpr + prompt (the Rust CreateScheduleRequest has
// neither as Option). Tighten the create signature so callers can't omit them
// and trip a 422; PATCH keeps the all-optional ScheduleWriteRequest.
export type CreateScheduleRequest = ScheduleWriteRequest & {
  cronExpr: string;
  prompt: string;
};

export async function createSchedule(
  config: DaemonConfig,
  workspaceRoot: string,
  schedule: CreateScheduleRequest,
): Promise<ScheduleWriteResponse> {
  return requestDaemonApi<ScheduleWriteResponse, CreateScheduleRequest>(config, "postSchedules", {
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
    body: schedule,
  });
}

export async function patchSchedule(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
  patch: ScheduleWriteRequest,
): Promise<ScheduleWriteResponse> {
  return requestDaemonApi<ScheduleWriteResponse, ScheduleWriteRequest>(
    config,
    "patchSchedulesById",
    {
      pathParams: { id },
      query: { workspaceRoot: workspaceRoot.trim() || undefined },
      body: patch,
    },
  );
}

export async function deleteSchedule(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<void> {
  await requestDaemonApi(config, "deleteSchedulesById", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function getScheduleRuns(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<ScheduleRunsResponse> {
  return requestDaemonApi<ScheduleRunsResponse>(config, "getSchedulesByIdRuns", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function saveScheduleTimeZone(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
  timeZone: string,
): Promise<void> {
  await requestDaemonApi(config, "putSchedulesByIdTimeZone", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
    body: { timeZone },
  });
}

export async function runScheduleNow(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<void> {
  await requestDaemonApi(config, "postSchedulesByIdRun", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function pauseSchedule(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<void> {
  await requestDaemonApi(config, "postSchedulesByIdPause", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function resumeSchedule(
  config: DaemonConfig,
  id: string,
  workspaceRoot: string,
): Promise<void> {
  await requestDaemonApi(config, "postSchedulesByIdResume", {
    pathParams: { id },
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function listMcpServers(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<McpListResponse> {
  return requestDaemonApi<McpListResponse>(config, "getMcp", {
    query: { workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function getMcpServerModeStatus(config: DaemonConfig): Promise<McpServerModeStatus> {
  return requestDaemonApi<McpServerModeStatus>(config, "getMcpServerStatus");
}

export async function saveMcpServer(
  config: DaemonConfig,
  body: {
    scope: McpServerScope;
    workspaceRoot?: string;
    name: string;
    config: McpServerConfig;
    trustPreset?: string;
    approvedBy?: string;
    channelPermissions?: McpChannelPermissions;
  },
): Promise<void> {
  await requestDaemonApi(config, "postMcpServers", { body });
}

export async function deleteMcpServer(
  config: DaemonConfig,
  name: string,
  scope: McpServerScope,
  workspaceRoot: string,
): Promise<void> {
  await requestDaemonApi(config, "deleteMcpServersByName", {
    pathParams: { name },
    query: { scope, workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function probeMcpServer(
  config: DaemonConfig,
  name: string,
  scope: McpServerScope,
  workspaceRoot: string,
): Promise<McpProbeResult> {
  return requestDaemonApi<McpProbeResult>(config, "postMcpServersByNameProbe", {
    pathParams: { name },
    query: { scope, workspaceRoot: workspaceRoot.trim() || undefined },
  });
}

export async function getLspStatus(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<LspStatusView> {
  return requestDaemonApi<LspStatusView>(config, "getLsp", {
    query: { workspaceRoot },
  });
}

export async function saveLspConfig(
  config: DaemonConfig,
  workspaceRoot: string,
  servers: LspWorkspaceServer[],
): Promise<LspStatusView> {
  return requestDaemonApi<LspStatusView, { workspaceRoot: string; servers: LspWorkspaceServer[] }>(
    config,
    "putLsp",
    { body: { workspaceRoot, servers } },
  );
}

export async function listSkills(config: DaemonConfig): Promise<SkillListResponse> {
  return requestDaemonApi<SkillListResponse>(config, "getSkills");
}

export async function readSkill(config: DaemonConfig, slug: string): Promise<SkillReadResponse> {
  return requestDaemonApi<SkillReadResponse>(config, "getSkillsBySlug", {
    pathParams: { slug },
  });
}

export async function installSkill(
  config: DaemonConfig,
  body: {
    slug: string;
    content: string;
    name?: string;
    description?: string;
    trigger?: string;
    enabled?: boolean;
    category?: "tool" | "behavioral";
    priority?: number;
    tags?: string[];
  },
): Promise<{ installed: boolean; skillDir: string; skill: SkillItem }> {
  return requestDaemonApi<{ installed: boolean; skillDir: string; skill: SkillItem }, typeof body>(
    config,
    "postSkillsInstall",
    { body },
  );
}

export async function reviewSkillProvenance(
  config: DaemonConfig,
  body: SkillProvenanceReviewRequest,
): Promise<SkillProvenanceReviewResponse> {
  return requestDaemonApi<SkillProvenanceReviewResponse, SkillProvenanceReviewRequest>(
    config,
    "postSkillsProvenanceReview",
    { body },
  );
}

export async function saveSkill(
  config: DaemonConfig,
  slug: string,
  body: {
    content: string;
    enabled?: boolean;
  },
): Promise<SkillSaveResponse> {
  return requestDaemonApi<SkillSaveResponse, typeof body>(config, "putSkillsBySlug", {
    pathParams: { slug },
    body,
  });
}

export async function deleteSkill(config: DaemonConfig, slug: string): Promise<void> {
  await requestDaemonApi(config, "deleteSkillsBySlug", {
    pathParams: { slug },
  });
}

export async function listSkillGovernance(config: DaemonConfig): Promise<SkillGovernanceResponse> {
  return requestDaemonApi<SkillGovernanceResponse>(config, "getSkillsProposals");
}

export async function quarantineSkillProposal(
  config: DaemonConfig,
  proposalId: string,
  reason?: string,
): Promise<{ quarantined: boolean; proposal: SkillProposalRecord; skill: SkillItem }> {
  return requestDaemonApi<
    { quarantined: boolean; proposal: SkillProposalRecord; skill: SkillItem },
    { reason?: string }
  >(config, "postSkillsProposalsByIdQuarantine", {
    pathParams: { id: proposalId },
    body: { reason },
  });
}

export async function recordSkillProposalBenchmark(
  config: DaemonConfig,
  proposalId: string,
  body: {
    benchmarkId: string;
    gateMode: "hard" | "soft" | "mixed";
    scoreContinuous: number;
    passed: boolean;
    reason?: string;
  },
): Promise<{
  recorded: boolean;
  proposal: SkillProposalRecord;
  benchmarkVerdict: SkillBenchmarkVerdict;
}> {
  return requestDaemonApi<
    { recorded: boolean; proposal: SkillProposalRecord; benchmarkVerdict: SkillBenchmarkVerdict },
    typeof body
  >(config, "postSkillsProposalsByIdBenchmark", {
    pathParams: { id: proposalId },
    body,
  });
}

export async function runSkillProposalBenchmark(
  config: DaemonConfig,
  proposalId: string,
  body: SkillProposalBenchmarkRunRequest,
): Promise<SkillProposalBenchmarkRunResponse> {
  return requestDaemonApi<SkillProposalBenchmarkRunResponse, SkillProposalBenchmarkRunRequest>(
    config,
    "postSkillsProposalsByIdRunBenchmark",
    {
      pathParams: { id: proposalId },
      body,
    },
  );
}

export async function transitionSkillLifecycle(
  config: DaemonConfig,
  slug: string,
  action: "quarantine" | "activate" | "retire",
  body: { reason?: string; operatorOverride?: boolean } = {},
): Promise<{ updated: boolean; action: string; skill: SkillItem; content: string }> {
  if (action === "quarantine") {
    return requestDaemonApi<
      { updated: boolean; action: string; skill: SkillItem; content: string },
      typeof body
    >(config, "postSkillsBySlugQuarantine", {
      pathParams: { slug },
      body,
    });
  }
  if (action === "activate") {
    return requestDaemonApi<
      { updated: boolean; action: string; skill: SkillItem; content: string },
      typeof body
    >(config, "postSkillsBySlugActivate", {
      pathParams: { slug },
      body,
    });
  }
  return requestDaemonApi<
    { updated: boolean; action: string; skill: SkillItem; content: string },
    typeof body
  >(config, "postSkillsBySlugRetire", {
    pathParams: { slug },
    body,
  });
}
