// Checked TypeScript mirror of the daemon/protocol contract.
// Run `npm run check:protocol-mirror` after protocol or session-row changes.

export interface HealthResponse {
  ok: boolean;
  service: string;
  port: number;
  databasePath: string;
  utcNow: string;
}

export type DiagnosticsStatus = "pass" | "warn" | "block";

export interface DiagnosticsCheck {
  id: string;
  label: string;
  status: DiagnosticsStatus;
  detail: string;
  fix: string;
}

export interface DiagnosticsReport {
  verdict: DiagnosticsStatus;
  checks: DiagnosticsCheck[];
}

export type CoreSandboxTier = "landlock-seccomp" | "landlock" | "none";

export interface CoreSandboxSettings {
  workspaceRoot: string;
  storePath: string;
  /** Effective tier after applying the operator floor and workspace tightening. */
  tier: CoreSandboxTier;
  workspaceTier?: CoreSandboxTier | null;
  operatorTier?: CoreSandboxTier | null;
  operatorValid?: boolean;
  workspaceValid?: boolean;
  source: "workspace" | "environment" | "default" | string;
  allowedTiers: CoreSandboxTier[];
}

/** Summary returned by POST /diagnostics/bundle. The bundle is a RightKit ZIP. */
export interface SupportBundle {
  bundleId: string;
  fileName: string;
  byteLength: number;
  crashReports: number;
  nativeArtifacts: number;
  logEvents: number;
  downloadUrl: string;
}

/** Body of GET /diagnostics/bundles/{id}: the ZIP, base64-encoded. */
export interface SupportBundleDownload {
  fileName: string;
  contentType: "application/zip";
  contentBase64: string;
}
