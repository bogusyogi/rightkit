import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { UPDATE_HOST_CONTRACT_VECTORS } from "@rightkit/app-update";
import {
  NativeUpdateClientError,
  UPDATE_MESSAGES,
  type NativeUpdateClient,
  type UpdateCheckView,
  type UpdateDownloadResult,
  type UpdateInstallResult,
  type UpdateProgress,
} from "@rightkit/app-update/native";

import {
  createUpdateController,
  releaseStatusUrl,
  type UpdateControllerDependencies,
  type UpdateState,
} from "./updates";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("../lib/bootProbation", () => ({
  beginBootProbation: vi.fn(async () => undefined),
  bootProbation: { acknowledgeHealthy: vi.fn(async () => undefined) },
}));

const HANDLE = "handle-1";

function checkView(overrides: Partial<UpdateCheckView> = {}): UpdateCheckView {
  return {
    available: true,
    kind: "patch",
    version: "0.2.0",
    tier: "patch",
    eligible: true,
    refusal: "none",
    handleId: HANDLE,
    checkedAt: 1,
    fresh: true,
    ...overrides,
  };
}

interface Harness {
  controller: ReturnType<typeof createUpdateController>;
  calls: string[];
  statuses: string[];
  states: UpdateState[];
  latest: string[];
  native: Pick<NativeUpdateClient, "check" | "download" | "install" | "onProgress"> & {
    download: Mock<NativeUpdateClient["download"]>;
    install: Mock<NativeUpdateClient["install"]>;
  };
  deps: UpdateControllerDependencies;
  emitProgress: (progress: UpdateProgress) => void;
}

function harness(
  options: {
    check?: UpdateCheckView | (() => Promise<UpdateCheckView>);
    download?: UpdateDownloadResult;
    install?: UpdateInstallResult;
    proVersion?: string | null;
  } = {},
): Harness {
  const calls: string[] = [];
  const statuses: string[] = [];
  let listener: ((progress: UpdateProgress) => void) | null = null;
  const native = {
    check: vi.fn(async (): Promise<UpdateCheckView> => {
      calls.push("check");
      const view = options.check ?? checkView();
      return typeof view === "function" ? view() : view;
    }),
    download: vi.fn(async (handle: string): Promise<UpdateDownloadResult> => {
      calls.push(`download:${handle}`);
      return options.download ?? { handleId: handle, state: "downloaded", message: "none" };
    }),
    install: vi.fn(async (handle: string): Promise<UpdateInstallResult> => {
      calls.push(`install:${handle}`);
      return options.install ?? { state: "installing", message: "none" };
    }),
    onProgress: vi.fn(async (callback: (progress: UpdateProgress) => void): Promise<() => void> => {
      listener = callback;
      return () => {
        calls.push("unlisten");
        listener = null;
      };
    }),
  };
  const deps: UpdateControllerDependencies = {
    native,
    proUpdateVersion: vi.fn(async () => options.proVersion ?? null),
    beginBootProbation: vi.fn(async (version: string) => {
      calls.push(`probation:${version}`);
    }),
    abandonBootProbation: vi.fn(async () => {
      calls.push("probation:abandon");
    }),
    relaunch: vi.fn(async () => {
      calls.push("relaunch");
    }),
    publish: (status) => statuses.push(status.kind),
  };
  return {
    controller: createUpdateController(deps),
    calls,
    statuses,
    states: [],
    latest: [],
    native,
    deps,
    emitProgress: (progress) => listener?.(progress),
  };
}

function check(h: Harness) {
  return h.controller.check(
    (version) => h.latest.push(version),
    (state) => h.states.push(state),
  );
}

function install(h: Harness) {
  return h.controller.install((state) => h.states.push(state));
}

describe("native update controller", () => {
  beforeEach(() => vi.clearAllMocks());

  // Entitlement is decided by the native host (licence authority); the renderer's job is to show
  // what it said and to install exactly what it made installable.
  for (const vector of UPDATE_HOST_CONTRACT_VECTORS) {
    it(`installs when the host says eligible and reports not entitled otherwise: ${vector.name}`, async () => {
      const kind = vector.tier === "patch" ? "patch" : "update";
      const h = harness({
        check: vector.eligible
          ? checkView({ kind, tier: vector.tier })
          : checkView({
              kind,
              tier: vector.tier,
              eligible: false,
              refusal: "notLicensed",
              handleId: undefined,
            }),
      });

      await check(h);
      expect(h.statuses[1]).toBe("available");
      expect(h.states.at(-1)).toBe(
        vector.eligible ? (vector.tier === "patch" ? "patch-available" : "update-available") : "pro-update-available",
      );

      await install(h);
      if (vector.eligible) {
        expect(h.calls).toContain(`install:${HANDLE}`);
        expect(h.calls.at(-1)).toBe("relaunch");
        expect(h.statuses.at(-1)).toBe("ready");
      } else {
        expect(h.native.download).not.toHaveBeenCalled();
        expect(h.native.install).not.toHaveBeenCalled();
        expect(h.controller.hasPending()).toBe(false);
      }
    });
  }

  it("states the not-entitled reason of an unentitled feature update", async () => {
    const h = harness({
      check: checkView({ kind: "update", tier: "update", eligible: false, refusal: "notLicensed", handleId: undefined }),
    });
    const published: unknown[] = [];
    h.deps.publish = (status) => published.push(status);
    const controller = createUpdateController(h.deps);
    await controller.check(() => undefined, () => undefined);
    expect(published.at(-1)).toEqual({
      kind: "available",
      version: "0.2.0",
      tier: "update",
      eligible: false,
      reason: "pro_required",
    });
  });

  it("offers an unentitled client the public release status of a feature update, without installing it", async () => {
    const h = harness({ check: checkView({ available: false, kind: undefined, version: undefined, handleId: undefined, eligible: false }), proVersion: "0.3.0" });
    await check(h);
    expect(h.latest).toEqual(["0.3.0"]);
    expect(h.states.at(-1)).toBe("pro-update-available");
    await install(h);
    expect(h.native.install).not.toHaveBeenCalled();
  });

  it("reports up to date when the host has nothing and no feature update is published", async () => {
    const h = harness({ check: checkView({ available: false, kind: undefined, version: undefined, handleId: undefined, eligible: false }) });
    await check(h);
    expect(h.states.at(-1)).toBe("current");
  });

  it("passes only the opaque handle to download and install, and never anything else", async () => {
    const h = harness();
    await check(h);
    await install(h);
    expect(h.native.download.mock.calls).toEqual([[HANDLE]]);
    expect(h.native.install.mock.calls).toEqual([[HANDLE]]);
    expect(vi.mocked(h.native.check).mock.calls).toEqual([[]]);
  });

  it("records boot probation before the installer starts, then relaunches", async () => {
    const h = harness();
    await check(h);
    await install(h);
    const order = h.calls.filter((call) => !call.startsWith("unlisten"));
    expect(order).toEqual([`check`, `download:${HANDLE}`, "probation:0.2.0", `install:${HANDLE}`, "relaunch"]);
    expect(h.deps.abandonBootProbation).not.toHaveBeenCalled();
  });

  it("clears the probation record when the host refuses the install", async () => {
    const h = harness({ install: { state: "refused", reason: "revoked", message: "revoked" } });
    await check(h);
    await install(h);
    expect(h.calls).toContain("probation:abandon");
    expect(h.calls).not.toContain("relaunch");
    expect(h.states.at(-1)).toBe("failed");
    expect(h.statuses.at(-1)).toBe("error");
    expect(h.controller.hasPending()).toBe(false);
  });

  it("does not install or start probation when the download fails", async () => {
    const h = harness({ download: { handleId: HANDLE, state: "failed", message: "verifyFailed" } });
    await check(h);
    await install(h);
    expect(h.native.install).not.toHaveBeenCalled();
    expect(h.deps.beginBootProbation).not.toHaveBeenCalled();
    expect(h.states.at(-1)).toBe("failed");
  });

  it("shows the fixed text of a native failure code, never native text", async () => {
    const h = harness({ download: { handleId: HANDLE, state: "failed", message: "verifyFailed" } });
    const published: Array<{ kind: string; message?: string }> = [];
    h.deps.publish = (status) => published.push(status);
    const controller = createUpdateController(h.deps);
    await controller.check(() => undefined, () => undefined);
    await controller.install(() => undefined);
    expect(published.at(-1)).toEqual({ kind: "error", message: UPDATE_MESSAGES.verifyFailed });
  });

  it("a rejected native command fails the check with the fixed message", async () => {
    const h = harness({
      check: async () => {
        throw new NativeUpdateClientError("network", "networkUnavailable");
      },
    });
    await check(h);
    expect(h.states.at(-1)).toBe("failed");
    expect(h.statuses.at(-1)).toBe("error");
  });

  it("downloads without installing, then installs the staged handle without downloading again", async () => {
    const h = harness();
    await check(h);
    await h.controller.download();
    expect(h.statuses.at(-1)).toBe("staged");
    expect(h.native.install).not.toHaveBeenCalled();
    await install(h);
    expect(h.native.download).toHaveBeenCalledTimes(1);
    expect(h.native.install).toHaveBeenCalledTimes(1);
  });

  it("publishes download progress for its own handle only", async () => {
    const h = harness();
    const published: Array<{ kind: string; pct?: number }> = [];
    h.deps.publish = (status) => published.push(status);
    h.native.download.mockImplementationOnce(async (handle: string): Promise<UpdateDownloadResult> => {
      h.emitProgress({ handleId: "other", received: 1, total: 2 });
      h.emitProgress({ handleId: handle, received: 50, total: 200 });
      return { handleId: handle, state: "downloaded", message: "none" };
    });
    const controller = createUpdateController(h.deps);
    await controller.check(() => undefined, () => undefined);
    await controller.download();
    expect(published.filter((s) => s.kind === "downloading").map((s) => s.pct)).toEqual([0, 25]);
  });

  it("a new check supersedes the pending handle", async () => {
    const views = [
      checkView(),
      checkView({ available: false, kind: undefined, version: undefined, handleId: undefined, eligible: false }),
    ];
    const h = harness({ check: async () => views.shift() as UpdateCheckView });
    await check(h);
    expect(h.controller.hasPending()).toBe(true);
    await check(h);
    expect(h.controller.hasPending()).toBe(false);
    await install(h);
    expect(h.native.install).not.toHaveBeenCalled();
  });

  it("scopes public release status to the native target", () => {
    expect(releaseStatusUrl("windows")).toContain("?platform=windows");
    expect(releaseStatusUrl("darwin")).toContain("?platform=darwin");
  });
});

describe("no renderer licence or updater path", () => {
  it("holds no token, header or updater-plugin call of its own", async () => {
    const { readFileSync } = await import("node:fs");
    const { resolve } = await import("node:path");
    const source = readFileSync(resolve(process.cwd(), "src", "state", "updates.ts"), "utf8");
    for (const forbidden of [
      "Authorization",
      "Bearer",
      "getHeaders",
      "getInstallHeaders",
      "plugin-updater",
      "createAutoUpdateClient",
      "signedToken",
      "updateTierInstallable",
    ]) {
      expect(source, forbidden).not.toContain(forbidden);
    }
    expect(source).toContain("@rightkit/app-update/native");
  });
});
