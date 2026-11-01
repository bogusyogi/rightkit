import { describe, expect, it, vi } from "vitest";
import {
  projectActivePluginContributions,
  projectPluginContributions,
  projectPluginPermissions,
} from "./pluginContributionProjection.js";
import type { PluginManifest } from "./pluginContributionTypes.js";

function manifest(overrides: Partial<PluginManifest> = {}): PluginManifest {
  return {
    id: "demo.plugin",
    name: "Demo Plugin",
    version: "1.0.0",
    description: "A demo",
    author: "Test",
    activationEvents: [{ type: "onStartup" }],
    contributes: {
      commands: [{ command: "demo.run", title: "Run", keybinding: "Ctrl+R" }],
      panels: [{ id: "demo.panel", name: "Demo", side: "left", icon: "panel" }],
      statusBarItems: [
        { id: "demo.status", text: "Ready", alignment: "right", command: "demo.run" },
      ],
      sessionActions: [{ id: "demo.action", panelId: "demo.panel", name: "Open", icon: "panel" }],
    },
    permissions: ["network", "network", "sessions.read"],
    ...overrides,
  };
}

describe("plugin contribution projection", () => {
  it("flattens Hermes contributions and applies runtime overrides", () => {
    const panel = vi.fn(() => null);
    const projection = projectPluginContributions(manifest(), {
      panelComponents: new Map([["demo.panel", panel]]),
      statusBarOverrides: new Map([["demo.status", { text: "Busy", visible: false }]]),
      sessionActionBadges: new Map([["demo.action", { count: 2 }]]),
    });

    expect(projection.commands[0]).toMatchObject({ pluginId: "demo.plugin", pluginName: "Demo Plugin" });
    expect(projection.panels[0].component).toBe(panel);
    expect(projection.statusBarItems[0]).toMatchObject({ text: "Busy", visible: false });
    expect(projection.sessionActions[0].badge).toEqual({ count: 2 });
    expect(projection.permissions).toEqual(["network", "sessions.read"]);
  });

  it("grants the donor's implicit storage permission for settings", () => {
    const projection = projectPluginContributions(manifest({
      contributes: { settings: { theme: { type: "string", title: "Theme", default: "dark" } } },
      permissions: [],
    }));
    expect(projection.permissions).toEqual(["storage"]);
  });

  it("fails closed for unsupported permission declarations", () => {
    expect(projectPluginPermissions(["network", "filesystem.write", "filesystem.write"]))
      .toEqual({ permissions: ["network"], unsupportedPermissions: ["filesystem.write"] });
  });

  it("filters inactive manifests without changing registration order", () => {
    const second = manifest({ id: "second.plugin", name: "Second" });
    const rows = projectActivePluginContributions(
      [manifest(), second],
      new Set(["second.plugin"]),
    );
    expect(rows.map((row) => row.pluginId)).toEqual(["second.plugin"]);
  });
});
