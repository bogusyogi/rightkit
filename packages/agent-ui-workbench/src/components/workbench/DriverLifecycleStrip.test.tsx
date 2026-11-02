// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import DriverLifecycleStrip from "./DriverLifecycleStrip.js";

const drivers = vi.hoisted(() => ({
  getStatus: vi.fn(),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  getDriverStatus: drivers.getStatus,
}));

function driverStatus(
  lifecycle:
    | "leased"
    | "starting"
    | "ready"
    | "running"
    | "waiting_for_approval"
    | "interrupted"
    | "completed"
    | "recovery_required"
    | "failed",
  diagnostic: string | null = null,
  overrides: { vendorSessionId?: string | null; protocolVersion?: string } = {},
) {
  return {
    sessionId: "session-1",
    driverKind: "codex",
    authRequired: false,
    lifecycle: "ready",
    drivers: [
      {
        bindingId: "binding-1",
        driverKind: "codex",
        lifecycle,
        authRequired: false,
        diagnostic,
        capabilities: {
          transport: "coderight-mcp",
          requiresLease: true,
          tools: [{ id: "read", requiresApproval: false }],
        },
        version: {
          executable: "/usr/local/bin/codex",
          installed: true,
          binaryVersion: "codex-cli 1.2.3",
          protocolVersion: overrides.protocolVersion ?? "app-server-jsonrpc",
          schemaHash: "schema",
        },
        vendorSessionId:
          overrides.vendorSessionId === undefined ? "vendor-session" : overrides.vendorSessionId,
        vendorTurnId: "vendor-turn",
        running: lifecycle === "running",
        gatewayState: { state: "ready" },
        processGeneration: "generation-1",
      },
    ],
  };
}

describe("DriverLifecycleStrip", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    drivers.getStatus.mockResolvedValue(driverStatus("running"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows active external conductor status in session context", async () => {
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={() => {}}
      />,
    );

    await waitFor(() => expect(screen.getByText("Active")).toBeTruthy());
    expect(screen.getByText("Codex")).toBeTruthy();
    expect(screen.getByText(/gateway connected/)).toBeTruthy();
    expect(screen.getByTestId("driver-lifecycle-strip").getAttribute("data-driver-lifecycle")).toBe(
      "running",
    );
  });

  it("routes waiting, interruption, and management", async () => {
    const onManage = vi.fn();
    drivers.getStatus.mockResolvedValue(
      driverStatus("waiting_for_approval", "Approval required for shell."),
    );
    const { rerender } = render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={onManage}
      />,
    );

    await waitFor(() => expect(screen.getByText("Waiting for approval")).toBeTruthy());
    expect(screen.getByText("Approval required for shell.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Manage" }));
    expect(onManage).toHaveBeenCalledOnce();

    drivers.getStatus.mockResolvedValue(driverStatus("interrupted"));
    rerender(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-2"
        driverKind="codex"
        onManage={onManage}
      />,
    );
    await waitFor(() => expect(screen.getByText("Interrupted")).toBeTruthy());
  });

  it("names a lost driver process and offers exactly one restart action, engine-backed by recovery_required", async () => {
    const onManage = vi.fn();
    drivers.getStatus.mockResolvedValue(
      driverStatus("recovery_required", "A prior driver process stopped after claiming work."),
    );
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-3"
        driverKind="codex"
        onManage={onManage}
      />,
    );

    await waitFor(() => expect(screen.getByText("Driver lost")).toBeTruthy());
    expect(screen.getByText("A prior driver process stopped after claiming work.")).toBeTruthy();
    expect(screen.getByTestId("driver-lifecycle-strip").getAttribute("data-driver-recovery")).toBe(
      "lost_process",
    );
    expect(screen.queryByRole("button", { name: "Manage" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Restart driver" }));
    expect(onManage).toHaveBeenCalledOnce();
  });

  it("flags an unsupported driver protocol version and offers to open conductor settings", async () => {
    const onManage = vi.fn();
    drivers.getStatus.mockResolvedValue(
      driverStatus("failed", null, { protocolVersion: "future-protocol-v9" }),
    );
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={onManage}
      />,
    );

    await waitFor(() => expect(screen.getByText("Unsupported driver version")).toBeTruthy());
    expect(screen.getByText(/future-protocol-v9/)).toBeTruthy();
    expect(screen.getByTestId("driver-lifecycle-strip").getAttribute("data-driver-recovery")).toBe(
      "unsupported_schema",
    );
    fireEvent.click(screen.getByRole("button", { name: "Open conductor settings" }));
    expect(onManage).toHaveBeenCalledOnce();
  });

  it("shows expired driver access — never a raw 401 — and reconnects on demand", async () => {
    drivers.getStatus.mockRejectedValueOnce(new Error("getSessionsByIdDriverStatus failed: 401"));
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={() => {}}
      />,
    );

    await waitFor(() => expect(screen.getByText("Driver access expired")).toBeTruthy());
    expect(document.body.textContent).not.toMatch(/failed: 401/);
    expect(document.body.textContent).not.toMatch(/getSessionsByIdDriverStatus/);
    expect(screen.getByTestId("driver-lifecycle-strip").getAttribute("data-driver-recovery")).toBe(
      "lease_expired",
    );

    drivers.getStatus.mockResolvedValue(driverStatus("ready"));
    fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
    await waitFor(() => expect(screen.getByText("Ready")).toBeTruthy());
  });

  it("shows reconnecting from the real vendor session already on the wire, never an elapsed-time guess", async () => {
    drivers.getStatus.mockResolvedValue(
      driverStatus("starting", null, { vendorSessionId: "vendor-already-established" }),
    );
    const { rerender } = render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={() => {}}
      />,
    );

    await waitFor(() => expect(screen.getByText("Reconnecting")).toBeTruthy());

    drivers.getStatus.mockResolvedValue(driverStatus("starting", null, { vendorSessionId: null }));
    rerender(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-fresh-start"
        driverKind="codex"
        onManage={() => {}}
      />,
    );
    await waitFor(() => expect(screen.getByText("Starting")).toBeTruthy());
    expect(screen.queryByText("Reconnecting")).toBeNull();
  });

  it("confirms a resume only from two real consecutive polls of the same vendor session, never a timer guess", async () => {
    vi.useFakeTimers();
    drivers.getStatus.mockResolvedValue(
      driverStatus("interrupted", null, { vendorSessionId: "vendor-continuity" }),
    );
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-resume"
        driverKind="codex"
        onManage={() => {}}
      />,
    );
    await vi.waitFor(() => expect(screen.getByText("Interrupted")).toBeTruthy());

    drivers.getStatus.mockResolvedValue(
      driverStatus("running", null, { vendorSessionId: "vendor-continuity" }),
    );
    await vi.advanceTimersByTimeAsync(2_000);
    await vi.waitFor(() => expect(screen.getByText("Resumed")).toBeTruthy());
    expect(screen.getByTestId("driver-lifecycle-strip").getAttribute("data-driver-lifecycle")).toBe(
      "running",
    );

    await vi.advanceTimersByTimeAsync(2_000);
    await vi.waitFor(() => expect(screen.getByText("Active")).toBeTruthy());
  });

  it("offers a retry when status loading fails", async () => {
    drivers.getStatus.mockRejectedValueOnce(new Error("Connection lost"));
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="claude"
        onManage={() => {}}
      />,
    );

    await waitFor(() => expect(screen.getByText("Status unavailable")).toBeTruthy());
    expect(screen.getByText("Connection lost")).toBeTruthy();
    drivers.getStatus.mockResolvedValue({
      ...driverStatus("completed"),
      driverKind: "claude",
      drivers: [
        {
          ...driverStatus("completed").drivers[0],
          driverKind: "claude",
        },
      ],
    });
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByText("Completed")).toBeTruthy());
  });

  // A session that never started a driver has no binding to reconnect, so
  // "Not resumed - open it to reconnect" pointed the user at a control that
  // could not help. With no binding the honest state is "Ready to start".
  it("reads as ready-to-start, not not-resumed, when the session has no binding at all", async () => {
    drivers.getStatus.mockResolvedValue({
      sessionId: "session-1",
      driverKind: "codex",
      authRequired: false,
      lifecycle: "not_resumed",
      drivers: [],
    });
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={() => {}}
      />,
    );
    await waitFor(() => expect(screen.getByText("Ready to start")).toBeTruthy());
    expect(screen.getByTestId("driver-lifecycle-strip").getAttribute("data-driver-lifecycle")).toBe(
      "not_started",
    );
    expect(screen.queryByText("Not resumed")).toBeNull();
  });

  // The daemon's session-level "not_resumed" must never mask a binding that is
  // reporting its own real state.
  it("shows the binding's own lifecycle even when the session reports not_resumed", async () => {
    drivers.getStatus.mockResolvedValue({
      sessionId: "session-1",
      driverKind: "codex",
      authRequired: false,
      lifecycle: "not_resumed",
      drivers: driverStatus("completed").drivers,
    });
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={() => {}}
      />,
    );

    const strip = await waitFor(() => screen.getByTestId("driver-lifecycle-strip"));
    await waitFor(() => expect(strip.getAttribute("data-driver-lifecycle")).toBe("completed"));
    expect(screen.queryByText("Not resumed")).toBeNull();
    expect(screen.queryByText("Status unavailable")).toBeNull();
    expect(document.body.textContent).not.toMatch(/failed: \d{3}/);
    expect(document.body.textContent).not.toMatch(/getSessionsByIdDriverStatus/);
  });

  it("degrades to a calm message — never the raw operationId/status — when the daemon 404s the status poll", async () => {
    drivers.getStatus.mockRejectedValueOnce(new Error("getSessionsByIdDriverStatus failed: 404"));
    render(
      <DriverLifecycleStrip
        config={{ url: "http://127.0.0.1:1", token: "token" }}
        sessionId="session-1"
        driverKind="codex"
        onManage={() => {}}
      />,
    );

    await waitFor(() => expect(screen.getByText("Status unavailable")).toBeTruthy());
    expect(document.body.textContent).not.toMatch(/failed: 404/);
    expect(document.body.textContent).not.toMatch(/getSessionsByIdDriverStatus/);
    expect(screen.getByText("This session hasn't been resumed yet.")).toBeTruthy();
  });
});
