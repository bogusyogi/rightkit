import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { AutoUpdateClient } from "@rightkit/app-update";
import { SectionFrame, SettingsPanel, aboutSection, appearanceSection, createMemorySettings, legalSection, licenseSection, standardSections, updatesSection } from "./index.js";
import type { SettingsContext, SettingsSection } from "./index.js";

const ctx: SettingsContext = { values: {}, set: async () => {}, patch: async () => {}, saving: false, error: null, clearError: () => {} };
const client: AutoUpdateClient = { checkOnce: async () => ({ kind: "idle" }), installAvailable: async () => ({ kind: "idle" }) };
const html = (s: SettingsSection) => renderToStaticMarkup(<>{s.render(ctx)}</>);
const about = { appName: "Acme", version: "1.0.0" };

describe("updates auto-install toggle", () => {
  it("is hidden by default", () => {
    const s = updatesSection({ client, currentVersion: "1.0.0" });
    expect(html(s)).not.toContain("Install updates automatically");
    expect(s.entries?.some((e) => e.id === "updates.auto")).toBe(false);
  });
  it("is shown when the backend declares support", () => {
    const s = updatesSection({ client, currentVersion: "1.0.0", supportsAutoInstall: true });
    expect(html(s)).toContain("Install updates automatically");
    expect(s.entries?.some((e) => e.id === "updates.auto")).toBe(true);
  });
  it("labels are overridable", () => {
    const s = updatesSection({ client, currentVersion: "1.0.0", supportsAutoInstall: true, labels: { checkNow: "Look now", autoInstall: "Auto", version: (v) => `v${v}` } });
    const h = html(s);
    expect(h).toContain("Look now");
    expect(h).toContain("v1.0.0");
    expect(h).toContain("Auto");
    expect(h).not.toContain("Check now");
    expect(h).toContain("Not checked yet.");
  });
});

describe("nav and heading seams", () => {
  const sections: SettingsSection[] = [{ id: "plan", label: "Plan", badge: <b>FREE</b>, heading: <em>Your plan</em>, render: () => <p>body</p> }, { id: "plain", label: "Plain", render: () => null }];
  it("renders a nav badge only when given", () => {
    const h = renderToStaticMarkup(<SettingsPanel backend={createMemorySettings()} sections={sections} />);
    expect(h).toContain('<span class="rk-settings__badge"><b>FREE</b></span>');
    expect(h.match(/rk-settings__badge/g)).toHaveLength(1);
  });
  it("heading differs from nav label; defaults to label", () => {
    expect(renderToStaticMarkup(<SectionFrame section={sections[0]!} ctx={ctx} />)).toContain("<h1");
    expect(renderToStaticMarkup(<SectionFrame section={sections[0]!} ctx={ctx} />)).toContain("<em>Your plan</em>");
    expect(renderToStaticMarkup(<SectionFrame section={sections[0]!} ctx={ctx} />)).not.toContain(">Plan<");
    expect(renderToStaticMarkup(<SectionFrame section={sections[1]!} ctx={ctx} />)).toContain(">Plain</h1>");
  });
});

describe("built-in icons", () => {
  it("appearance and legal have nav icons", () => {
    expect(appearanceSection().icon).toBeTruthy();
    expect(legalSection({ documents: [] }).icon).toBeTruthy();
  });
  it("every standard section has one", () => {
    for (const s of standardSections({ about, updates: { client, currentVersion: "1" } })) expect(s.icon, s.id).toBeTruthy();
  });
});

describe("license planSlot", () => {
  it("renders above the licence rows", () => {
    const s = licenseSection({ client: { load: async () => null } as never, planSlot: <div id="plan-slot">Upgrade</div> });
    const h = html(s);
    expect(h).toContain('id="plan-slot"');
    expect(h.indexOf("plan-slot")).toBeLessThan(h.indexOf("license.status"));
  });
});

describe("about panels slot", () => {
  it("renders outside the card; in-card extra still works", () => {
    const h = html(aboutSection({ ...about, extra: <i id="in-card" />, panels: <section id="outside" /> }));
    expect(h).toContain('id="in-card"');
    expect(h.indexOf("</section><section id=\"outside\"")).toBeGreaterThan(h.indexOf("in-card"));
    expect(h).toMatch(/id="in-card"[\s\S]*<\/section><section id="outside"/);
  });
});

describe("standardSections selection", () => {
  const ids = (c: Parameters<typeof standardSections>[0]) => standardSections(c).map((s) => s.id);
  it("about is optional; existing config still works", () => {
    expect(ids({})).toEqual(["appearance"]);
    expect(ids({ about })).toEqual(["appearance", "about"]);
    expect(ids({ about, appearance: false })).toEqual(["about"]);
  });
  it("include / omit pick built-ins by id", () => {
    expect(ids({ about, omit: ["appearance"] })).toEqual(["about"]);
    expect(ids({ about, include: ["about"] })).toEqual(["about"]);
  });
  it("override merges fields but keeps the id", () => {
    const [a] = standardSections({ override: { appearance: { label: "Look", badge: "NEW", id: "x" } as never } });
    expect(a).toMatchObject({ id: "appearance", label: "Look", badge: "NEW" });
  });
});
