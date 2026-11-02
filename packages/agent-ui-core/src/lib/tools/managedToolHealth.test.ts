import { describe, expect, it } from "vitest";
import {
  managedToolErrorMessage,
  managedToolStatusLabel,
  projectManagedToolHealth,
} from "./managedToolHealth.js";

describe("managedToolHealth", () => {
  it("preserves the real installed version probe", () => {
    const health = projectManagedToolHealth(
      "claude-code",
      { installed: true, version: "1.2.3", output: "Claude Code 1.2.3", executable: "/bin/claude" },
      "2026-08-29T00:00:00.000Z",
    );
    expect(health).toEqual({
      toolId: "claude-code",
      installed: true,
      version: "1.2.3",
      output: "Claude Code 1.2.3",
      executable: "/bin/claude",
      checkedAt: "2026-08-29T00:00:00.000Z",
    });
    expect(managedToolStatusLabel(health)).toBe("Installed");
  });

  it("keeps a missing-install result actionable without inventing a version", () => {
    const health = projectManagedToolHealth("claude-code", {
      installed: false,
      output: "No such file or directory",
    });
    expect(health.version).toBeNull();
    expect(managedToolStatusLabel(health)).toBe("Not installed");
    expect(health.output).toContain("No such file");
  });

  it("normalizes unknown errors for the settings surface", () => {
    expect(managedToolErrorMessage(new Error("probe failed"))).toBe("probe failed");
    expect(managedToolErrorMessage(null)).toBe("Could not check the managed tool");
  });
});
