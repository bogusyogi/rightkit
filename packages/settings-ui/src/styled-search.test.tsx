// @vitest-environment jsdom
// The 0.4.2 additive props on the styled parts: className, headingLevel, hideWhenEmpty="unmount", passthroughWhenInactive.
// Defaults are pinned by the snapshot tests (default-output, output, default-markup), which are not edited by this change.
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SettingRow, SettingsGroup } from "./primitives.js";
import { SettingsSearchProvider, SettingsSearchSection } from "./search.js";

afterEach(() => cleanup());

describe("SettingsGroup additive props", () => {
  it("className is added after rk-sg; with none the class is exactly rk-sg", () => {
    const a = render(<SettingsGroup title="A" className="my-group"><SettingRow label="x" /></SettingsGroup>);
    expect(a.container.querySelector("section")?.className).toBe("rk-sg my-group");
    a.unmount();
    const b = render(<SettingsGroup title="A"><SettingRow label="x" /></SettingsGroup>);
    expect(b.container.querySelector("section")?.className).toBe("rk-sg");
  });

  it("headingLevel changes the title element; the default is still h3 with rk-sg__title", () => {
    const a = render(<SettingsGroup title="Alpha" headingLevel={2}><SettingRow label="x" /></SettingsGroup>);
    expect(a.container.querySelector("h2.rk-sg__title")?.textContent).toBe("Alpha");
    expect(a.container.querySelector("h3")).toBeNull();
    a.unmount();
    const b = render(<SettingsGroup title="Alpha"><SettingRow label="x" /></SettingsGroup>);
    expect(b.container.querySelector("h3.rk-sg__title")?.textContent).toBe("Alpha");
  });
});

describe("SettingRow className", () => {
  it("is appended after the existing classes, with and without stacked", () => {
    const v = render(
      <>
        <SettingRow label="a" className="my-row" />
        <SettingRow label="b" stacked className="my-row" />
        <SettingRow label="c" />
      </>,
    );
    expect([...v.container.querySelectorAll(".rk-sr")].map((e) => e.className)).toEqual(["rk-sr my-row", "rk-sr rk-sr--stacked my-row", "rk-sr"]);
  });
});

describe("SettingsGroup hideWhenEmpty=\"unmount\"", () => {
  function Host({ q, mode }: { q: string; mode: boolean | "unmount" }) {
    return (
      <SettingsSearchProvider query={q}>
        <SettingsGroup title="Alpha" hideWhenEmpty={mode}><SettingRow label="alpha row" /></SettingsGroup>
        <SettingsGroup title="Beta" hideWhenEmpty={mode}><SettingRow label="beta row" /></SettingsGroup>
      </SettingsSearchProvider>
    );
  }

  it("an empty group leaves no section and no heading in the DOM, and reappears when a row matches", () => {
    const v = render(<Host q="zzz" mode="unmount" />);
    expect(v.container.querySelectorAll("section")).toHaveLength(0);
    expect(v.container.querySelector("h3")).toBeNull();
    expect(screen.queryByText("alpha row")).toBeNull();
    v.rerender(<Host q="alpha" mode="unmount" />);
    expect(v.container.querySelector("h3")?.textContent).toBe("Alpha");
    expect(v.container.querySelector("section")?.getAttribute("style")).toBeNull();
    expect(v.container.querySelectorAll("section")).toHaveLength(1);
    expect(screen.getByText("alpha row")).toBeTruthy();
  });

  it("the group's rows stay registered while it is unmounted, so a later match reveals it", () => {
    // Starts empty (no row has registered yet on the first render), then a matching query arrives.
    const v = render(<Host q="beta" mode="unmount" />);
    expect(v.container.querySelector("h3")?.textContent).toBe("Beta");
    v.rerender(<Host q="gamma" mode="unmount" />);
    expect(v.container.querySelector("h3")).toBeNull();
    v.rerender(<Host q="beta" mode="unmount" />);
    expect(v.container.querySelector("h3")?.textContent).toBe("Beta");
  });

  it("true keeps the 0.4.0 behaviour: the heading stays in the DOM and the section is display none", () => {
    const v = render(<Host q="zzz" mode />);
    expect(v.container.querySelector("h3")?.textContent).toBe("Alpha");
    expect(v.container.querySelector("section")?.style.display).toBe("none");
  });

  it("without a provider nothing is unmounted", () => {
    const v = render(<SettingsGroup title="Solo" hideWhenEmpty="unmount"><SettingRow label="x" /></SettingsGroup>);
    expect(v.container.querySelector("h3")?.textContent).toBe("Solo");
  });
});

describe("SettingsSearchSection passthroughWhenInactive and className", () => {
  function Host({ q, passthrough, className }: { q: string; passthrough?: boolean; className?: string }) {
    return (
      <SettingsSearchProvider query={q}>
        <SettingsSearchSection id="look" title="Appearance" passthroughWhenInactive={passthrough} className={className}>
          <SettingRow label="Dark mode" />
        </SettingsSearchSection>
      </SettingsSearchProvider>
    );
  }

  it("with no query the children render untouched: no section, no heading, no result class", () => {
    const v = render(<Host q="" passthrough />);
    expect(v.container.querySelector("section")).toBeNull();
    expect(v.container.querySelector("h2")).toBeNull();
    expect(v.container.querySelector(".rk-settings__result")).toBeNull();
    expect(screen.getByText("Dark mode")).toBeTruthy();
  });

  it("with a query the section and its heading come back", () => {
    const v = render(<Host q="dark" passthrough className="my-sec" />);
    expect(v.container.querySelector("section")?.className).toBe("rk-settings__result my-sec");
    expect(screen.getByRole("heading", { level: 2, name: "Appearance" })).toBeTruthy();
    expect(screen.getByText("Dark mode")).toBeTruthy();
  });

  it("without the flag an inactive search still renders the section (0.4.0 default)", () => {
    const v = render(<Host q="" />);
    expect(v.container.querySelector("section.rk-settings__result")).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Appearance" })).toBeTruthy();
  });

  it("passthrough also applies outside a provider, where no search is active", () => {
    const v = render(
      <SettingsSearchSection id="look" title="Appearance" passthroughWhenInactive>
        <p>plain body</p>
      </SettingsSearchSection>,
    );
    expect(v.container.innerHTML).toBe("<p>plain body</p>");
  });
});
