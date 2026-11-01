import { describe, expect, it } from "vitest";

import { compareCliVersions, projectCliUpdate, projectCliUpdates } from "./updateProjection";

const provider = { providerId: "codex", displayName: "Codex CLI", installed: true, binaryVersion: "v1.2.3" };

describe("CLI update projection", () => {
  it("compares numeric versions and rejects ambiguous values", () => {
    expect(compareCliVersions("1.2.4", "1.2.3")).toBe(1);
    expect(compareCliVersions("1.2", "1.2.0")).toBe(0);
    expect(compareCliVersions("nightly", "1.2.0")).toBeNull();
  });

  it("does not invent a latest version when the daemon only reports inventory", () => {
    expect(projectCliUpdate(provider)).toMatchObject({
      state: "unavailable",
      installedVersion: "v1.2.3",
      latestVersion: null,
      canInstall: false,
    });
  });

  it("projects current, newer, and missing binaries honestly", () => {
    expect(projectCliUpdate(provider, "1.2.3").state).toBe("current");
    expect(projectCliUpdate(provider, "1.3.0").state).toBe("update-available");
    expect(projectCliUpdate({ ...provider, installed: false, binaryVersion: null }, "1.3.0").state).toBe("not-installed");
    expect(projectCliUpdates([provider], { codex: "1.3.0" })[0]?.latestVersion).toBe("1.3.0");
  });
});
