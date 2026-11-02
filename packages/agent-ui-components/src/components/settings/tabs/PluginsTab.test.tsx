// @vitest-environment happy-dom
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import PluginsTab from "./PluginsTab.js";
import { hasPluginUpdate } from "../../../hooks/usePluginsRuntime.js";
import type { PluginManifest, PluginMarketplaceEntry } from "@rightkit/agent-ui-core/lib/types";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "PluginsTab.tsx"),
  "utf8",
);

// FM-156: the mechanics (sync, install, signatures, checksums) all existed, but
// the catalog rendered every row as a bare "Install" — a user could not tell
// what was already installed, whether an update existed, or whether an entry was
// signed. Those are the decisions someone actually makes before installing.
describe("marketplace discovery", () => {
  it("uses the donor semver rule so an older catalog entry is not an update", () => {
    expect(hasPluginUpdate("1.2.0", "1.10.0")).toBe(true);
    expect(hasPluginUpdate("1.10.0", "1.2.0")).toBe(false);
    expect(hasPluginUpdate("1.2.0", "1.2.0")).toBe(false);
    expect(hasPluginUpdate("not-semver", "2.0.0")).toBe(false);
  });

  it("cross-references installed plugins instead of always offering Install", () => {
    expect(source).toMatch(/plugins\.find\(\(p\) => p\.id === entry\.id\)/);
    expect(source).toContain("updateAvailable");
    expect(source).toContain("actionLabel");
  });

  it("labels the action by what it will actually do", () => {
    for (const label of ['"Update"', '"Reinstall"', '"Install"', '"Updating"']) {
      expect(source).toContain(label);
    }
  });

  it("shows a readable trust signal, not just a hash", () => {
    expect(source).toMatch(/entry\.signedBy \? "signed" : "unsigned"/);
    expect(source).toContain("integrity is checksum-only");
  });

  it("does not present a downgrade as an available update", () => {
    render(
      <PluginsTab
        {...baseProps()}
        plugins={[makePlugin({ version: "2.0.0" })]}
        marketplaceEntries={[makeMarketplaceEntry({ version: "1.0.0" })]}
      />,
    );

    expect(screen.getByText("1 installed · 1 enabled")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Reinstall" })).toBeTruthy();
  });
});

function makePlugin(overrides: Partial<PluginManifest> = {}): PluginManifest {
  return {
    id: "sample",
    name: "Sample plugin",
    version: "1.0.0",
    description: "",
    entry: "index.js",
    providesTools: [],
    providesCommands: [],
    enabled: true,
    ...overrides,
  };
}

function makeMarketplaceEntry(
  overrides: Partial<PluginMarketplaceEntry> = {},
): PluginMarketplaceEntry {
  return {
    ...makePlugin(),
    category: "general",
    tags: [],
    downloads: 0,
    rating: 0,
    sourceUrl: "",
    sourcePath: "",
    checksumSha256: "abc",
    signedBy: "",
    ...overrides,
  };
}

function baseProps() {
  return {
    pluginDir: "/plugins",
    plugins: [] as PluginManifest[],
    pendingId: null,
    installDraft: { sourcePath: "", trustAccepted: false, overwrite: false },
    marketplacePath: "",
    marketplaceEntries: [] as PluginMarketplaceEntry[],
    marketplaceQuery: "",
    marketplaceSyncDraft: {
      catalogUrl: "",
      expectedSha256: "",
      trustAccepted: false,
      requireSigned: false,
      allowFileUrls: false,
    },
    marketplaceInstallDraft: {
      expectedSha256: "",
      trustAccepted: false,
      overwrite: false,
      allowFileUrls: false,
    },
    marketplaceError: null,
    onRefresh: vi.fn(),
    onToggle: vi.fn(),
    onInstallDraft: vi.fn(),
    onPickSource: vi.fn(),
    onInstall: vi.fn(),
    onMarketplaceQuery: vi.fn(),
    onRefreshMarketplace: vi.fn(),
    onMarketplaceSyncDraft: vi.fn(),
    onSyncMarketplace: vi.fn(),
    onMarketplaceInstallDraft: vi.fn(),
    onInstallMarketplace: vi.fn(),
  };
}

describe("PluginsTab state summary + progressive disclosure (PA-03)", () => {
  it("renders a truthful one-line state summary derived from real plugin/marketplace state", () => {
    const plugins = [
      makePlugin({ id: "a", version: "1.0.0", enabled: true }),
      makePlugin({ id: "b", version: "1.0.0", enabled: false }),
      makePlugin({ id: "c", version: "1.0.0", enabled: true }),
    ];
    const marketplaceEntries = [makeMarketplaceEntry({ id: "a", version: "2.0.0" })];
    render(
      <PluginsTab
        {...baseProps()}
        plugins={plugins}
        marketplaceEntries={marketplaceEntries}
        marketplaceSyncDraft={{
          catalogUrl: "",
          expectedSha256: "",
          trustAccepted: false,
          requireSigned: true,
          allowFileUrls: false,
        }}
      />,
    );

    expect(
      screen.getByText("3 installed · 2 enabled · 1 update available · signing required"),
    ).toBeTruthy();
  });

  it("says truthfully when nothing is installed", () => {
    render(<PluginsTab {...baseProps()} />);
    expect(screen.getByText("No plugins installed")).toBeTruthy();
  });

  it("marks Install as the one emphasized primary action and downgrades Sync", () => {
    render(<PluginsTab {...baseProps()} />);
    const install = screen.getByRole("button", { name: /Install/ });
    expect(install.className).toContain("bg-accent");

    fireEvent.click(screen.getByRole("button", { name: "Advanced: marketplace catalog sync" }));
    const sync = screen.getByRole("button", { name: /Sync/ });
    expect(sync.className).not.toContain("bg-accent");
  });

  it("collapses the marketplace catalog sync controls by default and expands on demand", () => {
    render(<PluginsTab {...baseProps()} />);
    const toggle = screen.getByRole("button", { name: "Advanced: marketplace catalog sync" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByPlaceholderText("https://...")).toBeNull();

    fireEvent.click(toggle);

    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByPlaceholderText("https://...")).toBeTruthy();
  });
});
