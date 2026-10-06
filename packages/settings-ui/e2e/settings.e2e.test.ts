import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { findChrome, launch, type Page } from "./browser.js";
import { startHost, type TestHost } from "./server.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const chrome = findChrome();
const shotsDir = process.env.RK_SHOTS_DIR ?? mkdtempSync(join(tmpdir(), "rk-settings-shots-"));

let page: Page;
let close: () => Promise<void>;
let host: TestHost;
let build: string;
let work: string;

const q = (sel: string) => JSON.stringify(sel);
const text = (sel: string) => page.eval<string>(`document.querySelector(${q(sel)})?.textContent ?? ""`);
const exists = (sel: string) => page.eval<boolean>(`!!document.querySelector(${q(sel)})`);
const settingsOnDisk = () => (existsSync(host.settingsFile) ? JSON.parse(readFileSync(host.settingsFile, "utf8")) : { values: {}, revision: 1 }) as { values: Record<string, unknown>; revision: number };
const waitHost = async (check: () => boolean, timeoutMs = 5000) => {
  const start = Date.now();
  while (!check()) {
    if (Date.now() - start > timeoutMs) throw new Error("timed out waiting for host state");
    await page.sleep(40);
  }
};
const open = async (query = "", theme: "light" | "dark" = "light") => {
  await page.setColorScheme(theme);
  await page.setViewport(1180, 760);
  await page.goto(`${host.origin}/index.html?${query}`);
  await page.waitFor("document.querySelector('.rk-settings__section')");
};
const section = (id: string) => page.click(`[data-section-id="${id}"]`);
const clickText = async (label: string, within = "document") => {
  await page.eval(`(() => { const b = [...${within}.querySelectorAll('button')].find(x => x.textContent.trim() === ${JSON.stringify(label)}); if (!b) throw new Error('no button ' + ${JSON.stringify(label)}); b.dataset.rkClick = '1'; })()`);
  await page.click("[data-rk-click='1']");
  await page.eval("document.querySelector('[data-rk-click]')?.removeAttribute('data-rk-click')");
};
const shot = (name: string) => page.screenshot(join(shotsDir, `${name}.png`));

describe.skipIf(!chrome)("settings-ui in real headless Chrome (muted) against a real local host", () => {
  beforeAll(async () => {
    mkdirSync(shotsDir, { recursive: true });
    build = mkdtempSync(join(tmpdir(), "rk-settings-build-"));
    work = mkdtempSync(join(tmpdir(), "rk-settings-work-"));
    execFileSync(process.execPath, [join(pkgRoot, "node_modules/vite/bin/vite.js"), "build", "--config", join(here, "harness/vite.config.mjs"), "--logLevel", "warn"], { cwd: pkgRoot, env: { ...process.env, RK_HARNESS_OUT: build }, stdio: "pipe" });
    host = await startHost(build, join(work, "settings.json"));
    const s = await launch(chrome!);
    page = s.page;
    close = s.close;
  }, 120_000);

  afterAll(async () => {
    await close?.();
    host?.close();
    for (const d of [build, work]) if (d) rmSync(d, { recursive: true, force: true });
  });

  it("renders standard and app sections grouped and ordered, inside the app-shell frame", async () => {
    await open();
    const groups = await page.eval<string[]>("[...document.querySelectorAll('.rk-settings__group')].map(e => e.textContent)");
    expect(groups).toEqual(["App", "General", "Account", "Support"]);
    const items = await page.eval<string[]>("[...document.querySelectorAll('.rk-settings__item')].map(e => e.textContent)");
    expect(items).toEqual(["Dictation", "Appearance", "Updates", "Speech models", "Licence", "Legal", "API keys", "Diagnostics", "About"]);
    expect(await exists(".rk-shell .rk-plane .rk-settings")).toBe(true);
    await shot("settings-appearance-light");
    await open("", "dark");
    await shot("settings-appearance-dark");
  });

  it("persists the theme choice through the host's settings file and restores it after reload with no other storage", async () => {
    await open();
    await section("appearance");
    await page.click(".rk-seg__item:nth-child(3)");
    await page.waitFor("document.documentElement.getAttribute('data-theme') === 'dark'");
    await page.waitFor("document.querySelector('.rk-settings__saving')?.textContent === ''");
    expect(settingsOnDisk().values["appearance.theme"]).toBe("dark");
    await open("section=appearance");
    await page.waitFor("document.documentElement.getAttribute('data-theme') === 'dark'");
    await shot("settings-theme-restored-dark");
    // Back to system so later tests start from the OS scheme.
    await page.click(".rk-seg__item:nth-child(1)");
    await page.waitFor("!document.documentElement.hasAttribute('data-theme')");
    await page.waitFor("document.querySelector('.rk-settings__saving')?.textContent === ''");
    await waitHost(() => settingsOnDisk().values["appearance.theme"] === "system");
  });

  it("applies an accent preset on top of the brand and saves only its id", async () => {
    await open("section=appearance");
    const before = await page.eval<string>("getComputedStyle(document.documentElement).getPropertyValue('--accent').trim()");
    await page.click(".rk-swatch[aria-label='Teal']");
    await page.waitFor("getComputedStyle(document.documentElement).getPropertyValue('--rk-brand').trim().toUpperCase() === '#0C8A7B'");
    await waitHost(() => settingsOnDisk().values["appearance.accent"] === "teal");
    expect(settingsOnDisk().values["appearance.accent"]).toBe("teal");
    expect(before.length).toBeGreaterThan(0);
    await page.click(".rk-swatch[aria-label='Brand']");
    await page.waitFor(`document.querySelector('[data-setting-id="appearance.accent"] .rk-swatch.is-on')?.getAttribute('aria-label') === 'Brand'`);
    await page.waitFor("document.querySelector('.rk-settings__saving')?.textContent === ''");
    await waitHost(() => !("appearance.accent" in settingsOnDisk().values));
    expect("appearance.accent" in settingsOnDisk().values).toBe(false);
  });

  it("rolls back a failed write and shows the host's error", async () => {
    await open("section=dictation");
    host.failNextSettingsPatch = true;
    await page.eval("document.querySelector('[data-setting-id=\\\"dictation.cleanup\\\"] .rk-toggle')?.focus()");
    await page.press("Enter");
    await waitHost(() => host.wire.some((w) => w.method === "rightkit.settings.patch" && w.body.includes("dictation.cleanup")));
    await page.waitFor("document.querySelector('.rk-settings__error')");
    expect(await text(".rk-settings__error")).toContain("disk full");
    expect(await page.eval<string>("document.querySelector('[data-setting-id=\"dictation.cleanup\"] .rk-toggle').getAttribute('aria-checked')")).toBe("true");
    expect("dictation.cleanup" in settingsOnDisk().values).toBe(false);
    await clickText("Dismiss");
    await page.eval("document.querySelector('[data-setting-id=\\\"dictation.cleanup\\\"] .rk-toggle')?.focus()");
    await page.press("Enter");
    await page.waitFor("document.querySelector('[data-setting-id=\"dictation.cleanup\"] .rk-toggle').getAttribute('aria-checked') === 'false'");
    await page.waitFor("document.querySelector('.rk-settings__saving')?.textContent === ''");
    expect(settingsOnDisk().values["dictation.cleanup"]).toBe(false);
  });

  it("searches sections and rows, and reveals the matching row after selecting a hit", async () => {
    await open();
    await page.click(".rk-settings__search");
    await page.type("punctuation");
    await page.waitFor("document.querySelectorAll('.rk-settings__item').length === 1");
    expect(await text(".rk-settings__item")).toBe("Dictation");
    expect(await text(".rk-settings__hit")).toBe("Clean up transcripts");
    await page.click(".rk-settings__hit");
    await page.waitFor("document.querySelector('[data-setting-id=\"dictation.cleanup\"].is-revealed')");
    await page.eval("document.querySelector('.rk-settings__search').select()");
    await page.type("zzzz");
    await page.waitFor("document.querySelector('.rk-settings__none')");
    expect(await text(".rk-settings__none")).toContain("zzzz");
  });

  it("moves through the nav with arrow keys", async () => {
    await open();
    await page.eval("document.querySelector('[data-section-id=\"dictation\"]').focus()");
    await page.press("ArrowDown");
    expect(await page.eval<string>("document.activeElement.dataset.sectionId")).toBe("appearance");
    await page.press("ArrowUp");
    await page.press("ArrowUp");
    expect(await page.eval<string>("document.activeElement.dataset.sectionId")).toBe("about");
  });

  it("stores an API key only in the credential store: value never reaches settings, DOM or later responses", async () => {
    const secret = "sk-test-NOT-A-REAL-KEY-12345";
    await open("section=api-keys");
    await page.waitFor("document.querySelector('[data-setting-id=\"apikeys.openai\"] input[aria-label=\"Provider API key value\"]')");
    await page.click("input[aria-label='Provider API key value']");
    await page.type(secret);
    await shot("settings-apikeys-typing-light");
    await clickText("Save", "document.querySelector('[data-setting-id=\"apikeys.openai\"]')");
    await page.waitFor("document.querySelector('[data-setting-id=\"apikeys.openai\"] .rk-badge--ok')");
    expect(host.secrets.get("acme/provider-key")).toBe(secret);
    expect(settingsOnDisk().values["secrets.openai"]).toEqual({ service: "acme", account: "provider-key" });
    expect(readFileSync(host.settingsFile, "utf8")).not.toContain(secret);
    expect(await page.eval<boolean>(`document.documentElement.outerHTML.includes(${JSON.stringify(secret)})`)).toBe(false);
    expect(await page.eval<boolean>(`[...document.querySelectorAll('input')].every(i => i.value !== ${JSON.stringify(secret)})`)).toBe(true);
    const carriers = host.wire.filter((w) => w.body.includes(secret)).map((w) => w.method);
    expect(carriers).toEqual(["rightkit.secrets.set"]);
    // Reload: presence comes back from the store, never the value.
    await open("section=api-keys");
    await page.waitFor("document.querySelector('[data-setting-id=\"apikeys.openai\"] .rk-badge--ok')");
    expect(await text("[data-setting-id='apikeys.relay'] .rk-sr__desc")).toBe("Not set.");
    await shot("settings-apikeys-light");
    await clickText("Remove", "document.querySelector('[data-setting-id=\"apikeys.openai\"]')");
    await clickText("Confirm", "document.querySelector('[data-setting-id=\"apikeys.openai\"]')");
    await page.waitFor("!document.querySelector('[data-setting-id=\"apikeys.openai\"] .rk-badge--ok')");
    expect(host.secrets.has("acme/provider-key")).toBe(false);
    expect("secrets.openai" in settingsOnDisk().values).toBe(false);
  });

  it("activates a licence against the license host, rejects a bad key, and deactivates with confirmation", async () => {
    await open("section=license");
    await page.waitFor("document.querySelector('[data-setting-id=\"license.activate\"] input[aria-label=\"Licence key\"]')");
    await page.click("input[aria-label='Licence key']");
    await page.type("WRONG-KEY");
    await clickText("Activate");
    try {
      await page.waitFor("document.querySelector('[data-setting-id=\"license.activate\"] .rk-sr__desc')?.textContent.includes('not recognised')");
    } catch (e) {
      const state = await page.eval<string>("JSON.stringify({active: document.activeElement?.outerHTML, body: document.querySelector('[data-setting-id=\\\"license.activate\\\"]')?.outerHTML, requests: performance.getEntriesByType('resource').map(x => x.name).filter(x => x.includes('/v1/licenses'))})");
      throw new Error(`${String(e)} state=${state}`);
    }
    await page.eval("document.querySelector('input[aria-label=\"Licence key\"]').select()");
    await page.type("GOOD-KEY-0001");
    await clickText("Activate");
    await page.waitFor("document.querySelector('[data-setting-id=\"license.status\"] .rk-badge--accent')");
    expect(await text("[data-setting-id='license.status']")).toContain("Pro");
    expect(await text("[data-setting-id='license.status']")).toContain("Lifetime");
    expect(host.licenseRequests.map((r) => r.path)).toEqual(["activate", "activate"]);
    await shot("settings-license-active-light");
    await clickText("Deactivate");
    await clickText("Confirm deactivate");
    await page.waitFor("document.querySelector('input[aria-label=\"Licence key\"]')");
    expect(host.licenseRequests.at(-1)?.path).toBe("deactivate");
  });

  it("checks and installs an update through the real auto-update client", async () => {
    await open("section=updates");
    expect(await text("[data-update-status]")).toContain("Not checked");
    await clickText("Check now");
    await page.waitFor("document.querySelector('[data-update-status=\"available\"]')");
    expect(await text("[data-update-status]")).toContain("1.5.0");
    await shot("settings-updates-light");
    await page.click("[data-setting-id='updates.auto'] .rk-toggle");
    await page.waitFor("document.querySelector('[data-setting-id=\"updates.auto\"] .rk-toggle').getAttribute('aria-checked') === 'true'");
    await clickText("Install update");
    await page.waitFor("['ready','downloading'].includes(document.querySelector('[data-update-status]')?.dataset.updateStatus)");
    await page.waitFor("document.querySelector('[data-update-status=\"ready\"]')");
    expect(await exists("[data-setting-id='updates.status'] .rk-btn--primary")).toBe(true);
    await page.waitFor("document.querySelector('.rk-settings__saving')?.textContent === ''");
    expect(settingsOnDisk().values["updates.autoInstall"]).toBe(true);
  });

  it("downloads a model with live progress, then removes it; blocks a model that does not fit", async () => {
    await open("section=models");
    await page.waitFor("document.querySelector('[data-setting-id=\"models.asr-large\"]')");
    expect(await text("[data-setting-id='models.asr-small']")).toContain("In use");
    expect(await text("[data-setting-id='models.asr-small']")).toContain("Tested on this hardware");
    expect(await page.eval<boolean>("document.querySelector('[data-setting-id=\"models.llm-huge\"] button').disabled")).toBe(true);
    await shot("settings-models-light");
    await clickText("Download", "document.querySelector('[data-setting-id=\"models.asr-large\"]')");
    await page.waitFor("document.querySelector('[data-setting-id=\"models.asr-large\"] [role=progressbar]')");
    await page.waitFor("!document.querySelector('[data-setting-id=\"models.asr-large\"] [role=progressbar]') && document.querySelector('[data-setting-id=\"models.asr-large\"]').textContent.includes('Remove')", 8000);
    await clickText("Remove", "document.querySelector('[data-setting-id=\"models.asr-large\"]')");
    await clickText("Confirm remove", "document.querySelector('[data-setting-id=\"models.asr-large\"]')");
    await page.waitFor("document.querySelector('[data-setting-id=\"models.asr-large\"]').textContent.includes('Download')");
  });

  it("collects a content-free diagnostics report and clears logs", async () => {
    await open("section=diagnostics");
    await page.waitFor("document.querySelector('[data-setting-id=\"diagnostics.collect\"]')");
    await clickText("Collect");
    await page.waitFor("document.querySelector('.rk-diag')");
    const report = await text(".rk-diag");
    expect(report).toContain("settings_opened");
    expect(report).not.toContain("never on disk");
    expect(JSON.stringify(host.logs)).not.toContain("never on disk");
    await shot("settings-diagnostics-light");
    await clickText("Clear");
    await clickText("Confirm");
    await page.waitFor("document.querySelector('[data-setting-id=\"diagnostics.collect\"]').textContent.includes('Logs cleared')");
    expect(host.logs).toHaveLength(0);
  });

  it("lets an app hide a standard section and shows About with copyable version info", async () => {
    await open("hide=legal,diagnostics");
    const items = await page.eval<string[]>("[...document.querySelectorAll('.rk-settings__item')].map(e => e.textContent)");
    expect(items).not.toContain("Legal");
    expect(items).not.toContain("Diagnostics");
    await section("about");
    expect(await text("[data-setting-id='about.version']")).toContain("Acme 1.4.2 · build 8841 · macOS");
    await open("section=legal");
    expect(await text(".rk-settings__section")).toContain("Terms of Service");
    expect(await text(".rk-settings__section")).toMatch(/(?:1 September|September 1), 2026/);
    await shot("settings-legal-light");
  });

  it("is usable at phone width and in dark mode", async () => {
    await page.setColorScheme("dark");
    await page.setViewport(520, 800);
    await page.goto(`${host.origin}/index.html?section=models`);
    await page.waitFor("document.querySelector('.rk-settings__section')");
    await page.waitFor("document.querySelector('[data-setting-id=\"models.asr-large\"]')");
    expect(await page.eval<boolean>("document.documentElement.scrollWidth <= window.innerWidth + 1")).toBe(true);
    await shot("settings-models-narrow-dark");
    await open("section=models", "dark");
    await shot("settings-models-dark");
  });

  it("left no files outside the temp work dir", () => {
    expect(existsSync(host.settingsFile)).toBe(true);
    expect(existsSync(`${host.settingsFile}.tmp`)).toBe(false);
    writeFileSync(join(work, "done"), "");
  });
});
