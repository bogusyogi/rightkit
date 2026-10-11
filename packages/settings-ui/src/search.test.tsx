// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { SettingRow, SettingsGroup } from "./primitives.js";
import { HighlightedText, SettingsItem, SettingsSearchProvider, SettingsSearchSection, SettingsSectionScope, highlightText, parseSearchTerms, useSettingsSearch } from "./core.js";

afterEach(() => cleanup());

function Probe({ onState }: { onState: (s: ReturnType<typeof useSettingsSearch>) => void }) {
  onState(useSettingsSearch());
  return null;
}
const labels = (c: HTMLElement) => [...c.querySelectorAll(".rk-sr__label")].map((e) => e.textContent);

describe("without a provider", () => {
  it("rows, groups and items are inert: nothing hides, nothing is marked", () => {
    const v = render(
      <SettingsGroup title="G" hideWhenEmpty>
        <SettingRow label="One" description="d" />
        <SettingsItem text="two" className="x">item</SettingsItem>
      </SettingsGroup>,
    );
    expect(v.container.querySelector("section")?.getAttribute("style")).toBeNull();
    expect(v.container.querySelector(".x")?.textContent).toBe("item");
    expect(v.container.querySelector("mark")).toBeNull();
  });
});

describe("registration", () => {
  it("rows, groups and items register on mount with label, description, group, section and keywords, and unregister on unmount", () => {
    let state!: ReturnType<typeof useSettingsSearch>;
    const tree = (show: boolean) => (
      <SettingsSearchProvider query="">
        <Probe onState={(s) => (state = s)} />
        <SettingsSectionScope id="sec" title="Section" keywords={["nav"]}>
          <SettingsGroup title="Group">
            <SettingRow id="r1" label="Row one" description="Desc one" keywords={["k1"]} />
            {show ? <SettingsItem id="i1" text="Item one" keywords={["k2"]}>x</SettingsItem> : null}
          </SettingsGroup>
        </SettingsSectionScope>
      </SettingsSearchProvider>
    );
    const v = render(tree(true));
    expect(state.count).toBe(2);
    expect(state.results).toEqual([
      { kind: "row", id: "r1", label: "Row one", description: "Desc one", keywords: ["k1"], group: "Group", section: "Section", sectionId: "sec" },
      { kind: "item", id: "i1", label: "Item one", description: undefined, keywords: ["k2"], group: "Group", section: "Section", sectionId: "sec" },
    ]);
    v.rerender(tree(false));
    expect(state.results.map((r) => r.id)).toEqual(["r1"]);
    v.unmount();
  });

  it("matches label, description, keywords, group title, section title and section keywords; every term must match", () => {
    let state!: ReturnType<typeof useSettingsSearch>;
    const at = (q: string) => (
      <SettingsSearchProvider query={q} filter={false}>
        <Probe onState={(s) => (state = s)} />
        <SettingsSectionScope id="s" title="Privacy" keywords={["secure"]}>
          <SettingsGroup title="Telemetry">
            <SettingRow label="Share usage" description="Anonymous counts" keywords={["analytics"]} />
          </SettingsGroup>
        </SettingsSectionScope>
      </SettingsSearchProvider>
    );
    const v = render(at(""));
    for (const [q, n] of [["share", 1], ["anonymous", 1], ["analytics", 1], ["telemetry", 1], ["privacy", 1], ["secure", 1], ["share privacy", 1], ["share nope", 0], ["zzz", 0]] as const) {
      v.rerender(at(q));
      expect([q, state.count]).toEqual([q, n]);
    }
  });

  it("a conditional row that is not rendered never appears, and a row that mounts late appears without any observer", () => {
    let state!: ReturnType<typeof useSettingsSearch>;
    function Late() {
      const [on, setOn] = useState(false);
      return (
        <>
          <button type="button" onClick={() => setOn(true)}>load</button>
          <SettingRow label="Always" />
          {on ? <SettingRow label="Late row" /> : null}
        </>
      );
    }
    render(
      <SettingsSearchProvider query="row">
        <Probe onState={(s) => (state = s)} />
        <Late />
      </SettingsSearchProvider>,
    );
    expect(state.results.map((r) => r.label)).toEqual([]);
    expect(screen.queryByText("Late row")).toBeNull();
    act(() => screen.getByText("load").click());
    expect(state.results.map((r) => r.label)).toEqual(["Late row"]);
    expect(screen.getByText("Late row")).toBeTruthy();
  });

  it("a changed label re-registers (the old text stops matching)", () => {
    let state!: ReturnType<typeof useSettingsSearch>;
    const at = (label: string) => (
      <SettingsSearchProvider query="">
        <Probe onState={(s) => (state = s)} />
        <SettingRow label={label} />
      </SettingsSearchProvider>
    );
    const v = render(at("Before"));
    v.rerender(at("After"));
    expect(state.results.map((r) => r.label)).toEqual(["After"]);
  });

  it("searchText stands in for a label that is not a string", () => {
    let state!: ReturnType<typeof useSettingsSearch>;
    render(
      <SettingsSearchProvider query="node">
        <Probe onState={(s) => (state = s)} />
        <SettingRow label={<b>Fancy</b>} searchText="plain node text" />
      </SettingsSearchProvider>,
    );
    expect(state.count).toBe(1);
  });
});

describe("filtering", () => {
  it("rows and items that do not match are not rendered; matches keep their controls", () => {
    const v = render(
      <SettingsSearchProvider query="dark">
        <SettingRow label="Dark mode"><button type="button">Toggle</button></SettingRow>
        <SettingRow label="Language" />
        <SettingsItem text="Account list" className="acct">accounts</SettingsItem>
        <SettingsItem text="Dark accounts" className="dark">dark list</SettingsItem>
      </SettingsSearchProvider>,
    );
    expect(labels(v.container)).toEqual(["Dark mode"]);
    expect(screen.getByRole("button", { name: "Toggle" })).toBeTruthy();
    expect(v.container.querySelector(".acct")).toBeNull();
    expect(v.container.querySelector(".dark")?.textContent).toBe("dark list");
  });

  it("a group whose title matches keeps all its rows", () => {
    const v = render(
      <SettingsSearchProvider query="startup">
        <SettingsGroup title="Startup"><SettingRow label="A" /><SettingRow label="B" /></SettingsGroup>
        <SettingsGroup title="Other"><SettingRow label="C" /></SettingsGroup>
      </SettingsSearchProvider>,
    );
    expect(labels(v.container)).toEqual(["A", "B"]);
  });

  it("hideWhenEmpty hides a group with no visible row and shows it again when a row matches or loads late", () => {
    function Host({ q, late }: { q: string; late: boolean }) {
      return (
        <SettingsSearchProvider query={q}>
          <SettingsGroup title="Alpha" hideWhenEmpty><SettingRow label="alpha row" />{late ? <SettingRow label="gamma late" /> : null}</SettingsGroup>
          <SettingsGroup title="Beta" hideWhenEmpty><SettingRow label="beta row" /></SettingsGroup>
          <SettingsGroup title="Never" hideWhenEmpty>{false}</SettingsGroup>
        </SettingsSearchProvider>
      );
    }
    const v = render(<Host q="" late={false} />);
    const sections = () => [...v.container.querySelectorAll("section")].map((s) => [s.querySelector("h3")?.textContent, s.style.display]);
    expect(sections()).toEqual([["Alpha", ""], ["Beta", ""], ["Never", "none"]]);
    v.rerender(<Host q="beta" late={false} />);
    expect(sections()).toEqual([["Alpha", "none"], ["Beta", ""], ["Never", "none"]]);
    v.rerender(<Host q="gamma" late={false} />);
    expect(sections().map((s) => s[1])).toEqual(["none", "none", "none"]);
    v.rerender(<Host q="gamma" late />);
    expect(sections()).toEqual([["Alpha", ""], ["Beta", "none"], ["Never", "none"]]);
    // Re-evaluated when registrations change while the query stays put.
    v.rerender(<Host q="gamma" late={false} />);
    expect(sections()[0]).toEqual(["Alpha", "none"]);
  });

  it("SettingsSearchSection renders a real heading, an Open action naming the section, and hides when nothing in it matches", () => {
    const opened: string[] = [];
    const v = render(
      <SettingsSearchProvider query="dark">
        <SettingsSearchSection id="look" title="Appearance" onOpen={(id) => opened.push(id)}><SettingRow label="Dark mode" /></SettingsSearchSection>
        <SettingsSearchSection id="data" title="Data" headingLevel={3}><SettingRow label="Export" /></SettingsSearchSection>
      </SettingsSearchProvider>,
    );
    expect(screen.getByRole("heading", { level: 2, name: "Appearance" })).toBeTruthy();
    screen.getByRole("button", { name: "Open section Appearance" }).click();
    expect(opened).toEqual(["look"]);
    const data = v.container.querySelector<HTMLElement>('[data-section-id="data"]')!;
    expect(data.style.display).toBe("none");
    expect(data.querySelector("h3")?.textContent).toBe("Data");
  });
});

describe("highlight", () => {
  const rows = (highlight?: boolean | ((m: string) => React.ReactNode)) => (
    <SettingsSearchProvider query="dark mo" highlight={highlight}>
      <SettingsGroup title="Dark things">
        <SettingRow label="Dark mode" description="Switch to a dark theme"><input aria-label="Dark mode toggle" /></SettingRow>
      </SettingsGroup>
    </SettingsSearchProvider>
  );
  it("is off by default: no extra elements", () => {
    const v = render(rows());
    expect(v.container.querySelector("mark")).toBeNull();
    expect(v.container.querySelector(".rk-sr__label")?.innerHTML).toBe("Dark mode");
  });
  it("true wraps each term in <mark>, text content and accessible names unchanged", () => {
    const v = render(rows(true));
    expect(v.container.querySelector(".rk-sr__label")?.innerHTML).toBe('<mark class="rk-settings__mark">Dark</mark> <mark class="rk-settings__mark">mo</mark>de');
    expect(v.container.querySelector(".rk-sr__label")?.textContent).toBe("Dark mode");
    expect(v.container.querySelector(".rk-sg__title")?.textContent).toBe("Dark things");
    expect(screen.getByRole("textbox", { name: "Dark mode toggle" })).toBeTruthy();
    expect(v.container.querySelector(".rk-sr__desc")?.textContent).toBe("Switch to a dark theme");
  });
  it("a function renders each matched piece", () => {
    const v = render(rows((m) => <em>{m}</em>));
    expect(v.container.querySelector(".rk-sr__label em")?.textContent).toBe("Dark");
  });
  it("highlightText merges overlapping terms and leaves non-matching text alone", () => {
    const html = (n: React.ReactNode) => render(<p>{n}</p>).container.innerHTML;
    expect(html(highlightText("abcdef", ["abc", "cde"], true))).toBe('<p><mark class="rk-settings__mark">abcde</mark>f</p>');
    expect(highlightText("abc", ["x"], true)).toBe("abc");
    expect(highlightText("abc", [], true)).toBe("abc");
    expect(highlightText("abc", ["a"], false)).toBe("abc");
    expect(parseSearchTerms("  Dark   MODE ")).toEqual(["dark", "mode"]);
  });
  it("HighlightedText marks text inside item content", () => {
    const v = render(
      <SettingsSearchProvider query="acc" highlight>
        <SettingsItem text="accounts"><HighlightedText text="accounts" /></SettingsItem>
      </SettingsSearchProvider>,
    );
    expect(v.container.querySelector("mark")?.textContent).toBe("acc");
  });
});
