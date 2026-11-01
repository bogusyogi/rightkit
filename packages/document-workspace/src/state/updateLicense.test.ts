// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createLicenseClient } from "@rightkit/license";
// Resolves only under the `development` condition (Vitest); never imported by app code in a release.
import { createLicenseMock } from "@rightkit/license/testing";

const ipc = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: Record<string, unknown>) => Promise<unknown>>(),
  relaunch: vi.fn(async () => {}),
}));

vi.mock("@rightkit/document-core/state/tauriBridge", () => ({
  browserQaLicenseClient: vi.fn(async () => null),
  nativeIpc: {
    invoke: (command: string, args?: Record<string, unknown>) => ipc.invoke(command, args),
    listen: vi.fn(async () => () => {}),
  },
  openExternal: vi.fn(async () => {}),
}));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: ipc.relaunch }));

const checkView = (over: Record<string, unknown> = {}) => ({
  available: true,
  kind: "update",
  version: "9.9.9",
  eligible: true,
  refusal: "none",
  handleId: "h-1",
  checkedAt: 1,
  fresh: true,
  ...over,
});

const releaseStatus = (over: Record<string, unknown> = {}) => ({
  schema: 1,
  appKey: "viewright",
  platform: "windows",
  patchVersion: null,
  updateVersion: null,
  ...over,
});

beforeEach(() => {
  localStorage.clear();
  ipc.invoke.mockReset();
  ipc.relaunch.mockClear();
  vi.resetModules();
});
afterEach(() => vi.restoreAllMocks());

async function load() {
  return import("./updateLicense");
}

const noop = () => {};

describe("native update host", () => {
  it("checks without credentials and announces only an eligible update at startup", async () => {
    ipc.invoke.mockResolvedValueOnce(checkView({ kind: "patch" }));
    const { checkForBackgroundUpdate } = await load();
    expect(await checkForBackgroundUpdate()).toEqual({ version: "9.9.9", tier: "patch" });
    expect(ipc.invoke).toHaveBeenCalledTimes(1);
    expect(ipc.invoke).toHaveBeenCalledWith("rightkit_update_check", undefined);

    ipc.invoke.mockResolvedValueOnce(
      checkView({ eligible: false, refusal: "tierIneligible", handleId: undefined }),
    );
    expect(await checkForBackgroundUpdate()).toBeNull();
  });

  it("downloads then installs the checked handle, relaunches, and persists no handle", async () => {
    ipc.invoke.mockImplementation(async (command) => {
      if (command === "rightkit_update_check") return checkView();
      if (command === "rightkit_update_download")
        return { handleId: "h-1", state: "downloaded", message: "none" };
      if (command === "rightkit_update_install") return { state: "installing", message: "none" };
      throw new Error(`unexpected ${command}`);
    });
    const { checkForUpdate, installPendingUpdate } = await load();
    expect(await checkForUpdate(noop, noop, noop)).toBe("available");
    expect(await installPendingUpdate(noop)).toBe("installing");
    expect(ipc.invoke.mock.calls.map(([command, args]) => [command, args])).toEqual([
      ["rightkit_update_check", undefined],
      ["rightkit_update_download", { handleId: "h-1" }],
      ["rightkit_update_install", { handleId: "h-1" }],
    ]);
    expect(ipc.relaunch).toHaveBeenCalledTimes(1);
    expect(JSON.stringify({ ...localStorage })).not.toContain("h-1");
  });

  it("announces a paid update from the native release status when the host offers none", async () => {
    ipc.invoke.mockImplementation(async (command) => {
      if (command === "rightkit_update_check")
        return checkView({ available: false, eligible: false, handleId: undefined, kind: undefined });
      if (command === "rightkit_update_release_status") return releaseStatus({ updateVersion: "9.9.9" });
      throw new Error(`unexpected ${command}`);
    });
    const { checkForUpdate } = await load();
    const versions: string[] = [];
    expect(await checkForUpdate((version) => versions.push(version), noop, noop)).toBe(
      "license-required",
    );
    expect(versions).toEqual(["9.9.9"]);
    expect(ipc.invoke).toHaveBeenCalledWith("rightkit_update_release_status", undefined);
  });

  it("reports current when the native release status names no newer release", async () => {
    ipc.invoke.mockImplementation(async (command) => {
      if (command === "rightkit_update_check")
        return checkView({ available: false, eligible: false, handleId: undefined, kind: undefined });
      if (command === "rightkit_update_release_status") return releaseStatus();
      throw new Error(`unexpected ${command}`);
    });
    const { checkForUpdate } = await load();
    expect(await checkForUpdate(noop, noop, noop)).toBe("current");
  });

  it("falls back to a manual download from the native release status when the check fails", async () => {
    ipc.invoke.mockImplementation(async (command) => {
      if (command === "rightkit_update_check") throw { code: "network", message: "offline" };
      if (command === "rightkit_update_release_status") return releaseStatus({ patchVersion: "9.9.9" });
      throw new Error(`unexpected ${command}`);
    });
    const { checkForUpdate, hasPendingExternalUpdate } = await load();
    expect(await checkForUpdate(noop, noop, noop)).toBe("available-external");
    expect(hasPendingExternalUpdate()).toBe(true);
  });

  it("fails when both the update check and the native release status fail", async () => {
    ipc.invoke.mockRejectedValue({ code: "network", message: "offline" });
    const { checkForUpdate } = await load();
    expect(await checkForUpdate(noop, noop, noop)).toBe("failed");
  });

  it("fails closed with licence-required when the host refuses the paid update", async () => {
    ipc.invoke.mockImplementation(async (command) => {
      if (command === "rightkit_update_check") return checkView();
      if (command === "rightkit_update_download")
        return { handleId: "h-1", state: "refused", reason: "tierIneligible", message: "updateRefused" };
      throw new Error(`unexpected ${command}`);
    });
    const { checkForUpdate, installPendingUpdate } = await load();
    await checkForUpdate(noop, noop, noop);
    expect(await installPendingUpdate(noop)).toBe("license-required");
    expect(ipc.invoke).not.toHaveBeenCalledWith("rightkit_update_install", expect.anything());
    expect(ipc.relaunch).not.toHaveBeenCalled();
  });

  it("reports a fixed failure message and stages the public installer after a failed patch install", async () => {
    ipc.invoke.mockImplementation(async (command) => {
      if (command === "rightkit_update_check") return checkView({ kind: "patch" });
      if (command === "rightkit_update_download")
        return { handleId: "h-1", state: "downloaded", message: "none" };
      if (command === "rightkit_update_install") return { state: "failed", message: "updateFailed" };
      throw new Error(`unexpected ${command}`);
    });
    const { checkForUpdate, installPendingUpdate, hasPendingExternalUpdate } = await load();
    await checkForUpdate(noop, noop, noop);
    let shown = "";
    expect(await installPendingUpdate(noop, (message) => (shown = message))).toBe("install-failed");
    expect(shown).toBe("The update could not be downloaded or installed.");
    expect(hasPendingExternalUpdate()).toBe(true);
    expect(ipc.relaunch).not.toHaveBeenCalled();
  });
});

describe("licence client", () => {
  it("sends the key only as a transient activate argument and keeps no record in web storage", async () => {
    const { bridge } = createLicenseMock();
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const client = createLicenseClient({
      invoke: (command, args) => {
        calls.push([command, args]);
        return bridge.invoke(command, args);
      },
      listen: bridge.listen,
    });
    const view = await client.activate("VR-TEST-KEY");
    expect(view.status).toBe("active");
    expect(calls).toEqual([["rightkit_license_activate", { key: "VR-TEST-KEY" }]]);
    expect(JSON.stringify(view)).not.toContain("VR-TEST-KEY");
    expect(localStorage.length).toBe(0);
  });
});
