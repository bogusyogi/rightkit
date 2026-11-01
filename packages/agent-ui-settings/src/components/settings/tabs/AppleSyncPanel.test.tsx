// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ProBridgeStatus } from "@rightkit/agent-ui-core/state/proBridge";
import AppleSyncPanel, { summarizeAppleSync } from "./AppleSyncPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

function status(overrides: Partial<ProBridgeStatus> = {}): ProBridgeStatus {
  return {
    keychainSyncEnabled: false,
    proResolutionEnabled: false,
    keychainReady: true,
    keychainDetail: null,
    consumptionSuppressed: false,
    proResolutionStatus: "off",
    supported: true,
    ...overrides,
  };
}

const TOGGLE = "Sync Pro across Apple devices";

describe("summarizeAppleSync", () => {
  it("reports Off, On, and the worst failing channel", () => {
    expect(summarizeAppleSync(status())).toEqual({ text: "Off", tone: "muted" });
    expect(
      summarizeAppleSync(
        status({ keychainSyncEnabled: true, proResolutionEnabled: true, proResolutionStatus: "ok" }),
      ),
    ).toEqual({ text: "On", tone: "ok" });
    expect(
      summarizeAppleSync(
        status({
          keychainSyncEnabled: true,
          proResolutionEnabled: true,
          keychainReady: false,
          keychainDetail: "Missing entitlement",
          proResolutionStatus: "ok",
        }),
      ),
    ).toEqual({ text: "Missing entitlement", tone: "warn" });
    expect(
      summarizeAppleSync(
        status({ proResolutionEnabled: true, proResolutionStatus: "blocked_no_keychain" }),
      ),
    ).toEqual({ text: "Waiting on iCloud Keychain", tone: "warn" });
    expect(summarizeAppleSync(status({ keychainSyncEnabled: true })).tone).toBe("warn");
  });
});

describe("AppleSyncPanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });
  afterEach(() => cleanup());

  it("renders nothing where iCloud Keychain is unsupported or status is unavailable", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(status({ supported: false }));
    const { container } = render(<AppleSyncPanel />);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("pro_bridge_status"));
    expect(container.querySelector("[data-testid='apple-sync-panel']")).toBeNull();

    cleanup();
    vi.mocked(invoke).mockRejectedValueOnce("no command");
    const second = render(<AppleSyncPanel />);
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(second.container.querySelector("[data-testid='apple-sync-panel']")).toBeNull();
  });

  it("shows consent off by default and turns sync on through the single command", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce(status())
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(
        status({ keychainSyncEnabled: true, proResolutionEnabled: true, proResolutionStatus: "ok" }),
      );
    render(<AppleSyncPanel />);
    const toggle = (await screen.findByRole("checkbox", { name: TOGGLE })) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    expect(screen.getByText("Off")).toBeTruthy();

    fireEvent.click(toggle);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("pro_bridge_set_sync_enabled", { enabled: true }),
    );
    await waitFor(() => expect(screen.getByText("On")).toBeTruthy());
    expect((screen.getByRole("checkbox", { name: TOGGLE }) as HTMLInputElement).checked).toBe(true);
  });

  it("surfaces the reason when the toggle fails and keeps the real state", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce(status())
      .mockRejectedValueOnce("HostAppleCredentials helper not bundled")
      .mockResolvedValueOnce(status());
    render(<AppleSyncPanel />);
    fireEvent.click(await screen.findByRole("checkbox", { name: TOGGLE }));
    expect(await screen.findByText("HostAppleCredentials helper not bundled")).toBeTruthy();
    expect((screen.getByRole("checkbox", { name: TOGGLE }) as HTMLInputElement).checked).toBe(false);
  });

  it("explains a standing suppression after a license was removed here", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(status({ consumptionSuppressed: true }));
    render(<AppleSyncPanel />);
    expect(await screen.findByText(/paused on this device/)).toBeTruthy();
  });
});
