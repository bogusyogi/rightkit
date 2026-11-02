import { describe, expect, it } from "vitest";

import {
  comparePluginVersions,
  hasPluginUpdate,
} from "./usePluginsRuntime.js";

describe("plugin lifecycle version decisions", () => {
  // Hermes IDE's plugin update tests compare numeric X.Y.Z components rather
  // than strings, so 1.10.0 correctly sorts after 1.2.0.
  it("compares numeric semver components", () => {
    expect(comparePluginVersions("1.2.0", "1.10.0")).toBe(-1);
    expect(comparePluginVersions("1.10.0", "1.2.0")).toBe(1);
    expect(comparePluginVersions("1.2", "1.2.0")).toBe(0);
  });

  it("only marks a newer catalog version as an update", () => {
    expect(hasPluginUpdate("1.0.0", "1.1.0")).toBe(true);
    expect(hasPluginUpdate("1.1.0", "1.0.0")).toBe(false);
    expect(hasPluginUpdate("1.0.0", "1.0.0")).toBe(false);
  });

  it("fails closed for malformed catalog versions", () => {
    expect(hasPluginUpdate("1.0.0", "latest")).toBe(false);
    expect(hasPluginUpdate("latest", "2.0.0")).toBe(false);
  });
});
