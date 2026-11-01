// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  entitlementSignature,
  isLicensed,
  isPro,
  licenseClient,
  licenseErrorMessage,
  viewMessage,
  type LicenseView,
} from "./license";
import { releasePlatform } from "./platform";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

function view(overrides: Partial<LicenseView> = {}): LicenseView {
  return {
    mode: "configured",
    status: "active",
    lifecycle: "active",
    tier: "pro",
    features: ["routing", "schedules"],
    kind: "term",
    stage: "final",
    validUntil: 1_900_000_000_000,
    confirmationDueAt: null,
    staleSince: null,
    licenseIdSuffix: "abcd",
    busy: false,
    unresolved: false,
    revisionState: "active",
    message: "none",
    ...overrides,
  };
}

describe("licence adapter over the native owner", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(listen).mockReset();
  });

  it("reads the licence only through the shell's status command", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(view());
    const current = await licenseClient.status();
    expect(invoke).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith("rightkit_license_status", undefined);
    expect(isLicensed(current)).toBe(true);
    expect(isPro(current)).toBe(true);
  });

  it("sends a typed key once, as the activate argument, and keeps no copy", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(view());
    await licenseClient.activate("KEY-1234");
    expect(invoke).toHaveBeenCalledWith("rightkit_license_activate", { key: "KEY-1234" });
    expect(invoke).toHaveBeenCalledOnce();
  });

  it("projects only the fixed view fields: a token that leaks into an answer is dropped", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      ...view(),
      signedToken: "leaked-token",
      activationToken: "leaked-activation",
      deviceId: "leaked-device",
    });
    const projected = await licenseClient.status();
    const serialized = JSON.stringify(projected);
    expect(serialized).not.toContain("leaked");
    expect(Object.keys(projected).sort()).toEqual(Object.keys(view()).sort());
  });

  it("deactivates and refreshes through the shell commands", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(view({ status: "none", lifecycle: "unlicensed", tier: null, features: [], revisionState: "tombstone" }));
    await licenseClient.deactivate();
    expect(invoke).toHaveBeenLastCalledWith("rightkit_license_deactivate", undefined);
    vi.mocked(invoke).mockResolvedValueOnce({ view: view(), outcome: "refreshed" });
    await licenseClient.refresh();
    expect(invoke).toHaveBeenLastCalledWith("rightkit_license_refresh", undefined);
  });

  it("listens on the owner's change event and delivers fixed views", async () => {
    let deliver: ((event: { payload: unknown }) => void) | undefined;
    vi.mocked(listen).mockImplementation(async (_event, handler) => {
      deliver = handler as (event: { payload: unknown }) => void;
      return () => undefined;
    });
    const seen: LicenseView[] = [];
    const stop = await licenseClient.onChange((next) => seen.push(next));
    expect(listen).toHaveBeenCalledWith("rightkit://license-changed", expect.any(Function));
    deliver?.({ payload: { view: view({ features: ["routing"] }) } });
    expect(seen).toHaveLength(1);
    expect(seen[0]?.features).toEqual(["routing"]);
    stop();
  });

  it("answers a refused call with the owner's message code as text, never raw detail", async () => {
    vi.mocked(invoke).mockRejectedValueOnce({
      code: "rejected",
      reason: "deviceLimit",
      message: "deviceLimit",
      cause: "stack trace with a secret",
    });
    const error = await licenseClient.activate("KEY").catch((caught: unknown) => caught);
    expect(licenseErrorMessage(error, "fallback")).toBe(
      "This licence is already active on the maximum number of devices.",
    );
    expect(licenseErrorMessage(error, "fallback")).not.toContain("secret");
  });

  it("renders a raw command rejection by its code and falls back otherwise", () => {
    expect(licenseErrorMessage({ code: "network", message: "networkUnavailable" }, "x")).toBe(
      "Could not reach the licence server. Check your connection and try again.",
    );
    // An unknown code never reaches the user verbatim: the owner's generic text stands in.
    expect(licenseErrorMessage(new Error(" boom "), "x")).toBe("Something went wrong.");
    expect(licenseErrorMessage(42, "fallback")).toBe("fallback");
  });

  it("describes a view's message and what it unlocks", () => {
    expect(viewMessage(null)).toBe("");
    expect(viewMessage(view({ message: "keyNotAccepted" }))).toBe("That licence key was not accepted.");
    expect(entitlementSignature(null)).toBe("none");
    expect(entitlementSignature(view({ features: ["b", "a"] }))).toBe(
      entitlementSignature(view({ features: ["a", "b"] })),
    );
    expect(entitlementSignature(view())).not.toBe(entitlementSignature(view({ tier: "plus" })));
  });
});

describe("licence state holds no credential of its own", () => {
  it("has no storage, token or device-id code in the renderer licence modules", async () => {
    const files = import.meta.glob(
      ["./license.ts", "./updates.ts", "./platform.ts", "../hooks/useLicenseState.ts"],
      { query: "?raw", import: "default", eager: true },
    ) as Record<string, string>;
    expect(Object.keys(files)).toHaveLength(4);
    for (const [path, source] of Object.entries(files)) {
      const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
      for (const forbidden of [
        "localStorage",
        "sessionStorage",
        "signedToken",
        "activationToken",
        "secure_update_license",
        "secure_device_id",
        "cr.updateLicense",
      ]) {
        expect(code, `${path}: ${forbidden}`).not.toContain(forbidden);
      }
    }
  });
});

describe("release platform", () => {
  it("reads the platform the shell injected, and nothing else", () => {
    expect(releasePlatform({ __RIGHTKIT_PLATFORM__: "windows" })).toBe("windows");
    expect(releasePlatform({ __RIGHTKIT_PLATFORM__: "darwin" })).toBe("darwin");
    expect(releasePlatform({ __RIGHTKIT_PLATFORM__: "linux" })).toBeNull();
    expect(releasePlatform({ __RIGHTKIT_PLATFORM__: "unknown" })).toBeNull();
    expect(releasePlatform({})).toBeNull();
  });
});
