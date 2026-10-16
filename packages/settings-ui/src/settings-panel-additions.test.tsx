// @vitest-environment jsdom
import { act, cleanup, render, renderHook, screen, fireEvent } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { SectionFrame, SettingsPanel } from "./SettingsPanel.js";
import { createMemorySettings } from "./backends.js";
import { useSettings } from "./useSettings.js";
import type { SettingsContext, SettingsSection } from "./types.js";

afterEach(() => cleanup());

// jsdom rewrites import.meta.url, so read the stylesheet from the package root (vitest runs there).
const settingsCss = readFileSync(resolve(process.cwd(), "settings.css"), "utf8");
const ruleOf = (selector: string) => {
  const start = settingsCss.indexOf(`${selector} {`);
  if (start === -1) throw new Error(`no rule for ${selector}`);
  return settingsCss.slice(start, settingsCss.indexOf("}", start));
};

const sections: SettingsSection[] = [
  { id: "alpha", label: "Alpha", order: 100, render: () => <p>Alpha body</p> },
  { id: "beta", label: "Beta", heading: "Beta heading", order: 200, hideSectionHeading: false, render: () => <p>Beta body</p> },
  { id: "gamma", label: "Gamma", order: 300, hideSectionHeading: true, render: () => <p>Gamma body</p> },
];

const ctx: SettingsContext = { values: {}, set: async () => {}, patch: async () => {}, saving: false, error: null, clearError: () => {} };

describe("hideSectionHeading", () => {
  it("removes the h1 for every section when the panel sets it, and keeps the body", async () => {
    render(<SettingsPanel backend={createMemorySettings()} sections={sections} hideSectionHeading />);
    await screen.findByText("Alpha body");
    expect(document.querySelector("h1.rk-settings__h1")).toBeNull();
    const article = document.querySelector("article.rk-settings__section")!;
    expect(article.getAttribute("aria-label")).toBe("Alpha");
    expect(article.hasAttribute("aria-labelledby")).toBe(false);
    expect(article.textContent).toContain("Alpha body");
  });
  it("keeps the h1 by default", async () => {
    const v = render(<SettingsPanel backend={createMemorySettings()} sections={sections} />);
    await screen.findByText("Alpha body");
    expect(v.container.querySelector("h1.rk-settings__h1")?.textContent).toBe("Alpha");
  });
  it("a section value overrides the panel value in both directions", async () => {
    const v = render(<SettingsPanel backend={createMemorySettings()} sections={sections} hideSectionHeading defaultActiveId="beta" />);
    await screen.findByText("Beta body");
    expect(v.container.querySelector("h1.rk-settings__h1")?.textContent).toBe("Beta heading");
    fireEvent.click(v.container.querySelector('[data-section-id="gamma"]')!);
    expect(v.container.querySelector("h1.rk-settings__h1")).toBeNull();
    expect(screen.getByText("Gamma body")).toBeTruthy();
  });
  it("SectionFrame honours its own flag and the section flag", () => {
    const html = (s: SettingsSection, flag?: boolean) => {
      const v = render(<SectionFrame section={s} ctx={ctx} hideSectionHeading={flag} />);
      const out = v.container.innerHTML;
      v.unmount();
      return out;
    };
    expect(html(sections[0]!)).toContain("<h1");
    expect(html(sections[0]!, true)).not.toContain("<h1");
    expect(html({ ...sections[0]!, hideSectionHeading: true })).not.toContain("<h1");
    expect(html({ ...sections[0]!, hideSectionHeading: false }, true)).toContain("<h1");
  });
});

describe("initial settings state", () => {
  it("renders the active section on the first render with no loading text", async () => {
    const backend = createMemorySettings({ theme: "dark" });
    const v = render(<SettingsPanel backend={backend} sections={sections} initial={backend.snapshot()} />);
    expect(screen.queryByText("Loading settings…")).toBeNull();
    expect(v.container.textContent).toContain("Alpha body");
    await act(async () => {});
    expect(screen.queryByText("Loading settings…")).toBeNull();
  });
  it("still shows the loading text when no initial state is given", async () => {
    render(<SettingsPanel backend={createMemorySettings()} sections={sections} />);
    expect(screen.getByText("Loading settings…")).toBeTruthy();
    await screen.findByText("Alpha body");
  });
  it("useSettings starts loaded with the snapshot values and revision, so the first write does not conflict", async () => {
    const backend = createMemorySettings({ theme: "dark" });
    const snap = backend.snapshot();
    const { result } = renderHook(() => useSettings(backend, { initial: snap }));
    expect(result.current.loaded).toBe(true);
    expect(result.current.values).toEqual({ theme: "dark" });
    await act(() => result.current.set("theme", "light"));
    expect(result.current.values).toEqual({ theme: "light" });
    expect(result.current.error).toBeNull();
  });
  it("useSettings without options still starts unloaded", () => {
    const { result } = renderHook(() => useSettings(createMemorySettings()));
    expect(result.current.loaded).toBe(false);
  });
});

describe("settings.css tokens", () => {
  it("keeps today's values as fallbacks", () => {
    expect(ruleOf(".rk-settings")).toContain("font-size: var(--rk-settings-font-size, 13.5px)");
    expect(ruleOf(".rk-settings__body")).toContain("padding: var(--rk-settings-padding, 24px 32px 40px)");
    expect(settingsCss).toContain(".rk-settings__body { padding: var(--rk-settings-padding, 18px 16px 32px); }");
  });
});

describe("0.4.5 frame options (#174)", () => {
  const stateful: SettingsSection[] = [
    { id: "one", label: "One", order: 1, render: () => <input aria-label="draft" defaultValue="" /> },
    { id: "two", label: "Two", order: 2, render: () => <p>Two body</p> },
  ];
  const mount = (props: Partial<React.ComponentProps<typeof SettingsPanel>> = {}) =>
    render(<SettingsPanel backend={createMemorySettings()} sections={stateful} {...props} />);
  const nav = (name: string) => document.querySelector<HTMLButtonElement>(`button[data-section-id="${name}"]`)!;

  it("keepMounted keeps a visited section's state while another is shown", async () => {
    mount({ keepMounted: true });
    const input = (await screen.findByLabelText("draft", { selector: "input" })) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "typed" } });
    fireEvent.click(nav("two"));
    await screen.findByText("Two body");
    const kept = document.querySelector<HTMLElement>(".rk-settings__kept")!;
    expect(kept.hidden).toBe(true);
    fireEvent.click(nav("one"));
    expect(document.querySelector<HTMLElement>(".rk-settings__kept")!.hidden).toBe(false);
    expect((screen.getByLabelText("draft") as HTMLInputElement).value).toBe("typed");
  });
  it("without keepMounted the section remounts and loses state, with no wrapper (default markup)", async () => {
    mount();
    const input = (await screen.findByLabelText("draft", { selector: "input" })) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "typed" } });
    fireEvent.click(nav("two"));
    await screen.findByText("Two body");
    expect(document.querySelector(".rk-settings__kept")).toBeNull();
    fireEvent.click(nav("one"));
    expect((screen.getByLabelText("draft") as HTMLInputElement).value).toBe("");
  });
  it("paint={false} and nativePreSize add their classes; the defaults add neither", async () => {
    const v = mount({ paint: false, nativePreSize: true });
    await screen.findByLabelText("draft");
    const root = v.container.querySelector(".rk-settings")!;
    expect(root).toHaveProperty("className", "rk-settings rk-settings--no-paint rk-settings--native-pre");
    v.unmount();
    const d = mount();
    await screen.findByLabelText("draft");
    expect(d.container.querySelector(".rk-settings")!.className).toBe("rk-settings");
  });
  it("the stylesheet unpaints and restores the native pre size only under the new classes", () => {
    expect(ruleOf(".rk-settings--no-paint")).toContain("color: inherit");
    expect(ruleOf(".rk-settings--no-paint")).toContain("background: none");
    expect(ruleOf(".rk-settings--native-pre pre:not(.rk-diag)")).toContain("font-size: medium");
    expect(ruleOf(".rk-settings")).toContain("background: var(--rk-plane)");
  });
});
