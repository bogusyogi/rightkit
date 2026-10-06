import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import type { Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { findChrome, launch, serveDir, type Page } from "./browser.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = join(here, "..");
const chrome = findChrome();
const shotsDir = process.env.RK_SHOTS_DIR ?? mkdtempSync(join(tmpdir(), "rk-onboarding-shots-"));

describe.skipIf(!chrome)("onboarding in real headless Chrome (muted)", () => {
  let page: Page;
  let close: (() => Promise<void>) | undefined;
  let server: Server | undefined;
  let origin = "";
  let site = "";

  beforeAll(async () => {
    const build = spawnSync(process.execPath, [join(pkg, "node_modules", "vite", "bin", "vite.js"), "build", "--config", join(pkg, "e2e", "harness", "vite.config.mjs")], { cwd: pkg, encoding: "utf8", timeout: 60_000 });
    if (build.status !== 0) throw new Error(`onboarding harness build failed\n${build.stdout}\n${build.stderr}`);
    site = join(pkg, "e2e", "dist");
    mkdirSync(shotsDir, { recursive: true });
    ({ server, origin } = await serveDir(site));
    const browser = await launch(chrome!);
    page = browser.page;
    close = browser.close;
    await page.setViewport(1100, 760);
  }, 90_000);

  afterAll(async () => {
    await close?.();
    server?.close();
    if (site.endsWith("/e2e/dist")) rmSync(site, { recursive: true, force: true });
  });

  it("keeps legal first, persists acceptance, then exposes ordered steps", async () => {
    await page.setColorScheme("light");
    await page.goto(`${origin}/`);
    // Settings load asynchronously; the legal gate renders once they resolve.
    await page.waitFor("document.querySelector('[data-testid=onboarding-legal]')", 5000);
    expect(await page.eval<string>("document.querySelector('[data-testid=onboarding-legal]')?.textContent ?? ''")).toContain("Agree & continue");
    await page.screenshot(join(shotsDir, "onboarding-legal-light.png"));
    await page.click("input[type=radio]");
    await page.click("input[type=checkbox]");
    await page.click("button[type=submit]");
    await page.waitFor("document.querySelector('[data-testid=onboarding][data-step-id=permissions]')");
    expect(await page.eval<string[]>("[...document.querySelectorAll('[data-testid=onboarding] .rk-onboarding__step')].map((e) => e.textContent || '')")).toEqual(["✓Agreement", "2Permissions", "3Models", "4Licence"]);
    await page.screenshot(join(shotsDir, "onboarding-permissions-light.png"));
  });

  it("renders dark mode & resumes a skipped screen without widening the viewport", async () => {
    await page.setColorScheme("dark");
    await page.click("footer button:first-child");
    await page.waitFor("window.__skipped === true", 3000);
    await page.goto(`${origin}/`);
    await page.waitFor("document.querySelector('[data-testid=onboarding]')");
    expect(await page.eval<boolean>("document.documentElement.scrollWidth <= window.innerWidth + 1")).toBe(true);
    await page.screenshot(join(shotsDir, "onboarding-resume-dark.png"));
  });

  it("does not report completion or skip when persistence fails, and shows the failure", async () => {
    await page.setColorScheme("light");
    await page.goto(`${origin}/?seed=last&failsave=1`);
    await page.waitFor("document.querySelector('[data-testid=onboarding][data-step-id]')");
    expect(await page.eval<string>("document.querySelector('[data-testid=onboarding]').dataset.stepId")).toBe("license");
    await page.click("footer button:last-child");
    await page.waitFor("document.querySelector('[role=alert]')", 3000);
    expect(await page.eval<string>("document.querySelector('[role=alert]').textContent")).toContain("disk full");
    await page.sleep(300);
    expect(await page.eval<boolean>("window.__done === true")).toBe(false);
    await page.screenshot(join(shotsDir, "onboarding-save-failed-light.png"));
    await page.click("footer button:first-child");
    await page.sleep(500);
    expect(await page.eval<boolean>("window.__skipped === true")).toBe(false);
    expect(await page.eval<string>("document.querySelector('[role=alert]')?.textContent ?? ''")).toContain("disk full");
    // Persisted state is unchanged: the final step is still current and nothing was written.
    expect(await page.eval<string>("JSON.parse(localStorage.getItem('rk-onboarding-settings')).values['onboarding.state'].skipped + ''")).toBe("false");
  });

  it("reports completion only after a successful save", async () => {
    await page.goto(`${origin}/?seed=last`);
    await page.waitFor("document.querySelector('[data-testid=onboarding]')");
    await page.click("footer button:last-child");
    await page.waitFor("window.__done === true", 3000);
    expect(await page.eval<boolean>("JSON.parse(localStorage.getItem('rk-onboarding-settings')).values['onboarding.state'].completed.includes('license')")).toBe(true);
  });

  it("enforces the agreement when a persisted step index is past zero but legal is not accepted", async () => {
    await page.goto(`${origin}/?seed=gate-step2`);
    await page.waitFor("document.querySelector('[data-testid=onboarding-legal]')", 3000);
    expect(await page.eval<boolean>("document.querySelector('[data-testid=onboarding]') === null")).toBe(true);
    expect(await page.eval<string>("document.querySelector('[data-testid=onboarding-legal]').textContent")).toContain("Agree & continue");
    await page.screenshot(join(shotsDir, "onboarding-legal-gate-step2-light.png"));
    // A failed acceptance save keeps the gate and shows the legal-gate error.
    await page.goto(`${origin}/?seed=gate-step2&failsave=1`);
    await page.waitFor("document.querySelector('[data-testid=onboarding-legal]')", 3000);
    await page.click("input[type=radio]");
    await page.click("input[type=checkbox]");
    await page.click("button[type=submit]");
    await page.waitFor("document.querySelector('[data-testid=onboarding-legal] [role=alert]')", 3000);
    expect(await page.eval<boolean>("document.querySelector('[data-testid=onboarding]') === null")).toBe(true);
    expect(await page.eval<boolean>("JSON.parse(localStorage.getItem('rk-onboarding-settings')).values['onboarding.state'].legalAccepted")).toBe(false);
  });
});
