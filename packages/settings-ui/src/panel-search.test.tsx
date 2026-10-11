// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SettingRow, SettingsGroup } from "./primitives.js";
import { SettingsPanel, type SettingsPanelProps } from "./SettingsPanel.js";
import { createMemorySettings } from "./backends.js";
import { SettingsItem, SettingsSearchProvider } from "./search.js";
import type { SettingsContext, SettingsSection } from "./types.js";

afterEach(() => cleanup());
(globalThis as { CSS?: unknown }).CSS ??= { escape: (v: string) => v };
Element.prototype.scrollIntoView ??= () => {};

function Toggle({ ctx, k, label }: { ctx: SettingsContext; k: string; label: string }) {
  return <input type="checkbox" aria-label={label} checked={ctx.values[k] === true} onChange={(e) => void ctx.set(k, e.target.checked)} />;
}

const sections: SettingsSection[] = [
  {
    id: "look", label: "Appearance", order: 100,
    render: (ctx) => (
      <SettingsGroup title="Theme">
        <SettingRow label="Dark mode" description="Use the dark theme"><Toggle ctx={ctx} k="dark" label="Dark mode switch" /></SettingRow>
        <SettingRow label="Accent colour" />
      </SettingsGroup>
    ),
  },
  {
    id: "privacy", label: "Privacy", order: 200,
    render: (ctx) => (
      <>
        <SettingsGroup title="Data" hideWhenEmpty>
          <SettingRow label="Share telemetry"><Toggle ctx={ctx} k="telemetry" label="Telemetry switch" /></SettingRow>
          {ctx.values.hasKey ? <SettingRow label="Backend only row" /> : null}
        </SettingsGroup>
        <SettingsGroup title="Accounts" hideWhenEmpty>
          <SettingsItem text="Connected accounts dark" className="accounts"><ul><li>me@example.com</li></ul></SettingsItem>
        </SettingsGroup>
      </>
    ),
  },
  { id: "about", label: "About", order: 300, render: () => <SettingsGroup title="Version"><SettingRow label="Build number" /></SettingsGroup> },
];

const panel = (props: Partial<SettingsPanelProps> = {}) => render(<SettingsPanel sections={sections} searchMode="all-sections" {...props} />);
const search = () => screen.getByLabelText("Search settings") as HTMLInputElement;
const type = (value: string) => fireEvent.change(search(), { target: { value } });
const headings = (c: HTMLElement) => [...c.querySelectorAll("h2.rk-settings__result-title")].filter((h) => (h.closest("section") as HTMLElement).style.display !== "none").map((h) => h.textContent);

describe("all-sections mode", () => {
  it("renders every section's matching rows with live controls, grouped by section, and writes through the controls", async () => {
    const backend = createMemorySettings();
    const v = panel({ backend });
    await screen.findByText("Dark mode");
    type("dark");
    expect(headings(v.container)).toEqual(["Appearance", "Privacy"]);
    const rows = [...v.container.querySelectorAll(".rk-sr__label, .accounts")].map((e) => e.textContent);
    expect(rows).toEqual(["Dark mode", "me@example.com"]);
    await act(async () => fireEvent.click(screen.getByRole("checkbox", { name: "Dark mode switch" })));
    await waitFor(() => expect(backend.snapshot().values.dark).toBe(true));
    expect((screen.getByRole("checkbox", { name: "Dark mode switch" }) as HTMLInputElement).checked).toBe(true);
    // Sections with nothing matching are hidden, not shown empty.
    const about = v.container.querySelector<HTMLElement>('section[data-section-id="about"]')!;
    expect(about.style.display).toBe("none");
  });

  it("openSectionLabel as a function supplies the whole action text", async () => {
    panel({ backend: createMemorySettings(), openSectionLabel: (title) => `Open ${title}` });
    await screen.findByText("Dark mode");
    type("telemetry");
    expect(screen.getByRole("button", { name: "Open Privacy" })).toBeTruthy();
  });

  it("section headings are real headings and Open section opens that section and clears the query", async () => {
    const onActive = vi.fn();
    const v = panel({ backend: createMemorySettings(), onActiveChange: onActive });
    await screen.findByText("Dark mode");
    type("telemetry");
    expect(screen.getByRole("heading", { level: 2, name: "Privacy" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open section Privacy" }));
    expect(onActive).toHaveBeenCalledWith("privacy");
    expect(search().value).toBe("");
    expect(v.container.querySelector("h1")?.textContent).toBe("Privacy");
    expect(v.container.querySelector("h2.rk-settings__result-title")).toBeNull();
  });

  it("clearing the query restores the active section and the scroll position", async () => {
    const v = panel({ backend: createMemorySettings(), defaultActiveId: "privacy" });
    await screen.findByText("Share telemetry");
    const body = v.container.querySelector<HTMLElement>(".rk-settings__body")!;
    body.scrollTop = 120;
    type("dark");
    expect(body.scrollTop).toBe(0);
    expect(v.container.querySelector("h1")).toBeNull();
    type("");
    expect(v.container.querySelector("h1")?.textContent).toBe("Privacy");
    expect(body.scrollTop).toBe(120);
  });

  it("the nav stays whole while searching and clicking a section leaves the search", async () => {
    const v = panel({ backend: createMemorySettings() });
    await screen.findByText("Dark mode");
    type("zzz-nothing");
    expect(v.container.querySelectorAll("button[data-section-id]")).toHaveLength(3);
    expect(v.container.querySelector(".rk-settings__body .rk-settings__none")?.textContent).toContain("zzz-nothing");
    fireEvent.click(v.container.querySelector('button[data-section-id="about"]')!);
    expect(search().value).toBe("");
    expect(v.container.querySelector("h1")?.textContent).toBe("About");
  });

  it("a row the backend does not publish is not a result; it appears once it loads, without an observer", async () => {
    const backend = createMemorySettings();
    const v = panel({ backend });
    await screen.findByText("Dark mode");
    type("backend only");
    expect(screen.queryByText("Backend only row")).toBeNull();
    expect(v.container.querySelector(".rk-settings__body .rk-settings__none")).toBeTruthy();
    await act(async () => void (await backend.patch({ hasKey: true })));
    await waitFor(() => expect(screen.getByText("Backend only row")).toBeTruthy());
    expect(headings(v.container)).toEqual(["Privacy"]);
    expect(v.container.querySelector(".rk-settings__body .rk-settings__none")).toBeNull();
  });

  it("highlight marks matches in rows and section titles and keeps accessible names", async () => {
    const v = panel({ backend: createMemorySettings(), highlight: true });
    await screen.findByText("Dark mode");
    type("dark");
    expect(v.container.querySelectorAll("mark.rk-settings__mark").length).toBeGreaterThan(0);
    expect(screen.getByRole("checkbox", { name: "Dark mode switch" })).toBeTruthy();
    expect(v.container.querySelector(".rk-sr__label")?.textContent).toBe("Dark mode");
  });
});

describe("controlled, backend-free use", () => {
  it("renders with no backend: sections get a working context and no loading flash", () => {
    const v = panel();
    expect(screen.queryByText("Loading settings…")).toBeNull();
    expect(v.container.querySelector("h1")?.textContent).toBe("Appearance");
  });

  it("sections can keep their own state without a backend", async () => {
    function Own() {
      const [n, setN] = useState(0);
      return <SettingRow label="Counter"><button type="button" onClick={() => setN(n + 1)}>clicks {n}</button></SettingRow>;
    }
    panel({ sections: [{ id: "own", label: "Own", render: () => <Own /> }] });
    fireEvent.click(screen.getByText("clicks 0"));
    expect(screen.getByText("clicks 1")).toBeTruthy();
  });

  it("ctx.set works against the panel's in-memory store when there is no backend", async () => {
    panel();
    await act(async () => fireEvent.click(screen.getByRole("checkbox", { name: "Dark mode switch" })));
    await waitFor(() => expect((screen.getByRole("checkbox", { name: "Dark mode switch" }) as HTMLInputElement).checked).toBe(true));
  });

  it("controlled query and active section", () => {
    const onQuery = vi.fn();
    const onActive = vi.fn();
    const v = panel({ query: "dark", onQueryChange: onQuery, activeSection: "about", onActiveSectionChange: onActive });
    expect(search().value).toBe("dark");
    expect(headings(v.container)).toEqual(["Appearance", "Privacy"]);
    fireEvent.change(search(), { target: { value: "dar" } });
    expect(onQuery).toHaveBeenCalledWith("dar");
    expect(search().value).toBe("dark"); // controlled: the host decides
    fireEvent.click(screen.getByRole("button", { name: "Open section Appearance" }));
    expect(onActive).toHaveBeenCalledWith("look");
    expect(onQuery).toHaveBeenLastCalledWith("");
  });

  it("activeSection wins over activeId and both callbacks fire", () => {
    const a = vi.fn();
    const b = vi.fn();
    const v = render(<SettingsPanel sections={sections} activeId="look" activeSection="about" onActiveChange={a} onActiveSectionChange={b} />);
    expect(v.container.querySelector("h1")?.textContent).toBe("About");
    fireEvent.click(v.container.querySelector('button[data-section-id="privacy"]')!);
    expect(a).toHaveBeenCalledWith("privacy");
    expect(b).toHaveBeenCalledWith("privacy");
  });

  it("hideNav no longer disables search when asked: the field sits above the content", () => {
    const v = render(<SettingsPanel sections={sections} hideNav search searchMode="all-sections" />);
    expect(v.container.querySelector("nav")).toBeNull();
    expect(v.container.querySelector('.rk-settings__body [role="search"] input')).toBe(search());
    type("telemetry");
    expect(headings(v.container)).toEqual(["Privacy"]);
  });

  it("hideNav with a controlled query and no built-in field still searches (the host owns the input)", () => {
    const v = render(<SettingsPanel sections={sections} hideNav query="telemetry" searchMode="all-sections" />);
    expect(screen.queryByLabelText("Search settings")).toBeNull();
    expect(headings(v.container)).toEqual(["Privacy"]);
  });

  it("hideSearch removes the field but keeps the nav", () => {
    const v = render(<SettingsPanel sections={sections} hideSearch />);
    expect(screen.queryByLabelText("Search settings")).toBeNull();
    expect(v.container.querySelector("nav")).toBeTruthy();
  });

  it("search={false} still removes the field, and searchLabel renames it", () => {
    const v = render(<SettingsPanel sections={sections} search={false} />);
    expect(screen.queryByLabelText("Search settings")).toBeNull();
    v.unmount();
    render(<SettingsPanel sections={sections} searchLabel="Find a setting" />);
    expect(screen.getByLabelText("Find a setting").getAttribute("placeholder")).toBe("Find a setting");
  });
});

describe("navigate mode with the provider available", () => {
  it("never hides rows and highlights only when asked", async () => {
    const withEntries = sections.map((s) => (s.id === "look" ? { ...s, entries: [{ id: "dark", label: "Dark mode" }] } : s));
    const v = render(<SettingsPanel backend={createMemorySettings()} sections={withEntries} highlight />);
    await screen.findByText("Dark mode");
    type("dark");
    expect(v.container.querySelectorAll(".rk-sr__label")).toHaveLength(2);
    expect(v.container.querySelector(".rk-sr__label mark")?.textContent).toBe("Dark");
    expect(v.container.querySelector(".rk-settings__sr")).toBeNull();
  });
});

describe("keyboard", () => {
  const withSearch = (props: Partial<SettingsPanelProps> = {}) => {
    const v = panel({ backend: createMemorySettings(), ...props });
    return v;
  };
  it("find shortcut is off by default", async () => {
    withSearch();
    fireEvent.keyDown(document.body, { key: "f", metaKey: true });
    fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
    expect(document.activeElement).toBe(document.body);
  });
  it("findShortcut focuses and selects the field with the platform modifier and prevents the browser's find", async () => {
    withSearch({ findShortcut: true });
    type("abc");
    const mac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
    const wrong = fireEvent.keyDown(document.body, { key: "f", ...(mac ? { ctrlKey: true } : { metaKey: true }) });
    expect(wrong).toBe(true);
    expect(document.activeElement).toBe(document.body);
    const ok = fireEvent.keyDown(document.body, { key: "F", ...(mac ? { metaKey: true } : { ctrlKey: true }) });
    expect(ok).toBe(false); // preventDefault called
    expect(document.activeElement).toBe(search());
    expect(search().selectionStart).toBe(0);
    expect(search().selectionEnd).toBe(3);
  });
  it("findShortcut ignores IME composition and accepts a custom matcher", async () => {
    withSearch({ findShortcut: (e) => e.key === "/" });
    fireEvent.keyDown(document.body, { key: "/", isComposing: true });
    expect(document.activeElement).toBe(document.body);
    fireEvent.keyDown(document.body, { key: "/" });
    expect(document.activeElement).toBe(search());
  });
  it("Escape does nothing without a policy", async () => {
    withSearch();
    type("abc");
    fireEvent.keyDown(search(), { key: "Escape" });
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(search().value).toBe("abc");
  });
  it("always: clears from anywhere", async () => {
    withSearch({ escapeClears: "always" });
    type("abc");
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(search().value).toBe("");
  });
  it("when-search-focused: clears only from the field", async () => {
    withSearch({ escapeClears: "when-search-focused" });
    type("abc");
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(search().value).toBe("abc");
    fireEvent.keyDown(search(), { key: "Escape" });
    expect(search().value).toBe("");
  });
  it("unless-in-input-or-dialog: the page and the field clear; another input and a dialog keep Escape", async () => {
    const v = panel({
      backend: createMemorySettings(),
      escapeClears: "unless-in-input-or-dialog",
      sections: [{ id: "x", label: "X", render: () => <><input aria-label="rename" /><div role="dialog"><button type="button">in dialog</button></div><p>page</p></> }],
    });
    type("abc");
    await screen.findByLabelText("rename");
    fireEvent.keyDown(screen.getByLabelText("rename"), { key: "Escape" });
    expect(search().value).toBe("abc");
    fireEvent.keyDown(screen.getByText("in dialog"), { key: "Escape" });
    expect(search().value).toBe("abc");
    fireEvent.keyDown(screen.getByText("page"), { key: "Escape" });
    expect(search().value).toBe("");
    type("abc");
    fireEvent.keyDown(search(), { key: "Escape" });
    expect(search().value).toBe("");
    v.unmount();
  });
  it("Escape is ignored during IME composition and when something else already handled it", async () => {
    withSearch({ escapeClears: "always" });
    type("abc");
    fireEvent.keyDown(search(), { key: "Escape", isComposing: true });
    fireEvent.keyDown(search(), { key: "Escape", keyCode: 229 });
    expect(search().value).toBe("abc");
    const handled = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    handled.preventDefault();
    document.body.dispatchEvent(handled);
    expect(search().value).toBe("abc");
  });
  it("removes its listener on unmount", () => {
    const v = withSearch({ escapeClears: "always", findShortcut: true });
    const spy = vi.spyOn(window, "removeEventListener");
    v.unmount();
    expect(spy).toHaveBeenCalledWith("keydown", expect.any(Function));
  });
});

describe("accessibility", () => {
  it("the field is labelled, and the polite live region announces the count", async () => {
    const v = panel({ backend: createMemorySettings() });
    await screen.findByText("Dark mode");
    expect(search().getAttribute("aria-label")).toBe("Search settings");
    const live = v.container.querySelector(".rk-settings__sr")!;
    expect(live.getAttribute("role")).toBe("status");
    expect(live.getAttribute("aria-live")).toBe("polite");
    expect(live.textContent).toBe("");
    type("dark");
    await waitFor(() => expect(live.textContent).toBe("2 settings found"), { timeout: 1500 });
    type("telemetry");
    await waitFor(() => expect(live.textContent).toBe("1 setting found"), { timeout: 1500 });
    type("zzz");
    await waitFor(() => expect(live.textContent).toBe("No settings found"), { timeout: 1500 });
    type("");
    await waitFor(() => expect(live.textContent).toBe(""));
  });
  it("the live region text is configurable and opt-in in navigate mode", async () => {
    const v = render(<SettingsPanel backend={createMemorySettings()} sections={sections} announceResults formatAnnouncement={(n) => `${n} hits`} />);
    await screen.findByText("Dark mode");
    expect(v.container.querySelector(".rk-settings__sr")).toBeTruthy();
    type("privacy");
    await waitFor(() => expect(v.container.querySelector(".rk-settings__sr")?.textContent).toBe("1 hits"), { timeout: 1500 });
  });
  it("typing, results changing and opening results never move focus", async () => {
    const v = panel({ backend: createMemorySettings() });
    await screen.findByText("Dark mode");
    search().focus();
    type("dark");
    type("telemetry");
    type("zzz");
    expect(document.activeElement).toBe(search());
    type("dark");
    await act(async () => void (await new Promise((r) => setTimeout(r, 350))));
    expect(document.activeElement).toBe(search());
    expect(within(v.container).getAllByRole("heading", { level: 2 }).length).toBeGreaterThan(0);
  });
});

describe("a host that owns its input", () => {
  it("SettingsSearchProvider drives rows from the host's own query state", () => {
    function Host() {
      const [q, setQ] = useState("");
      return (
        <SettingsSearchProvider query={q}>
          <input aria-label="mine" value={q} onChange={(e) => setQ(e.target.value)} />
          <SettingRow label="Alpha" />
          <SettingRow label="Beta" />
        </SettingsSearchProvider>
      );
    }
    const v = render(<Host />);
    expect(v.container.querySelectorAll(".rk-sr")).toHaveLength(2);
    fireEvent.change(screen.getByLabelText("mine"), { target: { value: "bet" } });
    expect([...v.container.querySelectorAll(".rk-sr__label")].map((e) => e.textContent)).toEqual(["Beta"]);
  });
});
