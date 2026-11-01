export type PermissionRuleEffect = "allow" | "ask" | "deny";

export interface PermissionRuleRow {
  index: number;
  pattern: string;
  effect: PermissionRuleEffect;
  source: string;
}

export interface PermissionRuleListResponse {
  rules: PermissionRuleRow[];
}

export interface PermissionDenialRow {
  id: string;
  createdAt: string;
  sessionId: string;
  toolCallId: string;
  reason: string;
}

export interface PermissionDenialListResponse {
  permissionDenialStorePath: string;
  denials: PermissionDenialRow[];
}

export type PersistentPermissionRuleScope = "user" | "project";

export interface PersistentPermissionRuleRow {
  index: number;
  pattern: string;
  effect: PermissionRuleEffect;
  scope: PersistentPermissionRuleScope;
  workspaceRoot?: string | null;
}

export interface PersistentPermissionRuleRecord {
  pattern: string;
  effect: PermissionRuleEffect;
}

export interface PersistentPermissionRuleStore {
  user: PersistentPermissionRuleRecord[];
  projects: Record<string, PersistentPermissionRuleRecord[]>;
}

export interface PersistentPermissionRuleListResponse {
  permissionRuleStorePath: string;
  scope: PersistentPermissionRuleScope;
  workspaceRoot?: string | null;
  rules: PersistentPermissionRuleRow[];
}

export type McpServerScope = "user" | "workspace";
export type McpServerType = "http" | "sse" | "stdio";

export type McpServerConfig =
  | { type: "http"; url: string }
  | { type: "sse"; url: string }
  | {
      type: "stdio";
      command: string;
      args?: string[];
      env?: Record<string, string>;
    };

export interface McpServerRow {
  scope: McpServerScope;
  name: string;
  type: McpServerType;
  config: McpServerConfig;
  trust?: McpTrustPolicy;
  channelPermissions?: McpChannelPermissions | null;
  policyDiagnostics?: McpPolicyDiagnostic[];
}

export interface McpTrustPolicy {
  preset: string;
  approvedBy?: string | null;
  approvedAt?: string | null;
  inferred?: boolean;
}

export interface McpChannelPermissions {
  allowedChannels: string[];
  allowMutating: boolean;
}

export interface McpPolicyDiagnostic {
  severity: "info" | "warning" | "error";
  code: string;
  message: string;
}

export interface McpTrustPreset {
  id: string;
  label: string;
  description: string;
}

export interface McpToolProbeRow {
  name: string;
  description: string;
  inputSchema: unknown;
}

export interface McpProbeResult {
  ok: boolean;
  scope: McpServerScope;
  name: string;
  type: McpServerType;
  toolCount: number;
  tools: McpToolProbeRow[];
  latencyMs: number;
  error?: string;
}

export interface McpServerModeStatus {
  command: string;
  readOnlyCommand: string;
  mutatingCommand: string;
  defaultMode: "read_only" | string;
  mutatingRequiresOptIn: boolean;
  readOnlyToolCount: number;
  mutatingToolCount: number;
  totalToolCount: number;
  readOnlyTools: string[];
  mutatingTools: string[];
}

export interface McpListResponse {
  userConfigPath: string;
  workspaceConfigPath?: string | null;
  trustPresets?: McpTrustPreset[];
  servers: McpServerRow[];
}

export interface LspWorkspaceServer {
  language: string;
  extensions: string[];
  serverPath: string;
  serverArgs: string[];
}

export interface LspServerStatus {
  language: string;
  executable: string;
  extensions: string[];
  configuredPath?: string | null;
  discoveredPath?: string | null;
  effectivePath?: string | null;
  serverArgs: string[];
  source: string;
  available: boolean;
}

export interface LspStatusView {
  workspaceRoot: string;
  configPath: string;
  fallbackEnabled: boolean;
  servers: LspServerStatus[];
}
