// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";

import SettingsModal from "./SettingsModal";
import settingsModalSource from "./SettingsModal.tsx?raw";
import settingsTypesSource from "./types.ts?raw";

describe("SettingsModal D0 contract", () => {
  it("groups every destination under the six desktop settings sections", () => {
    for (const label of [
      "General",
      "Models & Providers",
      "Capabilities",
      "Execution",
      "Access",
      "Advanced",
    ]) {
      expect(settingsTypesSource).toContain(`label: "${label}"`);
    }
    expect(settingsTypesSource).not.toMatch(/id:\s*"runtime",\s*label:\s*"Runtime"/);
    expect(settingsTypesSource).not.toMatch(/id:\s*"account",\s*label:\s*"Account"/);
    expect(settingsTypesSource).not.toContain('"import"');
  });

  it("hosts RightKit's modal shell and settings panel with validated tab deep links", () => {
    expect(settingsModalSource).toContain("@rightkit/react-shell/react");
    expect(settingsModalSource).toContain("<ModalShell");
    expect(settingsModalSource).toContain("@rightkit/settings-ui/core");
    expect(settingsModalSource).toContain("<SettingsPanel");
    expect(settingsModalSource).toContain('searchLabel="Search settings"');
    expect(settingsModalSource).toMatch(/settings=\$\{tab\}/);
    expect(settingsModalSource).toContain("window.history.replaceState");
    expect(settingsModalSource).toContain("hashchange");
  });

  it("renders honest dirty and configuration-error states", () => {
    expect(settingsModalSource).toContain('data-testid="settings-dirty-banner"');
    expect(settingsModalSource).toContain('data-testid="settings-config-error"');
    expect(settingsModalSource).toContain("Settings unavailable.");
    expect(settingsModalSource).toContain("Unsaved settings changes.");
    expect(settingsModalSource).toContain("Check the engine connection and try again.");
  });

  it("keeps controls named and described", () => {
    expect(settingsModalSource).toContain("title={closeTooltip}");
    expect(settingsModalSource).toContain("title={discardTooltip}");
    expect(settingsModalSource).toContain("title={saveTooltip}");
    expect(settingsModalSource).toContain("title={reloadLabel}");
    expect(settingsModalSource).toContain('aria-label="Close settings (Esc)"');
  });

  it("keeps the section title and actions visible", () => {
    expect(settingsModalSource).toContain('id="settings-modal-title"');
    expect(settingsModalSource).toContain("ariaLabel={`${hostAppName()} settings`}");
  });

  it("prevents concurrent save, discard, retry, close, and dismissal during reload", () => {
    expect(settingsModalSource).toContain("disabled={isReloading || !settingsDirty}");
    expect(settingsModalSource).toContain("disabled={isReloading || !canSaveSettings}");
    expect(settingsModalSource).toContain("disabled={isReloading}");
    expect(settingsModalSource).toContain('data-settings-action="try-again"');
    // Escape and backdrop are vetoed into attemptClose, which refuses while a reload is pending.
    expect(settingsModalSource).toContain("attemptClose();\n          return false;");
    expect(settingsModalSource).toContain("if (settingsConfigPending) return;");
    expect(settingsModalSource).toContain("aria-busy={isReloading}");
    expect(settingsModalSource).toContain("data-settings-pending=");
    expect(settingsModalSource).toContain("Settings reload in progress — wait for it to finish");
  });
});

describe("SettingsModal nav selection", () => {
  it("marks exactly one nav row as selected", async () => {
    const { render } = await import("@testing-library/react");
    const props = {
      open: true,
      activeTab: "plugins",
      onClose: () => {},
      onTab: () => {},
      workspaceRoot: "/tmp",
      engineReady: true,
      onReloadSettingsConfig: () => {},
      onDiscardSettings: () => {},
      onSaveSettings: () => {},
    } as unknown as Parameters<typeof SettingsModal>[0];
    render(<SettingsModal {...props} />);
    const rows = Array.from(document.body.querySelectorAll("[data-section-id]"));
    const selected = rows.filter(
      (row) => row.getAttribute("aria-current") === "page",
    );
    expect(selected).toHaveLength(1);
    expect(selected[0]?.textContent?.trim()).toBe("Plugins");
  });
});
