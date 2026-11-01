import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  DaemonConfig,
  DriverBindingStatus,
  DriverDiscovery,
  DriverSessionStatus,
  SessionRow,
} from "@rightkit/agent-ui-core/lib/types";

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  discoverDriver: vi.fn(),
  getDriverStatus: vi.fn(),
  isCanonicalAgentRuntimeAlreadyOpen: vi.fn(),
  isCanonicalAgentRuntimeMissing: vi.fn(),
  issueDriverLease: vi.fn(),
  openAgentRuntime: vi.fn(),
  promptAgentRuntime: vi.fn(),
  startDriver: vi.fn(),
  resumeAgentRuntime: vi.fn(),
  promptDriver: vi.fn(),
  resumeDriver: vi.fn(),
}));

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
import { useDriverConductor } from "./useDriverConductor";

const config: DaemonConfig = { url: "http://127.0.0.1:9171", token: "t" };

function makeSession(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    id: "session-1",
    title: "Session",
    model: "model",
    workspaceRoot: "/workspace",
    createdAt: "2026-08-02T00:00:00Z",
    updatedAt: "2026-08-02T00:00:00Z",
    ...overrides,
  };
}

function makeStatus(drivers: DriverBindingStatus[]): DriverSessionStatus {
  return {
    sessionId: "session-1",
    driverKind: "codex",
    authRequired: false,
    lifecycle: "ready",
    drivers,
  };
}

function makeBinding(overrides: Partial<DriverBindingStatus> = {}): DriverBindingStatus {
  return {
    bindingId: "binding-1",
    driverKind: "codex",
    lifecycle: "running",
    authRequired: false,
    diagnostic: null,
    capabilities: { transport: "coderight-mcp", requiresLease: true, tools: [] },
    version: {
      executable: "codex",
      installed: true,
      binaryVersion: "1.0.0",
      protocolVersion: "app-server-jsonrpc",
      schemaHash: "sha256:abc",
    },
    vendorSessionId: null,
    vendorTurnId: null,
    running: true,
    gatewayState: { state: "ready" },
    processGeneration: null,
    ...overrides,
  };
}

function makeDiscovery(overrides: Partial<DriverDiscovery> = {}): DriverDiscovery {
  return {
    driverKind: "codex",
    installed: true,
    authRequired: false,
    lifecycle: "ready",
    diagnostic: null,
    capabilities: { transport: "coderight-mcp", requiresLease: true, tools: [] },
    version: {
      executable: "codex",
      installed: true,
      binaryVersion: "1.0.0",
      protocolVersion: "app-server-jsonrpc",
      schemaHash: "sha256:abc",
    },
    ...overrides,
  };
}

describe("useDriverConductor", () => {
  beforeEach(() => {
    vi.mocked(discoverDriver).mockReset();
    vi.mocked(getDriverStatus).mockReset();
    vi.mocked(isCanonicalAgentRuntimeAlreadyOpen).mockReset();
    vi.mocked(isCanonicalAgentRuntimeMissing).mockReset();
    vi.mocked(issueDriverLease).mockReset();
    vi.mocked(openAgentRuntime).mockReset();
    vi.mocked(promptAgentRuntime).mockReset();
    vi.mocked(startDriver).mockReset();
    vi.mocked(resumeAgentRuntime).mockReset();
    vi.mocked(promptDriver).mockReset();
    vi.mocked(resumeDriver).mockReset();
    vi.mocked(isCanonicalAgentRuntimeAlreadyOpen).mockReturnValue(false);
    vi.mocked(isCanonicalAgentRuntimeMissing).mockReturnValue(true);
    vi.mocked(openAgentRuntime).mockRejectedValue(new Error("agentRuntimeopen failed: 404"));
    vi.mocked(resumeAgentRuntime).mockRejectedValue(new Error("agentRuntimeresume failed: 404"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("rejects send() on a native session without touching any driver API", async () => {
    const nativeSession = makeSession({ driverKind: "native" });
    const { result } = renderHook(() => useDriverConductor({ config, session: nativeSession }));

    expect(result.current.ready).toBe(false);
    await expect(result.current.send("hello")).rejects.toThrow(/native engine/i);

    expect(getDriverStatus).not.toHaveBeenCalled();
    expect(discoverDriver).not.toHaveBeenCalled();
    expect(issueDriverLease).not.toHaveBeenCalled();
    expect(startDriver).not.toHaveBeenCalled();
    expect(promptDriver).not.toHaveBeenCalled();
  });

  it("issues a lease, starts the driver, then prompts when no binding exists yet", async () => {
    const session = makeSession({ driverKind: "codex" });
    const callOrder: string[] = [];
    // The vendor session id only exists after Codex's handshake, which lands
    // after startDriver returns — the hook must wait for it rather than invent
    // one, or the prompt is delivered against an id the vendor never issued.
    let handshakeDone = false;
    vi.mocked(getDriverStatus).mockImplementation(async () =>
      handshakeDone
        ? makeStatus([makeBinding({ lifecycle: "ready", vendorSessionId: "vendor-real-1" })])
        : makeStatus([]),
    );
    vi.mocked(discoverDriver).mockImplementation(async () => {
      callOrder.push("discover");
      return makeDiscovery();
    });
    vi.mocked(issueDriverLease).mockImplementation(async () => {
      callOrder.push("lease");
      return {
        leaseId: "lease-1",
        bindingId: "binding-1",
        sessionId: session.id,
        driverKind: "codex",
        expiresAt: "2026-08-02T01:00:00Z",
        tools: [],
        capabilityToken: "token-1",
        mcp: {},
      };
    });
    vi.mocked(startDriver).mockImplementation(async () => {
      callOrder.push("start");
      handshakeDone = true;
      return {
        started: true,
        bindingId: "binding-1",
        driverKind: "codex",
        pid: 1,
        binaryVersion: "1.0.0",
      };
    });
    vi.mocked(promptDriver).mockImplementation(async () => {
      callOrder.push("prompt");
      return { accepted: true, requestId: "req-1" };
    });

    const { result } = renderHook(() => useDriverConductor({ config, session }));
    await waitFor(() => expect(getDriverStatus).toHaveBeenCalledTimes(1));

    await act(async () => {
      await result.current.send("do the thing");
    });

    expect(callOrder).toEqual(["discover", "lease", "start", "prompt"]);
    expect(issueDriverLease).toHaveBeenCalledWith(
      config,
      session.id,
      expect.objectContaining({
        driverKind: "codex",
        binaryVersion: "1.0.0",
        schemaHash: "sha256:abc",
      }),
    );
    expect(promptDriver).toHaveBeenCalledWith(
      config,
      session.id,
      "token-1",
      "binding-1",
      "vendor-real-1",
      "do the thing",
      undefined,
    );
  });

  it("opens and prompts a Grok root through canonical agent runtime routes", async () => {
    const session = makeSession({
      driverKind: "grok" as SessionRow["driverKind"],
      allowedTools: ["read_file"],
    });
    vi.mocked(getDriverStatus).mockResolvedValue(makeStatus([]));
    vi.mocked(isCanonicalAgentRuntimeMissing).mockReturnValue(false);
    vi.mocked(openAgentRuntime).mockResolvedValue({
      bindingId: "binding-session-1",
      vendorSessionId: null,
    });
    vi.mocked(promptAgentRuntime).mockResolvedValue({ accepted: true });

    const { result } = renderHook(() => useDriverConductor({ config, session, modelId: "grok-4" }));
    await waitFor(() => expect(getDriverStatus).toHaveBeenCalledTimes(1));

    await act(async () => {
      await result.current.send("do the thing");
    });

    expect(openAgentRuntime).toHaveBeenCalledWith(config, session.id, session.id, {
      providerId: "grok",
      modelId: "grok-4",
      accountProfileId: "grok/default",
      role: "root",
      worktreeId: undefined,
      toolCeiling: ["read_file"],
    });
    expect(promptAgentRuntime).toHaveBeenCalledWith(
      config,
      session.id,
      session.id,
      "do the thing",
      undefined,
    );
    expect(discoverDriver).not.toHaveBeenCalled();
  });

  it("reuses an existing started binding instead of re-leasing on a later send", async () => {
    const session = makeSession({ driverKind: "codex" });
    vi.mocked(discoverDriver).mockResolvedValue(makeDiscovery());
    vi.mocked(issueDriverLease).mockResolvedValue({
      leaseId: "lease-1",
      bindingId: "binding-1",
      sessionId: session.id,
      driverKind: "codex",
      expiresAt: "2026-08-02T01:00:00Z",
      tools: [],
      capabilityToken: "token-1",
      mcp: {},
    });
    vi.mocked(startDriver).mockResolvedValue({
      started: true,
      bindingId: "binding-1",
      driverKind: "codex",
      pid: 1,
      binaryVersion: "1.0.0",
    });
    vi.mocked(promptDriver).mockResolvedValue({ accepted: true, requestId: "req-1" });

    const runningBinding = makeBinding({ lifecycle: "running", vendorSessionId: "vendor-99" });
    vi.mocked(getDriverStatus)
      .mockResolvedValueOnce(makeStatus([])) // initial poll on mount: nothing yet
      .mockResolvedValue(makeStatus([runningBinding])); // every refresh after that

    const { result } = renderHook(() => useDriverConductor({ config, session }));
    await waitFor(() => expect(getDriverStatus).toHaveBeenCalledTimes(1));

    await act(async () => {
      await result.current.send("first");
    });
    expect(discoverDriver).toHaveBeenCalledTimes(1);
    expect(issueDriverLease).toHaveBeenCalledTimes(1);
    expect(startDriver).toHaveBeenCalledTimes(1);

    await waitFor(() => expect(result.current.binding?.lifecycle).toBe("running"));

    await act(async () => {
      await result.current.send("second");
    });

    // The binding is already active and the lease from the first send is
    // still held — no second discover/lease/start, just another prompt.
    expect(discoverDriver).toHaveBeenCalledTimes(1);
    expect(issueDriverLease).toHaveBeenCalledTimes(1);
    expect(startDriver).toHaveBeenCalledTimes(1);
    expect(promptDriver).toHaveBeenCalledTimes(2);
    expect(promptDriver).toHaveBeenLastCalledWith(
      config,
      session.id,
      "token-1",
      "binding-1",
      "vendor-99",
      "second",
      undefined,
    );
  });

  it("surfaces a clear error when discovery reports authentication is required", async () => {
    const session = makeSession({ driverKind: "claude" });
    vi.mocked(getDriverStatus).mockResolvedValue(makeStatus([]));
    vi.mocked(discoverDriver).mockResolvedValue(
      makeDiscovery({ driverKind: "claude", authRequired: true }),
    );

    const { result } = renderHook(() => useDriverConductor({ config, session }));
    await waitFor(() => expect(getDriverStatus).toHaveBeenCalledTimes(1));

    await expect(result.current.send("hello")).rejects.toThrow(/authentication/i);

    expect(discoverDriver).toHaveBeenCalledTimes(1);
    expect(issueDriverLease).not.toHaveBeenCalled();
    expect(startDriver).not.toHaveBeenCalled();
    expect(promptDriver).not.toHaveBeenCalled();
    await waitFor(() => expect(result.current.error).toMatch(/authentication/i));
  });

  it("stops polling driver status once the hook unmounts", async () => {
    vi.useFakeTimers();
    const session = makeSession({ driverKind: "codex" });
    vi.mocked(getDriverStatus).mockResolvedValue(makeStatus([]));

    const { unmount } = renderHook(() => useDriverConductor({ config, session }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(getDriverStatus).toHaveBeenCalledTimes(1);

    unmount();
    const callsAtUnmount = vi.mocked(getDriverStatus).mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(20_000);
    });

    expect(getDriverStatus).toHaveBeenCalledTimes(callsAtUnmount);
  });

  // Reopening a session used to leave its driver dead: resumeDriver was only
  // reachable from a manual button inside Settings.
  it("reconnects a stopped driver as soon as the session is opened", async () => {
    const session = makeSession({ driverKind: "codex" });
    vi.mocked(discoverDriver).mockResolvedValue(makeDiscovery());
    vi.mocked(issueDriverLease).mockResolvedValue({
      leaseId: "lease-1",
      bindingId: "binding-1",
      sessionId: session.id,
      driverKind: "codex",
      expiresAt: "2026-08-02T01:00:00Z",
      tools: [],
      capabilityToken: "token-1",
      mcp: {},
    });
    vi.mocked(resumeDriver).mockResolvedValue({ resumed: true, requestId: "req-1" });
    vi.mocked(getDriverStatus).mockResolvedValue(
      makeStatus([makeBinding({ lifecycle: "stopped", vendorSessionId: "vendor-7" })]),
    );

    renderHook(() => useDriverConductor({ config, session }));

    await waitFor(() => expect(resumeDriver).toHaveBeenCalledTimes(1));
    expect(resumeDriver).toHaveBeenCalledWith(
      config,
      session.id,
      "token-1",
      "binding-1",
      "vendor-7",
    );
  });

  // The one lifecycle auto-resume must never touch: a prior process died after
  // claiming durable work, and reviving it without an explicit acknowledgement
  // risks repeating those effects.
  it("never auto-resumes a binding that needs recovery", async () => {
    const session = makeSession({ driverKind: "codex" });
    vi.mocked(discoverDriver).mockResolvedValue(makeDiscovery());
    vi.mocked(getDriverStatus).mockResolvedValue(
      makeStatus([makeBinding({ lifecycle: "recovery_required", vendorSessionId: "vendor-7" })]),
    );

    renderHook(() => useDriverConductor({ config, session }));

    await waitFor(() => expect(getDriverStatus).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(resumeDriver).not.toHaveBeenCalled();
  });
});
