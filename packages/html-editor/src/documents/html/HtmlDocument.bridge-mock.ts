import { vi } from "vitest";

const bridge = vi.hoisted(() => ({
  readDocument: vi.fn(async () => ({ content: "<p>nav</p>" })),
  grantRelativeFile: vi.fn(async () => null),
  openExternal: vi.fn(async () => {}),
  openFileDialog: vi.fn(async () => null),
  readRelativeAsset: vi.fn(async () => null),
  readRelativeScript: vi.fn(async () => null),
  // No site-root grant under test: previews fall back to the inlining path, which is what the
  // assertions in these suites describe.
  grantSiteRoot: vi.fn(async () => null),
  chooseSiteRoot: vi.fn(async () => null),
  revokeSiteRoot: vi.fn(async () => {}),
  openDevtools: vi.fn(async () => {}),
  setDevtoolsShortcutScope: vi.fn(async () => {}),
  onContextViewSource: vi.fn(async () => () => {}),
  onContextOpenLinkTab: vi.fn(async () => () => {}),
  onContextOpenLinkExternal: vi.fn(async () => () => {}),
  getSettingsMirror: vi.fn(async () => null),
  loadSettingsDocument: vi.fn(async () => ({ value: null, revision: 0 })),
  saveSettingsDocument: vi.fn(async () => null),
  SETTINGS_DOCUMENT_KEY: "vr.settings.v1",
  isBrowserQaMode: vi.fn(() => false),
  setSettingsMirror: vi.fn(async () => {}),
}));

export { bridge };

vi.mock("@rightkit/document-core/state/tauriBridge", () => bridge);
