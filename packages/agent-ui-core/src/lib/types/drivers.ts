export type DriverKind =
  | "native"
  | "codex"
  | "claude"
  | "grok"
  | "cursor"
  | "cline"
  | "commandcode"
  | (string & {});
export type ExternalDriverKind = Exclude<DriverKind, "native">;

export type DriverLifecycle =
  | "unavailable"
  | "authentication_required"
  | "leased"
  | "starting"
  | "ready"
  | "running"
  | "waiting_for_approval"
  | "interrupted"
  | "completed"
  | "recovery_required"
  | "failed"
  | "stopped";

export type DriverGatewayState =
  | { state: "starting" }
  | { state: "ready" }
  | { state: "failed"; message: string };

export interface DriverVersion {
  executable: string;
  installed: boolean;
  binaryVersion: string | null;
  protocolVersion: string;
  schemaHash: string | null;
}

export interface DriverCapability {
  id: string;
  requiresApproval: boolean;
}

export interface DriverCapabilities {
  transport: "coderight-mcp";
  requiresLease: true;
  tools: DriverCapability[];
}

export interface DriverDiscovery {
  driverKind: ExternalDriverKind;
  installed: boolean;
  authRequired: boolean;
  lifecycle: "ready" | "unavailable" | "authentication_required";
  diagnostic: string | null;
  capabilities: DriverCapabilities;
  version: DriverVersion;
}

export interface DriverBindingStatus {
  bindingId: string;
  driverKind: ExternalDriverKind;
  lifecycle: Exclude<DriverLifecycle, "unavailable" | "authentication_required">;
  authRequired: boolean;
  diagnostic: string | null;
  capabilities: DriverCapabilities;
  version: DriverVersion;
  vendorSessionId: string | null;
  vendorTurnId: string | null;
  running: boolean;
  gatewayState: DriverGatewayState;
  processGeneration: string | null;
}

export interface DriverSessionStatus {
  sessionId: string;
  driverKind: DriverKind;
  authRequired: boolean;
  /**
   * `not_resumed` means the session exists in storage (e.g. restored from
   * Recents) but has not been resumed into the daemon's in-memory session
   * map yet — a calm, truthful state, not an error.
   */
  lifecycle: "native" | "ready" | "authentication_required" | "not_resumed";
  drivers: DriverBindingStatus[];
}

export interface DriverLease {
  leaseId: string;
  bindingId: string;
  sessionId: string;
  driverKind: ExternalDriverKind;
  expiresAt: string;
  tools: string[];
  capabilityToken: string;
  mcp: Record<string, unknown>;
}
