import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import PluginContributionHost from "@rightkit/agent-ui-settings/components/PluginContributionHost";
import type { PluginContributionProjection } from "@rightkit/agent-ui-core/lib/pluginContributionProjection";

afterEach(cleanup);

function projection(): PluginContributionProjection {
  return {
    pluginId: "demo.plugin",
    pluginName: "Demo Plugin",
    version: "1.0.0",
    description: "A demo plugin",
    commands: [{ command: "demo.run", title: "Run demo", pluginId: "demo.plugin", pluginName: "Demo Plugin" }],
    panels: [{ id: "demo.panel", name: "Demo panel", side: "left", icon: "panel", pluginId: "demo.plugin" }],
    statusBarItems: [{ id: "demo.status", text: "Ready", alignment: "right", pluginId: "demo.plugin", visible: true }],
    sessionActions: [{ id: "demo.action", panelId: "demo.panel", name: "Open", icon: "panel", pluginId: "demo.plugin" }],
    settings: {},
    permissions: ["sessions.read"],
    unsupportedPermissions: [],
  };
}

describe("PluginContributionHost", () => {
  it("renders contributions and dispatches only through supplied callbacks", () => {
    const onCommand = vi.fn();
    const onSessionAction = vi.fn();
    render(
      <PluginContributionHost
        projection={projection()}
        onCommand={onCommand}
        onSessionAction={onSessionAction}
      />,
    );

    expect(screen.getByTestId("plugin-contribution-host")).toHaveAttribute("data-plugin-id", "demo.plugin");
    expect(screen.getByRole("button", { name: "Run demo" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Run demo" }));
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(onCommand).toHaveBeenCalledWith("demo.run");
    expect(onSessionAction).toHaveBeenCalledWith("demo.action");
  });

  it("opens a contributed panel and uses its component props", () => {
    const Panel = ({ pluginId, panelId }: { pluginId: string; panelId: string }) => (
      <span>{pluginId}:{panelId}</span>
    );
    const next = projection();
    next.panels = [{ ...next.panels[0], component: Panel }];
    render(<PluginContributionHost projection={next} />);

    fireEvent.click(screen.getByRole("button", { name: /Demo panel/ }));
    expect(screen.getByText("demo.plugin:demo.panel")).toBeInTheDocument();
  });

  it("does not render hidden status items and reports unsupported permissions", () => {
    const next = projection();
    next.statusBarItems[0].visible = false;
    next.unsupportedPermissions = ["filesystem.write"];
    render(<PluginContributionHost projection={next} />);

    expect(screen.queryByText("Ready")).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("filesystem.write");
  });
});
