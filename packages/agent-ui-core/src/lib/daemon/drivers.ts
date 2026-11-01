import type {
  DaemonConfig,
  DriverBindingStatus,
  DriverDiscovery,
  DriverKind,
  DriverLease,
  DriverLifecycle,
  DriverSessionStatus,
  ExternalDriverKind,
} from "../types";
import { requestDaemonApi } from "../generated/daemon-openapi";

const capabilityHeaders = (capabilityToken: string) => ({
  "x-coderight-capability-token": capabilityToken,
});

function agentRuntimeUrl(
  config: DaemonConfig,
  sessionId: string,
  agentId: string,
  action: string,
): URL {
  return new URL(
    `${config.url.replace(/\/+$/, "")}/sessions/${encodeURIComponent(sessionId)}/agents/${encodeURIComponent(agentId)}/runtime/${action}`,
  );
}

async function agentRuntimeRequest<T>(
  config: DaemonConfig,
  sessionId: string,
  agentId: string,
  action: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(agentRuntimeUrl(config, sessionId, agentId, action), {
    method: body === undefined ? "GET" : "POST",
    headers: {
      ...(config.token ? { authorization: `Bearer ${config.token}` } : {}),
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`agentRuntime${action} failed: ${response.status}`);
  return (await response.json()) as T;
}

export function isCanonicalAgentRuntimeMissing(error: unknown): boolean {
  return (
    error instanceof Error &&
    /agentRuntime(?:open|prompt|resume|status) failed: 404$/.test(error.message)
  );
}

export function isCanonicalAgentRuntimeAlreadyOpen(error: unknown): boolean {
  return error instanceof Error && /agentRuntimeopen failed: 409$/.test(error.message);
}

export interface OpenAgentRuntimeRequest {
  providerId: ExternalDriverKind;
  modelId?: string;
  accountProfileId: string;
  role: string;
  worktreeId?: string;
  toolCeiling?: string[];
}

export async function openAgentRuntime(
  config: DaemonConfig,
  sessionId: string,
  agentId: string,
  request: OpenAgentRuntimeRequest,
): Promise<{ bindingId: string; vendorSessionId: string | null }> {
  return agentRuntimeRequest(config, sessionId, agentId, "open", {
    runtime: {
      kind: "authenticated_cli",
      providerId: request.providerId,
      modelId: request.modelId ?? null,
      accountProfileId: request.accountProfileId,
    },
    role: request.role,
    worktreeId: request.worktreeId,
    toolCeiling: request.toolCeiling ?? [],
  });
}

export function promptAgentRuntime(
  config: DaemonConfig,
  sessionId: string,
  agentId: string,
  text: string,
  requestLabel?: string,
): Promise<{ accepted?: boolean }> {
  return agentRuntimeRequest(config, sessionId, agentId, "prompt", { text, requestLabel });
}

export function resumeAgentRuntime(
  config: DaemonConfig,
  sessionId: string,
  agentId: string,
): Promise<{ resumed: boolean }> {
  return agentRuntimeRequest(config, sessionId, agentId, "resume", {});
}

export async function discoverDriver(
  config: DaemonConfig,
  driverKind: ExternalDriverKind,
): Promise<DriverDiscovery> {
  return requestDaemonApi<DriverDiscovery>(config, "getDriversByKindDiscovery", {
    pathParams: { kind: driverKind },
  });
}

/**
 * The generated daemon client throws `${operationId} failed: ${status}` for
 * any non-2xx response (e.g. `"getSessionsByIdDriverStatus failed: 404"`).
 * That operationId/status pair is an internal wire-protocol detail — it must
 * never reach the UI verbatim. Recognize that specific shape and translate it
 * into a calm, human message; leave any other error (network failure, abort,
 * etc.) untouched so real diagnostics still surface.
 */
const DAEMON_API_ERROR_PATTERN = /^[A-Za-z0-9_]+ failed: (\d{3})$/;

export function describeDriverStatusError(error: unknown): {
  status: number | null;
  message: string;
} {
  if (error instanceof Error) {
    const match = DAEMON_API_ERROR_PATTERN.exec(error.message);
    if (match) {
      const status = Number(match[1]);
      if (status === 404) {
        return { status, message: "This session hasn't been resumed yet." };
      }
      if (status === 401) {
        return { status, message: "This session's driver access expired." };
      }
      return { status, message: "Driver status could not be loaded." };
    }
    return { status: null, message: error.message };
  }
  return { status: null, message: "Driver status could not be loaded." };
}

export type DriverRecoveryReason = "lost_process" | "unsupported_schema" | "lease_expired";

/**
 * The daemon's wire contract (`engine/bins/coderight/src/api_drivers.rs`)
 * only distinguishes one recovery reason today: `recovery_required`, which
 * `driver_diagnostic()` there defines exactly as a prior driver process
 * dying after claiming durable work — that IS "lost process", engine-backed
 * one-to-one. The other two reasons have no dedicated status from the
 * daemon yet:
 *  - `unsupported_schema` is derived defensively from the real
 *    `version.protocolVersion` field against the only two values
 *    `driver_protocol_version()` in that same file currently emits
 *    ("app-server-jsonrpc", "stream-json"). It cannot fire against today's
 *    daemon; it exists so a future protocol the frontend hasn't shipped
 *    support for reads as "unsupported", not a generic failure.
 *  - `lease_expired` reuses the 401 the daemon's lease-gated driver routes
 *    return for an expired/invalid capability lease (`authorize_lease` in
 *    the same file). The read-only status poll this component calls is
 *    itself unauthenticated, so it cannot produce a 401 today either, but
 *    `describeDriverStatusError` already carries the status code through
 *    for any caller that can.
 */
const SUPPORTED_DRIVER_PROTOCOL_VERSIONS = new Set(["app-server-jsonrpc", "stream-json"]);

export function describeDriverRecovery(
  binding: DriverBindingStatus | null,
  statusErrorCode: number | null,
): DriverRecoveryReason | null {
  if (statusErrorCode === 401) {
    return "lease_expired";
  }
  if (!binding) {
    return null;
  }
  if (binding.lifecycle === "recovery_required") {
    return "lost_process";
  }
  if (
    binding.lifecycle === "failed" &&
    !SUPPORTED_DRIVER_PROTOCOL_VERSIONS.has(binding.version.protocolVersion)
  ) {
    return "unsupported_schema";
  }
  return null;
}

/**
 * `starting`/`leased` with an already-established vendor session means this
 * binding is re-establishing a conductor session it already had, not
 * starting fresh — a real fact carried on `vendorSessionId`, not a guess
 * from how long the poll has been quiet.
 */
export function isReconnectingBinding(binding: DriverBindingStatus | null): boolean {
  return (
    binding !== null &&
    (binding.lifecycle === "starting" || binding.lifecycle === "leased") &&
    binding.vendorSessionId !== null
  );
}

export interface DriverPollSnapshot {
  lifecycle: DriverLifecycle;
  vendorSessionId: string | null;
}

const RESUMABLE_PRIOR_LIFECYCLES = new Set<DriverLifecycle>([
  "interrupted",
  "recovery_required",
  "leased",
  "starting",
  "stopped",
]);
const RESUMED_TARGET_LIFECYCLES = new Set<DriverLifecycle>([
  "running",
  "ready",
  "waiting_for_approval",
]);

/**
 * True only when two REAL consecutive poll snapshots prove the same vendor
 * session came back from a needs-recovery lifecycle to live work. Never
 * driven by elapsed time or a missed-poll count.
 */
export function didResumeFromSnapshot(
  previous: DriverPollSnapshot | null,
  current: DriverPollSnapshot | null,
): boolean {
  return Boolean(
    previous &&
      current &&
      RESUMABLE_PRIOR_LIFECYCLES.has(previous.lifecycle) &&
      RESUMED_TARGET_LIFECYCLES.has(current.lifecycle) &&
      previous.vendorSessionId !== null &&
      previous.vendorSessionId === current.vendorSessionId,
  );
}

export async function getDriverStatus(
  config: DaemonConfig,
  sessionId: string,
): Promise<DriverSessionStatus> {
  try {
    const runtime = await agentRuntimeRequest<{
      providerId?: unknown;
      lifecycle?: unknown;
      vendorSessionId?: unknown;
      vendorTurnId?: unknown;
      diagnostic?: unknown;
      process?: { connected?: unknown; generation?: unknown };
    }>(config, sessionId, sessionId, "status");
    if (typeof runtime.providerId !== "string" || typeof runtime.lifecycle !== "string") {
      throw new Error("Agent runtime returned an invalid status response");
    }
    return {
      sessionId,
      driverKind: runtime.providerId,
      authRequired: false,
      lifecycle: "ready",
      drivers: [
        {
          bindingId: sessionId,
          driverKind: runtime.providerId,
          lifecycle: runtime.lifecycle as DriverBindingStatus["lifecycle"],
          authRequired: false,
          diagnostic: typeof runtime.diagnostic === "string" ? runtime.diagnostic : null,
          capabilities: { transport: "coderight-mcp", requiresLease: true, tools: [] },
          version: {
            executable: runtime.providerId,
            installed: true,
            binaryVersion: null,
            protocolVersion: "runtime-managed",
            schemaHash: null,
          },
          vendorSessionId:
            typeof runtime.vendorSessionId === "string" ? runtime.vendorSessionId : null,
          vendorTurnId: typeof runtime.vendorTurnId === "string" ? runtime.vendorTurnId : null,
          running: runtime.process?.connected === true,
          gatewayState: { state: runtime.process?.connected === true ? "ready" : "starting" },
          processGeneration:
            typeof runtime.process?.generation === "string" ? runtime.process.generation : null,
        },
      ],
    };
  } catch (error) {
    if (!isCanonicalAgentRuntimeMissing(error)) {
      throw error;
    }
    return requestDaemonApi<DriverSessionStatus>(config, "getSessionsByIdDriverStatus", {
      pathParams: { id: sessionId },
    });
  }
}

export async function issueDriverLease(
  config: DaemonConfig,
  sessionId: string,
  request: {
    driverKind: ExternalDriverKind;
    binaryVersion: string;
    schemaHash: string;
    tools?: string[];
    ttlSeconds?: number;
  },
): Promise<DriverLease> {
  return requestDaemonApi<DriverLease>(config, "postSessionsByIdDriverCapabilitiesLeases", {
    pathParams: { id: sessionId },
    body: request,
  });
}

export interface DriverModelOption {
  id: string;
  model: string;
  displayName: string;
  description?: string;
  isDefault?: boolean;
}

/**
 * The models a driver can actually run.
 *
 * Codex answers from its own app-server `model/list`, so the list reflects the
 * user's account rather than anything we hardcode. Claude Code has no local
 * list command, so the daemon returns `source: "static"` and the frontend
 * supplies the documented catalogue instead.
 */
export async function listDriverModels(
  config: DaemonConfig,
  kind: ExternalDriverKind,
): Promise<{ models: DriverModelOption[]; source?: string }> {
  return requestDaemonApi(config, "getDriversByKindModels", {
    pathParams: { kind },
  });
}

export async function startDriver(
  config: DaemonConfig,
  sessionId: string,
  capabilityToken: string,
  bindingId: string,
  executable?: string,
  model?: string,
): Promise<{
  started: boolean;
  bindingId: string;
  driverKind: ExternalDriverKind;
  pid: number | null;
  binaryVersion: string;
}> {
  return requestDaemonApi(config, "postSessionsByIdDriverStart", {
    pathParams: { id: sessionId },
    headers: capabilityHeaders(capabilityToken),
    body: { bindingId, executable, model },
  });
}

export async function resumeDriver(
  config: DaemonConfig,
  sessionId: string,
  capabilityToken: string,
  bindingId: string,
  vendorSessionId: string,
): Promise<{ resumed: boolean; requestId: string | number }> {
  return requestDaemonApi(config, "postSessionsByIdDriverResume", {
    pathParams: { id: sessionId },
    headers: capabilityHeaders(capabilityToken),
    body: { bindingId, vendorSessionId },
  });
}

export async function stopDriver(
  config: DaemonConfig,
  sessionId: string,
  capabilityToken: string,
  bindingId: string,
): Promise<{ stopped: boolean }> {
  return requestDaemonApi(config, "postSessionsByIdDriverStop", {
    pathParams: { id: sessionId },
    headers: capabilityHeaders(capabilityToken),
    body: { bindingId },
  });
}

export async function promptDriver(
  config: DaemonConfig,
  sessionId: string,
  capabilityToken: string,
  bindingId: string,
  vendorSessionId: string,
  text: string,
  requestLabel?: string,
): Promise<{ accepted: boolean; requestId: string | number }> {
  return requestDaemonApi(config, "postSessionsByIdDriverPrompt", {
    pathParams: { id: sessionId },
    headers: capabilityHeaders(capabilityToken),
    body: { bindingId, vendorSessionId, text, requestLabel },
  });
}

export async function interruptDriver(
  config: DaemonConfig,
  sessionId: string,
  capabilityToken: string,
  bindingId: string,
  vendorSessionId: string,
  vendorTurnId?: string | null,
): Promise<{ interrupted: boolean; requestId: string | number }> {
  return requestDaemonApi(config, "postSessionsByIdDriverInterrupt", {
    pathParams: { id: sessionId },
    headers: capabilityHeaders(capabilityToken),
    body: { bindingId, vendorSessionId, vendorTurnId },
  });
}

export function driverKindForSession(kind?: DriverKind): DriverKind {
  return kind ?? "native";
}
