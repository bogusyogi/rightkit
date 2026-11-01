import { describe, expect, it } from "vitest";

import { classifyStatusForToast } from "./statusToast";

describe("classifyStatusForToast", () => {
  it("classifies failure-shaped messages as error toasts", () => {
    expect(classifyStatusForToast("Delete failed")).toBe("error");
    expect(classifyStatusForToast("Local engine unavailable")).toBe("error");
    expect(classifyStatusForToast("Could not save provider key")).toBe("error");
    expect(classifyStatusForToast("Settings save failed: 500")).toBe("error");
  });

  it("classifies allowlisted success messages as success toasts", () => {
    expect(classifyStatusForToast("Session deleted")).toBe("success");
    expect(classifyStatusForToast("Settings saved.")).toBe("success");
    expect(
      classifyStatusForToast("Settings saved. Restart the daemon to apply Telegram changes."),
    ).toBe("success");
  });

  it("does not toast steady labels", () => {
    expect(classifyStatusForToast("Ready")).toBeNull();
    expect(classifyStatusForToast("Preview mode")).toBeNull();
    expect(classifyStatusForToast("  ready  ")).toBeNull();
  });

  it("does not toast routine transient statuses that aren't errors or on the allowlist", () => {
    expect(classifyStatusForToast("Conductor model set to Sonnet")).toBeNull();
    expect(classifyStatusForToast("Resuming...")).toBeNull();
    expect(classifyStatusForToast("")).toBeNull();
    expect(classifyStatusForToast("   ")).toBeNull();
  });
});
