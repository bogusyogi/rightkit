import type { InferenceSelection } from "./inference.js";

export type WorkspaceTrustLevel = "untrusted" | "trusted" | "sandboxed" | "locked";

export interface WorkspaceTrustStatus {
  workspaceRoot: string;
  trust: WorkspaceTrustLevel;
  trusted: boolean;
  requiresTrust: boolean;
  approvedBy?: string | null;
  updatedAt?: string | null;
  storePath?: string;
}

export interface AwaySummary {
  sessionId: string;
  sinceIndex: number;
  nextIndex: number;
  messageCount: number;
  userMessages: number;
  assistantMessages: number;
  toolResults: number;
  errorCount: number;
  latestAt: string;
  hasActivity: boolean;
  bullets: string[];
}

export interface SessionRow {
  id: string;
  title: string;
  model: string;
  inferenceSelection?: InferenceSelection | null;
  workspaceRoot: string;
  createdAt: string;
  updatedAt: string;
  /** True after a real user submission or a compaction marker proving prior chat. */
  hasHistory?: boolean;
  // Lifecycle status: "idle"/"running" for live sessions, "dormant" for sessions
  // that exist only in storage (e.g. after a daemon restart). Optional for compat.
  status?: string;
  role?: string;
  parentSessionId?: string;
  forkedFromSessionId?: string | null;
  safeMode?: boolean;
  tags?: string[];
  allowedTools?: string[];
  disallowedTools?: string[];
  toolset?: string | null;
  activeSkills?: string[];
  executionBackend?: string | null;
  executionWorkspaceId?: string | null;
  executionNetworkProfile?: string | null;
  driverKind?: DriverKind;
  mcpChannel?: string;
  specId?: string | null;
  workspaceTrust?: WorkspaceTrustLevel;
  effort?: "low" | "medium" | "high" | null;
  thinkingBudgetTokens?: number | null;
  disableThinking?: boolean;
  costUsd?: number | null;
}

export interface DispatchDecision {
  strategy: "direct" | "multi-agent" | string;
  rationale: string;
}

export interface ToolsetProfile {
  id: string;
  name: string;
  description: string;
  allowedTools: string[];
  disallowedTools: string[];
  readOnly: boolean;
  defaultBehavioralSkills: string[];
}

export interface SettingsToolsetsResponse {
  workspaceRoot: string;
  path: string;
  configured: ToolsetProfile[];
  effective: ToolsetProfile[];
}

export interface SaveToolsetRequest {
  name?: string;
  description?: string;
  allowedTools?: string[];
  disallowedTools?: string[];
  readOnly?: boolean;
  defaultBehavioralSkills?: string[];
}

export interface ToolGatewayToolStatus {
  id: string;
  available: boolean;
}

export interface ToolGatewayStatus {
  id: string;
  name: string;
  toolset: string;
  description: string;
  status: string;
  provider: string;
  secretStatus: string;
  tools: ToolGatewayToolStatus[];
}

export interface ToolGatewaySnapshot {
  gateways: ToolGatewayStatus[];
}
import type { DriverKind } from "./drivers.js";
