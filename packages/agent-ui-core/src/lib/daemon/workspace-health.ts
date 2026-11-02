import type {
  CoreSandboxSettings,
  CoreSandboxTier,
  DaemonConfig,
  DiagnosticsReport,
  HealthResponse,
  InstructionStatus,
  SessionRow,
  SupportBundle,
  SupportBundleDownload,
  ToolGatewaySnapshot,
  ToolInvocationCancelResult,
  ToolInvocationEventRow,
  ToolInvocationRecord,
  ToolsetProfile,
} from "../types.js";
import { requestDaemonApi } from "../generated/daemon-openapi.js";
import { invoke } from "@tauri-apps/api/core";

export interface WorkspaceMention {
  kind: "file" | "symbol";
  label: string;
  detail: string;
  insertText: string;
  path: string;
  line?: number | null;
}

export async function searchWorkspaceMentions(
  workspaceRoot: string,
  query: string,
  kind: "file" | "symbol" | "all" = "all",
  limit = 24,
): Promise<WorkspaceMention[]> {
  if (!workspaceRoot.trim()) return [];
  return invoke<WorkspaceMention[]>("search_workspace_mentions", {
    workspaceRoot,
    query,
    kind,
    limit,
  });
}

export async function getHealth(config: DaemonConfig): Promise<HealthResponse> {
  const health = await requestDaemonApi<HealthResponse>(config, "getHealth");
  if (health.service !== "coderight-daemon") {
    throw new Error(`unexpected daemon service: ${health.service}`);
  }
  return health;
}

export async function getDiagnostics(
  config: DaemonConfig,
  workspaceRoot?: string,
): Promise<DiagnosticsReport> {
  return requestDaemonApi<DiagnosticsReport>(config, "getHealthDiagnostics", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function getCoreSandboxSettings(
  config: DaemonConfig,
  workspaceRoot?: string,
): Promise<CoreSandboxSettings> {
  return requestDaemonApi<CoreSandboxSettings>(config, "getSandboxSettings", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
  });
}

export async function saveCoreSandboxSettings(
  config: DaemonConfig,
  tier: CoreSandboxTier,
  workspaceRoot?: string,
): Promise<CoreSandboxSettings> {
  return requestDaemonApi<CoreSandboxSettings>(config, "putSandboxSettings", {
    query: { workspaceRoot: workspaceRoot?.trim() || undefined },
    body: { tier },
  });
}

export async function createDiagnosticsBundle(config: DaemonConfig): Promise<SupportBundle> {
  return requestDaemonApi<SupportBundle>(config, "postDiagnosticsBundle");
}

export async function downloadDiagnosticsBundle(
  config: DaemonConfig,
  bundleId: string,
): Promise<SupportBundleDownload> {
  return requestDaemonApi<SupportBundleDownload>(config, "getDiagnosticsBundlesById", {
    pathParams: { id: bundleId },
  });
}

export async function getInstructions(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<InstructionStatus> {
  return requestDaemonApi<InstructionStatus>(config, "getInstructions", {
    query: workspaceRoot.trim() ? { workspaceRoot: workspaceRoot.trim() } : undefined,
  });
}

export async function listSessions(config: DaemonConfig): Promise<SessionRow[]> {
  return requestDaemonApi<SessionRow[]>(config, "getSessions");
}

export interface SessionSearchHit {
  sessionId: string;
  title: string;
  snippet: string;
}

/** Full-text search across past sessions' transcripts (cross-session recall). */
export async function searchSessions(
  config: DaemonConfig,
  q: string,
  limit = 20,
): Promise<SessionSearchHit[]> {
  return requestDaemonApi<SessionSearchHit[]>(config, "getSessionsSearch", {
    query: { q, limit },
  });
}

export async function listToolsets(config: DaemonConfig): Promise<ToolsetProfile[]> {
  return requestDaemonApi<ToolsetProfile[]>(config, "getToolsets");
}

export async function listToolsetGateways(config: DaemonConfig): Promise<ToolGatewaySnapshot> {
  return requestDaemonApi<ToolGatewaySnapshot>(config, "getToolsetsGateways");
}

export async function listToolInvocations(
  config: DaemonConfig,
  sessionId: string,
): Promise<ToolInvocationRecord[]> {
  return requestDaemonApi<ToolInvocationRecord[]>(config, "getSessionsByIdToolInvocations", {
    pathParams: { id: sessionId },
  });
}

export async function getToolInvocation(
  config: DaemonConfig,
  invocationId: string,
): Promise<ToolInvocationRecord> {
  return requestDaemonApi<ToolInvocationRecord>(config, "getToolInvocationsById", {
    pathParams: { id: invocationId },
  });
}

export async function getToolInvocationEvents(
  config: DaemonConfig,
  invocationId: string,
): Promise<ToolInvocationEventRow[]> {
  return requestDaemonApi<ToolInvocationEventRow[]>(config, "getToolInvocationsByIdEvents", {
    pathParams: { id: invocationId },
  });
}

export async function cancelToolInvocation(
  config: DaemonConfig,
  invocationId: string,
  reason?: string,
): Promise<ToolInvocationCancelResult> {
  return requestDaemonApi<ToolInvocationCancelResult>(config, "postToolInvocationsByIdCancel", {
    pathParams: { id: invocationId },
    body: { reason },
  });
}
