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
});
