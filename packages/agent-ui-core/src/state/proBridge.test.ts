// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  getProBridgeStatus,
  notifyProBridgeAfterActivation,
  redeemWindowsLinkCode,
  setProBridgeSyncEnabled,
  suppressProBridgeConsumption,
} from "./proBridge.js";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("Apple continuity commands", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("reads status and sets the single sync control through the named commands", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      keychainSyncEnabled: true,
      proResolutionEnabled: true,
      keychainReady: true,
      consumptionSuppressed: false,
      proResolutionStatus: "ok",
      supported: true,
    });
    await expect(getProBridgeStatus()).resolves.toMatchObject({ proResolutionStatus: "ok" });
    expect(invoke).toHaveBeenCalledWith("pro_bridge_status");

    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await setProBridgeSyncEnabled(true);
    expect(invoke).toHaveBeenLastCalledWith("pro_bridge_set_sync_enabled", { enabled: true });
  });

  it("lets a failed sync toggle reject so the panel can show the reason", async () => {
    vi.mocked(invoke).mockRejectedValueOnce("keychain unavailable");
    await expect(setProBridgeSyncEnabled(true)).rejects.toBe("keychain unavailable");
  });

  it("treats suppression and post-activation publish as best effort", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("command not found"));
    await expect(suppressProBridgeConsumption(true)).resolves.toBeUndefined();
    await expect(notifyProBridgeAfterActivation()).resolves.toBeUndefined();
    expect(invoke).toHaveBeenCalledWith("pro_bridge_suppress_consumption", { suppressed: true });
    expect(invoke).toHaveBeenCalledWith("pro_bridge_after_activation");
  });
});

describe("Windows link-code redemption", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("sends only the code and an optional label and answers the owner's view", async () => {
    const view = { status: "active", tier: "pro", features: ["routing"] };
    vi.mocked(invoke).mockResolvedValueOnce(view);
    await expect(redeemWindowsLinkCode("AB12-CD34")).resolves.toBe(view);
    expect(invoke).toHaveBeenCalledWith("license_windows_link_redeem", {
      code: "AB12-CD34",
      deviceLabel: null,
    });
    vi.mocked(invoke).mockResolvedValueOnce(view);
    await redeemWindowsLinkCode("AB12-CD34", "Office PC");
    expect(invoke).toHaveBeenLastCalledWith("license_windows_link_redeem", {
      code: "AB12-CD34",
      deviceLabel: "Office PC",
    });
  });

  it("lets the owner's command error reach the caller unchanged", async () => {
    const refused = { code: "rejected", reason: "keyNotAccepted", message: "keyNotAccepted" };
    vi.mocked(invoke).mockRejectedValueOnce(refused);
    await expect(redeemWindowsLinkCode("nope")).rejects.toBe(refused);
  });
});
