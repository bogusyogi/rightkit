import { describe, expect, it } from "vitest";
import {
  PLUGIN_PERMISSIONS,
  isPluginPermission,
  type PluginManifest,
} from "./pluginContributionTypes";

describe("plugin contribution types", () => {
  it("keeps the Hermes permission vocabulary closed", () => {
    expect(PLUGIN_PERMISSIONS).toEqual([
      "clipboard.read",
      "clipboard.write",
      "storage",
      "terminal.read",
      "terminal.write",
      "sessions.read",
      "notifications",
      "network",
      "shell.exec",
    ]);
    expect(isPluginPermission("network")).toBe(true);
    expect(isPluginPermission("filesystem.write")).toBe(false);
  });

  it("represents panels, commands, status items and permissions together", () => {
    const manifest: PluginManifest = {
      id: "example.plugin",
      name: "Example",
      version: "1.0.0",
      description: "Example contributions",
      author: "App",
      activationEvents: [{ type: "onStartup" }],
      contributes: {
        panels: [{ id: "panel", name: "Panel", side: "left", icon: "panel" }],
        commands: [{ command: "example.run", title: "Run" }],
        statusBarItems: [{ id: "status", text: "Ready", alignment: "right" }],
      },
      permissions: ["sessions.read"],
    };
    expect(manifest.contributes.panels?.[0].side).toBe("left");
    expect(manifest.contributes.commands?.[0].command).toBe("example.run");
    expect(manifest.permissions).toEqual(["sessions.read"]);
  });
});
