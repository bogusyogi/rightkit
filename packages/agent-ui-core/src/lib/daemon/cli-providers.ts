import type { DaemonConfig } from "../types";
import type {
  CliAuthMode,
  CliDelegationPolicy,
  CliProviderId,
  CliProviderAuthSource,
  CliProviderAuthState,
  CliTransportKind,
  HarnessIsolationMode,
} from "../types/cliProviders";

export interface DaemonCliProvider {
  providerId: CliProviderId;
  displayName: string;
  installed: boolean;
  executable: string | null;
  binaryVersion: string | null;
  executableSha256: string | null;
  transport: CliTransportKind;
  auth: {
    mode: CliAuthMode;
    /** Compatibility projection of launchReady; consumers use launchReady. */
    signedIn: boolean;
    detectedSignedIn: boolean;
    launchReady: boolean;
    source: CliProviderAuthSource;
    state: CliProviderAuthState;
    accountLabel: string | null;
  };
  harness: {
    eligibility:
      | "ready"
      | "login_required"
      | "harness_verification_required"
      | "unqualified"
      | "version_unsupported";
    attestationId: string | null;
    isolationMode: HarnessIsolationMode | "unknown" | null;
  };
  models: {
    available: boolean;
    source: string | null;
    items: string[];
  };
  repairActions: string[];
}

export interface CliProvidersSnapshot {
  providers: DaemonCliProvider[];
}

export type CliProviderMutation = "login" | "logout" | "verify-harness";

export interface DaemonCliAttestation {
  id: string;
  providerId: CliProviderId;
  executableSha256: string;
  binaryVersion: string;
  protocolVersion: string;
  profileHash: string;
  policyHash: string;
  toolSchemaHash: string;
  operatingSystem: string;
  architecture: string;
  isolationMode: HarnessIsolationMode;
  passedProbes: string[];
  failedProbes: string[];
  verifiedAt: string;
}

function daemonUrl(config: DaemonConfig, path: string): URL {
  return new URL(`${config.url.replace(/\/+$/, "")}${path}`);
}

async function daemonRequest<T>(
  config: DaemonConfig,
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  const response = await fetch(daemonUrl(config, path), {
    method,
    headers: {
      ...(config.token ? { authorization: `Bearer ${config.token}` } : {}),
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    let detail = "";
    try {
      const body = await response.json();
      detail = String(body?.reason ?? body?.error ?? "").trim();
    } catch {
      // A daemon error body is optional; status remains useful.
    }
    throw new Error(detail || `CLI runtime request failed: ${response.status}`);
  }
  return (await response.json()) as T;
}

const AUTH_MODES = new Set(["native_cli_account", "byok_or_external_api", "unknown"]);
const AUTH_SOURCES = new Set(["ambient_cli", "managed_cli"]);
const AUTH_STATES = new Set([
  "unavailable",
  "needs_auth",
  "ambient_authenticated",
  "configured",
  "unqualified",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isNullableString(value: unknown): boolean {
  return value === null || typeof value === "string";
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isCliProvider(value: unknown): boolean {
  if (!isRecord(value) || !isRecord(value.auth) || !isRecord(value.harness) || !isRecord(value.models)) {
    return false;
  }
  const { auth, harness, models } = value;
  return (
    typeof value.providerId === "string" &&
    typeof value.displayName === "string" &&
    typeof value.installed === "boolean" &&
    isNullableString(value.executable) &&
    isNullableString(value.binaryVersion) &&
    isNullableString(value.executableSha256) &&
    typeof value.transport === "string" &&
    typeof auth.mode === "string" &&
    AUTH_MODES.has(auth.mode) &&
    typeof auth.signedIn === "boolean" &&
    typeof auth.detectedSignedIn === "boolean" &&
    typeof auth.launchReady === "boolean" &&
    typeof auth.source === "string" &&
    AUTH_SOURCES.has(auth.source) &&
    typeof auth.state === "string" &&
    AUTH_STATES.has(auth.state) &&
    isNullableString(auth.accountLabel) &&
    typeof harness.eligibility === "string" &&
    isNullableString(harness.attestationId) &&
    isNullableString(harness.isolationMode) &&
    typeof models.available === "boolean" &&
    isNullableString(models.source) &&
    isStringArray(models.items) &&
    isStringArray(value.repairActions)
  );
}

function isCliProvidersSnapshot(value: unknown): value is CliProvidersSnapshot {
  return isRecord(value) && Array.isArray(value.providers) && value.providers.every(isCliProvider);
}

export async function listCliProviders(config: DaemonConfig): Promise<CliProvidersSnapshot> {
  const snapshot = await daemonRequest<unknown>(config, "/cli-providers");
  if (!isCliProvidersSnapshot(snapshot)) {
    throw new Error("CLI runtime returned an invalid provider snapshot");
  }
  return snapshot;
}

export async function getCliProviderAttestation(
  config: DaemonConfig,
  provider: CliProviderId,
): Promise<DaemonCliAttestation | null> {
  const value = await daemonRequest<unknown>(
    config,
    `/cli-providers/${encodeURIComponent(provider)}/attestation`,
  );
  if (value === null) return null;
  if (
    !value ||
    typeof value !== "object" ||
    typeof (value as { id?: unknown }).id !== "string" ||
    !Array.isArray((value as { passedProbes?: unknown }).passedProbes) ||
    !Array.isArray((value as { failedProbes?: unknown }).failedProbes)
  ) {
    throw new Error("CLI runtime returned an invalid harness attestation");
  }
  return value as DaemonCliAttestation;
}

/** Mutations never trust a success-shaped body: refresh live daemon state after acknowledgement. */
export async function mutateCliProvider(
  config: DaemonConfig,
  provider: CliProviderId,
  action: CliProviderMutation,
): Promise<CliProvidersSnapshot> {
  const acknowledgement = await daemonRequest<unknown>(
    config,
    `/cli-providers/${encodeURIComponent(provider)}/${action}`,
    "POST",
  );
  if (!acknowledgement || typeof acknowledgement !== "object") {
    throw new Error("CLI runtime returned an invalid mutation acknowledgement");
  }
  return listCliProviders(config);
}

export function getCliDelegationPolicy(
  config: DaemonConfig,
  sessionId: string,
): Promise<CliDelegationPolicy> {
  return daemonRequest<CliDelegationPolicy>(
    config,
    `/sessions/${encodeURIComponent(sessionId)}/cli-delegation-policy`,
  );
}

export function setCliDelegationPolicy(
  config: DaemonConfig,
  sessionId: string,
  policy: CliDelegationPolicy,
): Promise<CliDelegationPolicy> {
  return daemonRequest<CliDelegationPolicy>(
    config,
    `/sessions/${encodeURIComponent(sessionId)}/cli-delegation-policy`,
    "PUT",
    policy,
  );
}
