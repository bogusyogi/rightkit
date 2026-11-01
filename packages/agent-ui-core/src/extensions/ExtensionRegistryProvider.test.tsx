import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ExtensionSlot } from "./ExtensionSlot.js";
import { ExtensionRegistry } from "./extensionRegistry.js";
import { ExtensionRegistryProvider } from "./ExtensionRegistryProvider.js";

afterEach(cleanup);

describe("ExtensionRegistryProvider", () => {
  it("shares the supplied registry with descendants", () => {
    const registry = new ExtensionRegistry();
    registry.register({ name: "demo", components: { header: () => <span>header extension</span> } });
    render(
      <ExtensionRegistryProvider registry={registry}>
        <ExtensionSlot name="header" />
      </ExtensionRegistryProvider>,
    );
    expect(screen.getByText("header extension")).toBeInTheDocument();
  });

  it("pins the registry instance for the lifetime of the provider", () => {
    const first = new ExtensionRegistry();
    const second = new ExtensionRegistry();
    first.register({ name: "first", components: { slot: () => <span>first</span> } });
    second.register({ name: "second", components: { slot: () => <span>second</span> } });

    const view = render(
      <ExtensionRegistryProvider registry={first}>
        <ExtensionSlot name="slot" />
      </ExtensionRegistryProvider>,
    );
    view.rerender(
      <ExtensionRegistryProvider registry={second}>
        <ExtensionSlot name="slot" />
      </ExtensionRegistryProvider>,
    );
    expect(screen.getByText("first")).toBeInTheDocument();
    expect(screen.queryByText("second")).not.toBeInTheDocument();
  });
});
