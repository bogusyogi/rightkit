import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ExtensionSlot } from "./ExtensionSlot.js";
import { ExtensionRegistry } from "./extensionRegistry.js";
import { ExtensionRegistryProvider } from "./ExtensionRegistryProvider.js";

afterEach(cleanup);

describe("ExtensionSlot", () => {
  it("renders the resolved component with the semantic slot props", () => {
    const registry = new ExtensionRegistry();
    registry.register({
      name: "demo",
      components: {
        composer: ({ extensionName, slot, data }) => (
          <span>{extensionName}:{slot}:{String(data?.mode)}</span>
        ),
      },
    });

    render(
      <ExtensionRegistryProvider registry={registry}>
        <ExtensionSlot name="composer" data={{ mode: "compact" }} />
      </ExtensionRegistryProvider>,
    );
    expect(screen.getByText("demo:composer:compact")).toBeInTheDocument();
  });

  it("uses a fallback for an unregistered slot", () => {
    const registry = new ExtensionRegistry();
    render(
      <ExtensionRegistryProvider registry={registry}>
        <ExtensionSlot name="sidebar" fallback={<span>default sidebar</span>} />
      </ExtensionRegistryProvider>,
    );
    expect(screen.getByText("default sidebar")).toBeInTheDocument();
  });

  it("updates when a component is registered after mount", () => {
    const registry = new ExtensionRegistry();
    render(
      <ExtensionRegistryProvider registry={registry}>
        <ExtensionSlot name="status" fallback={<span>empty</span>} />
      </ExtensionRegistryProvider>,
    );
    expect(screen.getByText("empty")).toBeInTheDocument();

    act(() => {
      registry.register({ name: "live", components: { status: () => <span>live status</span> } });
    });
    expect(screen.getByText("live status")).toBeInTheDocument();
  });

  it("can project a declarative template when no component exists", () => {
    const registry = new ExtensionRegistry();
    registry.setTemplates({ status: { label: "template status" } });
    render(
      <ExtensionRegistryProvider registry={registry}>
        <ExtensionSlot
          name="status"
          renderTemplate={(template) => <span>{(template as { label: string }).label}</span>}
        />
      </ExtensionRegistryProvider>,
    );
    expect(screen.getByText("template status")).toBeInTheDocument();
  });
});
