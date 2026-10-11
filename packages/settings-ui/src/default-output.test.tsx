// @vitest-environment jsdom
// Pins the 0.3.4 default render of the panel, the primitives and search. Written before the 0.4.0 search work; the snapshots
// in __snapshots__/default-output.test.tsx.snap must never change when an option is added.
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SettingsPanel } from "./SettingsPanel.js";
import { SettingRow, SettingsGroup } from "./primitives.js";
import { createMemorySettings } from "./backends.js";
import type { SettingsSection } from "./types.js";

afterEach(() => cleanup());
// jsdom lacks CSS.escape and scrollIntoView, which revealSetting uses.
(globalThis as { CSS?: unknown }).CSS ??= { escape: (v: string) => v };
Element.prototype.scrollIntoView ??= () => {};

const sections: SettingsSection[] = [
  {
    id: "general", label: "General", group: "App", order: 100, icon: <i>g</i>, badge: "NEW", keywords: ["basics"],
    entries: [{ id: "launch", label: "Launch at login", keywords: ["startup"] }, { id: "dock", label: "Show in dock" }],
    render: () => (
      <>
        <SettingsGroup title="Startup" description="How the app starts.">
          <SettingRow id="launch" label="Launch at login" description="Start with the computer."><input type="checkbox" aria-label="Launch" /></SettingRow>
          <SettingRow id="dock" label="Show in dock" stacked><input aria-label="Dock" /></SettingRow>
          <SettingRow label="Plain row" />
        </SettingsGroup>
        <SettingsGroup><SettingRow label="Untitled group row">x</SettingRow></SettingsGroup>
      </>
    ),
  },
  { id: "privacy", label: "Privacy", group: "App", order: 200, entries: [{ id: "telemetry", label: "Share telemetry" }], render: () => <SettingsGroup title="Data"><SettingRow id="telemetry" label="Share telemetry">t</SettingRow></SettingsGroup> },
  { id: "about", label: "About", group: "Info", order: 300, heading: "About this app", render: () => <p>About body</p> },
];

const panel = (props: Partial<React.ComponentProps<typeof SettingsPanel>> = {}) =>
  render(<SettingsPanel backend={createMemorySettings({ a: 1 })} sections={sections} {...props} />);
const loaded = async (v: ReturnType<typeof panel>) => {
  await screen.findByRole("article", {}, { timeout: 2000 }).catch(() => undefined);
  await new Promise((r) => setTimeout(r, 0));
  return v.container.innerHTML;
};
const type = (value: string) => fireEvent.change(screen.getByLabelText("Search settings"), { target: { value } });

describe("0.3.4 default output", () => {
  it("primitives standalone", () => {
    const v = render(
      <SettingsGroup title="T" description="D">
        <SettingRow id="x" label="L" description="Desc" stacked><button type="button">Go</button></SettingRow>
        <SettingRow label={<b>Node</b>} />
      </SettingsGroup>,
    );
    expect(v.container.innerHTML).toMatchSnapshot();
  });
  it("full panel with icons, badge, footer, groups and rows", async () => {
    const v = panel({ navFooter: <span>v1</span> });
    expect(await loaded(v)).toMatchSnapshot();
  });
  it("custom nav heading, renderNavItem, search off", async () => {
    const v = panel({ navHeading: "Prefs", search: false, renderNavItem: (s, st) => <em>{s.label}{st.active ? "*" : ""}</em> });
    expect(await loaded(v)).toMatchSnapshot();
  });
  it("hideNav (no search)", async () => {
    const v = panel({ hideNav: true, activeId: "privacy" });
    expect(await loaded(v)).toMatchSnapshot();
  });
  it("hideNav with search left at its default stays without a search box", async () => {
    const v = panel({ hideNav: true });
    await loaded(v);
    expect(screen.queryByLabelText("Search settings")).toBeNull();
  });
  it("query matching an entry shows hit buttons and leaves the rows unfiltered", async () => {
    const v = panel();
    await loaded(v);
    type("telemetry");
    expect(v.container.innerHTML).toMatchSnapshot();
  });
  it("query matching a section label and a keyword", async () => {
    const v = panel();
    await loaded(v);
    type("basics");
    expect(v.container.innerHTML).toMatchSnapshot("keyword");
    type("info");
    expect(v.container.innerHTML).toMatchSnapshot("group");
  });
  it("query matching nothing", async () => {
    const v = panel();
    await loaded(v);
    type("zzz");
    expect(v.container.innerHTML).toMatchSnapshot();
  });
  it("Escape, Cmd+F and Ctrl+F do nothing by default", async () => {
    const v = panel();
    await loaded(v);
    type("dock");
    const before = v.container.innerHTML;
    fireEvent.keyDown(document.body, { key: "Escape" });
    fireEvent.keyDown(screen.getByLabelText("Search settings"), { key: "Escape" });
    fireEvent.keyDown(document.body, { key: "f", metaKey: true });
    fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
    expect((screen.getByLabelText("Search settings") as HTMLInputElement).value).toBe("dock");
    expect(document.activeElement).toBe(document.body);
    expect(v.container.innerHTML).toBe(before);
  });
  it("clicking a hit selects its section", async () => {
    const v = panel();
    await loaded(v);
    type("telemetry");
    fireEvent.click(screen.getByText("Share telemetry", { selector: "button.rk-settings__hit" }));
    expect(v.container.querySelector("article")?.getAttribute("aria-labelledby")).toBe("rk-sec-privacy");
  });
});
