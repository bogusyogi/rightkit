import { describe, expect, it } from "vitest";
import { shouldShowOnboardingPanel } from "./sessionSurfaces";

describe("shouldShowOnboardingPanel", () => {
  it("stays hidden during a normal engine boot with a known workspace", () => {
    expect(
      shouldShowOnboardingPanel({
        engineReady: false,
        status: "Starting local engine...",
        workspaceRoot: "/Volumes/D/claude/coderight",
      }),
    ).toBe(false);
  });

  it("stays hidden when boot reports a session-level status, not a start word", () => {
    // Regression: the real boot status is "Ready in <workspace>", which matches
    // no "starting/connecting" word — the old allowlist flashed the chooser here.
    for (const status of ["Ready in /Volumes/D/claude/coderight", "Ready", ""]) {
      expect(
        shouldShowOnboardingPanel({
          engineReady: false,
          status,
          workspaceRoot: "/Volumes/D/claude/coderight",
        }),
      ).toBe(false);
    }
  });

  it("shows when the engine is down and no workspace is configured", () => {
    expect(
      shouldShowOnboardingPanel({
        engineReady: false,
        status: "Starting local engine...",
        workspaceRoot: "",
      }),
    ).toBe(true);
  });

  it("shows when the engine failed even with a workspace", () => {
    expect(
      shouldShowOnboardingPanel({
        engineReady: false,
        status: "Local engine unavailable",
        workspaceRoot: "/Volumes/D/claude/coderight",
      }),
    ).toBe(true);
  });

  it("never shows once the engine is ready", () => {
    expect(
      shouldShowOnboardingPanel({
        engineReady: true,
        status: "Connected",
        workspaceRoot: "",
      }),
    ).toBe(false);
  });
});
