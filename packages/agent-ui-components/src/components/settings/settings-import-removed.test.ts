// @vitest-environment node
/**
 * F0 cleanup guards. The cross-harness session import was rejected in
 * docs/research/competitive-landscape.md A1 ("Rejected; remove"); these
 * source-based assertions fail the build if a future change re-introduces the
 * Settings tab type, the navigation group entry, the preview helper, or the
 * `ImportSessionTab` component.
 */
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const typesPath = fileURLToPath(new URL("./types.ts", import.meta.url));
const sessionsPermissionsPath = fileURLToPath(
  new URL("@rightkit/agent-ui-core/lib/daemon/sessions-permissions", import.meta.url),
);
const importSessionTabPath = fileURLToPath(new URL("./tabs/ImportSessionTab.tsx", import.meta.url));
const importSessionTabTestPath = fileURLToPath(
  new URL("./tabs/ImportSessionTab.test.tsx", import.meta.url),
);

function readSource(path: string): string {
  return readFileSync(path, "utf8");
}

describe("Settings types — cross-harness import surface", () => {
  const source = readSource(typesPath);

  it('does not list "import" as a SettingsTab value', () => {
    expect(source).not.toMatch(/^\s*\|\s*"import"\s*$/m);
  });

  it("does not include the 'import' tab in SETTINGS_TABS", () => {
    // Match an array literal that contains "import" as a member — any
    // presence means a user-visible nav item is still wired.
    expect(source).not.toMatch(/SETTINGS_TABS[\s\S]*?"import"/);
  });

  it("does not list the 'import' tab in SETTINGS_NAV_ITEMS", () => {
    expect(source).not.toMatch(/SETTINGS_NAV_ITEMS[\s\S]*?tab:\s*"import"/);
  });

  it("does not list the 'import' tab in SETTINGS_NAV_GROUPS", () => {
    expect(source).not.toMatch(/SETTINGS_NAV_GROUPS[\s\S]*?"import"/);
  });
});

describe("sessions-permissions — importSessionPreview removed", () => {
  const source = readSource(sessionsPermissionsPath);

  it("does not export importSessionPreview", () => {
    expect(source).not.toMatch(/export\s+(?:async\s+)?function\s+importSessionPreview/);
  });

  it("does not reference postSessionsImport (the manifest operation for the removed route)", () => {
    expect(source).not.toMatch(/postSessionsImport/);
  });
});

describe("ImportSessionTab component and test files", () => {
  it("ImportSessionTab.tsx is deleted", () => {
    expect(existsSync(importSessionTabPath)).toBe(false);
  });

  it("ImportSessionTab.test.tsx is deleted", () => {
    expect(existsSync(importSessionTabTestPath)).toBe(false);
  });
});
