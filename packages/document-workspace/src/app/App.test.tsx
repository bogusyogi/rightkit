// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, waitFor, cleanup } from "@testing-library/react";
import { afterEach } from "vitest";
import { SettingsProvider } from "@rightkit/document-core/settings/settings";

// Approved exception (scripts/test-edge-cases.json): the startup ordering of the live OS-open
// listener versus the queued-path drain is an async race that no browser-only route outcome can
// force deterministically. Every other App shell workflow is covered by the browser E2E journeys
// in scripts/qa-app-root-e2e.mjs.
const h = vi.hoisted(() => ({
  openLifecycleOrder: [] as string[],
}));

vi.mock("@rightkit/document-core/state/tauriBridge", () => ({
  inTauri: () => false,
  installExternalLinkGuard: vi.fn(),
  openFileDialog: vi.fn(async () => null),
  saveFileDialog: vi.fn(async () => null),
  readDocument: vi.fn(async () => {
    throw new Error("no documents in the startup-order check");
  }),
  writeDocument: vi.fn(async () => "UTF-8"),
  writeDocumentBytes: vi.fn(async () => {}),
  watchFile: vi.fn(async () => {}),
  watchFiles: vi.fn(async () => {}),
  revokePath: vi.fn(async () => {}),
  fileFingerprint: vi.fn(async () => null),
  grantKnownFileToken: vi.fn(async (token: string) => token),
  openLinkedDocument: vi.fn(async (target: string) => target),
  regrantSavedTarget: vi.fn(async (path: string) => path),
  readClipboardContent: vi.fn(async () => ({ kind: "empty" })),
  getInitialOpenPaths: vi.fn(async () => {
    h.openLifecycleOrder.push("initial-path-drain");
    return [] as string[];
  }),
  onAppEvent: vi.fn(async (name: string) => {
    if (name === "open-file") h.openLifecycleOrder.push("open-file-listener");
    return () => {};
  }),
  openExternal: vi.fn(),
  nativeIpc: {
    invoke: vi.fn(async () => {
      throw new Error("no native host in the startup-order check");
    }),
    listen: vi.fn(async () => () => {}),
  },
  browserQaLicenseClient: vi.fn(async () => null),
  editImageExternal: vi.fn(async () => {}),
  printCurrentWebview: vi.fn(async () => true),
  saveSession: vi.fn(async () => {}),
  loadSession: vi.fn(async () => []),
  clearSession: vi.fn(async () => {}),
  saveSetupRecovery: vi.fn(async () => {}),
  loadSetupRecovery: vi.fn(async () => null),
  clearSetupRecovery: vi.fn(async () => {}),
  getSettingsMirror: vi.fn(async () => null),
  loadSettingsDocument: vi.fn(async () => ({ value: null, revision: 0 })),
  saveSettingsDocument: vi.fn(async () => null),
  SETTINGS_DOCUMENT_KEY: "vr.settings.v1",
  isBrowserQaMode: vi.fn(() => false),
  setSettingsMirror: vi.fn(async () => {}),
}));

vi.mock("../state/library.js", () => ({
  listRecent: vi.fn(async () => []),
  listPins: vi.fn(async () => []),
  togglePin: vi.fn(async () => true),
  togglePinDir: vi.fn(async () => true),
  recordRecent: vi.fn(async () => {}),
  saveViewState: vi.fn(async () => {}),
  isPinnedPath: vi.fn(async () => false),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    destroy: vi.fn(async () => {}),
    onCloseRequested: vi.fn(async () => () => {}),
  }),
}));

vi.mock("../documents/DocumentRouter.js", () => ({ DocumentRouter: () => null }));

import { App } from "./App.js";

afterEach(() => {
  cleanup(); // unmount between tests so window keydown listeners don't stack across App instances
});

beforeEach(() => {
  h.openLifecycleOrder.length = 0;
  localStorage.clear();
  sessionStorage.clear();
});

describe("App shell workflows", () => {
  it("registers the live OS-open listener before draining queued startup paths", async () => {
    render(
      <SettingsProvider>
        <App />
      </SettingsProvider>,
    );

    await waitFor(() => expect(h.openLifecycleOrder).toContain("initial-path-drain"));
    expect(h.openLifecycleOrder.slice(0, 2)).toEqual(["open-file-listener", "initial-path-drain"]);
  });
});
