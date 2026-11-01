import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  DaemonConfig,
  DriverBindingStatus,
  DriverLease,
  DriverSessionStatus,
  ExternalDriverKind,
  SessionRow,
} from "@rightkit/agent-ui-core/lib/types";
import {
  discoverDriver,
  getDriverStatus,
  isCanonicalAgentRuntimeAlreadyOpen,
  isCanonicalAgentRuntimeMissing,
  issueDriverLease,
  openAgentRuntime,
  promptAgentRuntime,
  promptDriver,
  resumeAgentRuntime,
  resumeDriver,
  startDriver,
} from "@rightkit/agent-ui-core/lib/daemon";

const DRIVER_STATUS_POLL_MS = 4000;

const PROMPTABLE_LIFECYCLES = new Set<DriverBindingStatus["lifecycle"]>([
  "ready",
  "running",
  "completed",
]);

/**
 * Mirrors the daemon's own acceptance check in
 * `engine/bins/coderight/src/api_drivers.rs::prompt_driver`: a Claude binding
 * is promptable while still "starting" because Claude's stream-json
 * transport only emits its system/init handshake after the first governed
 * user message — that first prompt IS its readiness handshake. Every other
 * kind, and every other lifecycle, must already be ready, running, or
 * between turns before a prompt can be accepted.
 */
function canAcceptPrompt(
  binding: DriverBindingStatus | null,
  driverKind: ExternalDriverKind,
): boolean {
  if (!binding) return false;
  if (PROMPTABLE_LIFECYCLES.has(binding.lifecycle)) return true;
  return driverKind === "claude" && binding.lifecycle === "starting";
}

function driverLabel(kind: ExternalDriverKind): string {
  return (
    (
      {
        claude: "Claude",
        codex: "Codex",
        grok: "Grok Build",
        cursor: "Cursor Agent",
        cline: "Cline",
        commandcode: "Command Code",
      } as Record<string, string>
    )[kind] ?? kind
  );
}

function loginCommandFor(kind: ExternalDriverKind): string {
  return `${kind} login`;
}

/**
 * Lifecycles a driver can be revived from just by reopening the session.
 * `recovery_required` is intentionally absent — it needs an explicit human
 * acknowledgement that claimed work must not be retried.
 */
const RESUMABLE_ON_OPEN = new Set(["stopped", "completed", "interrupted"]);

/** How long to wait for the vendor handshake before giving up on a prompt. */
const VENDOR_SESSION_TIMEOUT_MS = 30_000;
const VENDOR_SESSION_POLL_MS = 250;

export interface UseDriverConductorOptions {
  config: DaemonConfig | null;
  session: SessionRow | null;
  /** Model the user picked for this CLI; the vendor's own default when unset. */
  modelId?: string;
}

export interface UseDriverConductorResult {
  status: DriverSessionStatus | null;
  binding: DriverBindingStatus | null;
  lifecycle: DriverBindingStatus["lifecycle"] | null;
  ready: boolean;
  starting: boolean;
  error: string | null;
  send: (text: string, requestLabel?: string) => Promise<void>;
}

/**
 * Drives an external CLI conductor for the selected session:
 * polls its live binding status, and on `send` ensures a capability lease
 * and a started driver process exist before forwarding the prompt. Inert for
 * a native session (`driverKind === "native"` or unset) — the caller decides
 * the native send path; this hook never polls, leases, or starts anything
 * for it, and `send` rejects rather than silently no-op.
 */
export function useDriverConductor({
  config,
  session,
  modelId,
}: UseDriverConductorOptions): UseDriverConductorResult {
  const sessionId = session?.id ?? null;
  const kind = session?.driverKind;
  const externalKind: ExternalDriverKind | null = kind && kind !== "native" ? kind : null;

  const [status, setStatus] = useState<DriverSessionStatus | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const leaseRef = useRef<DriverLease | null>(null);
  const bindingRef = useRef<DriverBindingStatus | null>(null);

  const binding = useMemo(
    () => status?.drivers.find((row) => row.driverKind === externalKind) ?? null,
    [status, externalKind],
  );
  bindingRef.current = binding;

  const refresh = useCallback(async () => {
    if (!config || !sessionId || !externalKind) return;
    const next = await getDriverStatus(config, sessionId);
    setStatus(next);
    bindingRef.current = next.drivers.find((row) => row.driverKind === externalKind) ?? null;
  }, [config, sessionId, externalKind]);

  // Reset all driver-scoped state whenever the selected session (or its
  // driver kind) changes, so a stale lease or binding from a previous
  // session can never leak into the newly selected one.
  // biome-ignore lint/correctness/useExhaustiveDependencies: sessionId and externalKind are read only to retrigger this reset on session change, not inside the body.
  useEffect(() => {
    leaseRef.current = null;
    bindingRef.current = null;
    setStatus(null);
    setError(null);
  }, [sessionId, externalKind]);

  // Poll only while an external-driver session is selected. No config, no
  // session, or a native session means no interval is ever created.
  useEffect(() => {
    if (!config || !sessionId || !externalKind) return;
    const poll = () => {
      void refresh();
    };
    poll();
    const interval = setInterval(poll, DRIVER_STATUS_POLL_MS);
    return () => clearInterval(interval);
  }, [config, sessionId, externalKind, refresh]);

  // Reconnect a session's driver when you open it.
  //
  // Before this, `resumeDriver` was reachable from exactly one place in the
  // product: a manual button inside Settings. So every session you came back
  // to sat at "Not resumed" until you went looking for it, which is not a
  // state a user should ever have to resolve by hand.
  //
  // `recovery_required` is deliberately excluded: that lifecycle means a prior
  // process died after claiming durable work, and the daemon requires an
  // explicit acknowledgement that those effects must not be retried. Silently
  // resuming it would be the one case where auto-resume can do damage.
  const autoResumedRef = useRef<string | null>(null);
  useEffect(() => {
    if (!config || !sessionId || !externalKind || !binding) return;
    if (!binding.vendorSessionId) return;
    if (!RESUMABLE_ON_OPEN.has(binding.lifecycle)) return;
    if (autoResumedRef.current === binding.bindingId) return;
    autoResumedRef.current = binding.bindingId;
    const vendorSessionId = binding.vendorSessionId;
    void (async () => {
      try {
        try {
          await resumeAgentRuntime(config, sessionId, sessionId);
        } catch (error) {
          if (!isCanonicalAgentRuntimeMissing(error)) throw error;
          const discovery = await discoverDriver(config, externalKind);
          if (!discovery.installed || discovery.authRequired) return;
          const lease = await issueDriverLease(config, sessionId, {
            driverKind: externalKind,
            binaryVersion: binding.version.binaryVersion ?? discovery.version.binaryVersion ?? "",
            schemaHash: binding.version.schemaHash ?? discovery.version.schemaHash ?? "",
          });
          leaseRef.current = lease;
          await resumeDriver(
            config,
            sessionId,
            lease.capabilityToken,
            lease.bindingId,
            vendorSessionId,
          );
        }
        await refresh();
      } catch {
        // Reopening a session must never fail because its driver could not be
        // revived; the strip still reports the real lifecycle and Send will
        // start a driver on demand.
        autoResumedRef.current = null;
      }
    })();
  }, [config, sessionId, externalKind, binding, refresh]);

  const waitForVendorSession = useCallback(
    async (bindingId: string): Promise<string> => {
      const deadline = Date.now() + VENDOR_SESSION_TIMEOUT_MS;
      for (;;) {
        const binding = bindingRef.current;
        if (binding?.bindingId === bindingId && binding.vendorSessionId) {
          return binding.vendorSessionId;
        }
        if (Date.now() >= deadline) {
          throw new Error(
            `${driverLabel(externalKind ?? "codex")} started but never reported a session to prompt.`,
          );
        }
        await new Promise((resolve) => setTimeout(resolve, VENDOR_SESSION_POLL_MS));
        await refresh();
      }
    },
    [externalKind, refresh],
  );

  const ensureLeaseAndStarted = useCallback(async (): Promise<DriverLease | null> => {
    if (!config || !sessionId || !externalKind) {
      throw new Error("No external driver session is selected.");
    }
    const current = bindingRef.current;
    if (leaseRef.current && canAcceptPrompt(current, externalKind)) {
      return leaseRef.current;
    }
    setStarting(true);
    try {
      try {
        await openAgentRuntime(config, sessionId, sessionId, {
          providerId: externalKind,
          modelId: modelId ?? session?.model,
          accountProfileId: `${externalKind}/default`,
          role: session?.role ?? "root",
          worktreeId: session?.executionWorkspaceId ?? undefined,
          toolCeiling: session?.allowedTools ?? [],
        });
        await refresh();
        return null;
      } catch (error) {
        if (current && isCanonicalAgentRuntimeAlreadyOpen(error)) return null;
        if (isCanonicalAgentRuntimeAlreadyOpen(error)) {
          throw new Error(
            `${driverLabel(externalKind)} needs a signed-in, harness-qualified account. Run \`${loginCommandFor(externalKind)}\`, then verify its harness.`,
          );
        }
        if (!isCanonicalAgentRuntimeMissing(error)) throw error;
      }
      const discovery = await discoverDriver(config, externalKind);
      const label = driverLabel(externalKind);
      if (!discovery.installed) {
        throw new Error(`${label} is not installed on this machine.`);
      }
      if (discovery.authRequired) {
        throw new Error(
          `${label} needs authentication. Run \`${loginCommandFor(externalKind)}\`, then try again.`,
        );
      }
      const binaryVersion = current?.version.binaryVersion ?? discovery.version.binaryVersion;
      const schemaHash = current?.version.schemaHash ?? discovery.version.schemaHash;
      if (!binaryVersion || !schemaHash) {
        throw new Error(`${label} discovery did not report a version and schema fingerprint.`);
      }
      const lease = await issueDriverLease(config, sessionId, {
        driverKind: externalKind,
        binaryVersion,
        schemaHash,
      });
      leaseRef.current = lease;
      if (!canAcceptPrompt(current, externalKind)) {
        await startDriver(
          config,
          sessionId,
          lease.capabilityToken,
          lease.bindingId,
          discovery.version.executable,
          modelId,
        );
      }
      return lease;
    } finally {
      setStarting(false);
    }
  }, [config, sessionId, externalKind, modelId, refresh, session]);

  const send = useCallback(
    async (text: string, requestLabel?: string) => {
      if (!externalKind) {
        const failure = new Error(
          "This session has no external driver to prompt — it runs on the native engine.",
        );
        setError(failure.message);
        throw failure;
      }
      if (!config || !sessionId) {
        const failure = new Error("No active session to prompt.");
        setError(failure.message);
        throw failure;
      }
      setError(null);
      try {
        const lease = await ensureLeaseAndStarted();
        if (lease === null) {
          await promptAgentRuntime(config, sessionId, sessionId, text, requestLabel);
          await refresh();
          return;
        }
        // The vendor owns this id — Codex mints it during its app-server
        // handshake and rejects a prompt carrying any other value. Starting the
        // driver returns before that handshake completes, so prompting straight
        // away sent a locally generated UUID and the turn silently produced
        // nothing: driver ready, vendorTurnId null, zero transcript events.
        const vendorSessionId = await waitForVendorSession(lease.bindingId);
        await promptDriver(
          config,
          sessionId,
          lease.capabilityToken,
          lease.bindingId,
          vendorSessionId,
          text,
          requestLabel,
        );
        await refresh();
      } catch (cause) {
        const failure = cause instanceof Error ? cause : new Error("Driver prompt failed.");
        setError(failure.message);
        throw failure;
      }
    },
    [config, sessionId, externalKind, ensureLeaseAndStarted, refresh, waitForVendorSession],
  );

  return {
    status,
    binding,
    lifecycle: binding?.lifecycle ?? null,
    ready: externalKind !== null && canAcceptPrompt(binding, externalKind),
    starting,
    error,
    send,
  };
}
