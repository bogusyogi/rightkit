// @vitest-environment jsdom
// Pins the built-in SettingsPanel markup so token and prop additions cannot change default output.
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SettingsPanel } from "./SettingsPanel.js";
import { createMemorySettings } from "./backends.js";
import type { SettingsSection } from "./types.js";

afterEach(() => cleanup());

const sections: SettingsSection[] = [
  { id: "alpha", label: "Alpha", group: "General", order: 100, entries: [{ id: "a1", label: "Alpha toggle" }], render: () => <p>Alpha body</p> },
  { id: "beta", label: "Beta", heading: "Beta heading", group: "General", order: 200, render: () => <p>Beta body</p> },
];

describe("SettingsPanel default markup", () => {
  it("loading state, then the first section with its heading", async () => {
    const v = render(<SettingsPanel backend={createMemorySettings({ theme: "dark" })} sections={sections} />);
    expect(v.container.innerHTML).toMatchSnapshot("loading");
    await screen.findByText("Alpha body");
    expect(v.container.innerHTML).toMatchSnapshot("loaded");
  });
  it("searching reveals entry rows", async () => {
    const v = render(<SettingsPanel backend={createMemorySettings()} sections={sections} />);
    await screen.findByText("Alpha body");
    const input = screen.getByLabelText("Search settings");
    const { fireEvent } = await import("@testing-library/react");
    fireEvent.change(input, { target: { value: "toggle" } });
    expect(v.container.innerHTML).toMatchSnapshot("search");
  });
});
