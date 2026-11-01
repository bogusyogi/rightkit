export type ExecutionBackendKind = "local";
export type ExecutionNetworkDefault = "allow" | "deny";
export type ExecutionDoctorVerdict = "pass" | "warn" | "block";

export interface WorkspaceLayout {
  root: string;
  uploads: string;
  workspace: string;
  outputs: string;
  logs: string;
  skills: string;
  metadata: string;
}

export interface ResourceLimits {
  cpu?: number | null;
  memoryMb?: number | null;
  diskMb?: number | null;
}

export interface BackendCapabilities {
  run: boolean;
  spawn: boolean;
  poll: boolean;
  stdin: boolean;
  kill: boolean;
  networkIsolation: boolean;
  persistentWorkspace: boolean;
}

export interface ExecutionBackendConfig {
  id: string;
  name: string;
  kind: ExecutionBackendKind;
  enabled: boolean;
  persistent: boolean;
  workspaceLayout: WorkspaceLayout;
  networkProfile?: string | null;
  resourceLimits: ResourceLimits;
  supports: BackendCapabilities;
}

export interface ExecutionNetworkProfile {
  id: string;
  name: string;
  default: ExecutionNetworkDefault;
  allowHosts: string[];
  allowPrivateNetworks: boolean;
  denyPrivateNetworks: boolean;
}

export interface ExecutionBackendRegistry {
  defaultBackend: string;
  backends: ExecutionBackendConfig[];
  networkProfiles: ExecutionNetworkProfile[];
}

export interface BackendDoctorCheck {
  id: string;
  label: string;
  verdict: ExecutionDoctorVerdict;
  detail: string;
  fix: string;
}

export interface BackendDoctorReport {
  backendId: string;
  verdict: ExecutionDoctorVerdict;
  checks: BackendDoctorCheck[];
}

export interface ExecutionWorkspacePaths {
  root: string;
  uploads: string;
  workspace: string;
  outputs: string;
  logs: string;
  skills: string;
  metadata: string;
}

export interface ExecutionWorkspaceRecord {
  id: string;
  backendId: string;
  name: string;
  status: "ready" | "running" | "stopped" | "error" | string;
  paths: ExecutionWorkspacePaths;
  createdAt: string;
  updatedAt: string;
}

export interface ExecutionWorkspaceListResponse {
  workspaceRoot: string;
  storePath: string;
  workspaces: ExecutionWorkspaceRecord[];
}

export interface CreateExecutionWorkspaceResponse {
  workspaceRoot: string;
  storePath: string;
  workspace: ExecutionWorkspaceRecord;
}

export interface ExecutionProcessRow {
  id: string;
  kind: "backgroundShell" | string;
  sessionId: string;
  workspaceRoot: string;
  backendId: string;
  executionWorkspaceId?: string | null;
  networkProfile?: string | null;
  command: string;
  status: "running" | "exited" | "killed" | "spawnFailed" | string;
  statusLabel: string;
  live: boolean;
  controlMode: "liveProcess" | "historicalUnavailable" | string;
  controlUnavailableReason?: string;
  lastSeenAt?: string;
  output?: string;
}

export interface ExecutionProcessListResponse {
  storePath?: string;
  processes: ExecutionProcessRow[];
}

export interface ExecutionProcessResponse {
  process: ExecutionProcessRow;
}

export interface ExecutionProcessLogs {
  processId: string;
  output: string;
  byteLength: number;
  tailBytes: number;
  truncated: boolean;
}

export interface ExecutionProcessLogsResponse {
  process: ExecutionProcessRow;
  logs: ExecutionProcessLogs;
}

export interface WriteExecutionProcessStdinRequest {
  input: string;
  appendNewline?: boolean;
}
