// Real-browser check of the reduced-motion default and its explicit opt-out (data-rk-motion="full").
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import type { Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { findChrome, launch, serveDir, type Page } from "./browser.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const chrome = findChrome();

let page: Page;
let close: () => Promise<void>;
let server: Server;
let out: string;
let origin: string;


const animation = (selector: string) => page.eval<string>(`getComputedStyle(document.querySelector(${JSON.stringify(selector)})).animationName`);

describe.skipIf(!chrome)("reduced motion in real headless Chrome", () => {
  beforeAll(async () => {
    out = mkdtempSync(join(tmpdir(), "rk-app-shell-motion-"));
    execFileSync(process.execPath, [join(pkgRoot, "node_modules/vite/bin/vite.js"), "build", "--config", join(here, "harness/vite.config.mjs"), "--logLevel", "warn"], {
      cwd: pkgRoot,
      env: { ...process.env, RK_HARNESS_OUT: out },
      stdio: "pipe",
    });
    ({ server, origin } = await serveDir(out));
    const session = await launch(chrome!);
    page = session.page;
    close = session.close;
    await page.goto(`${origin}/variants.html?`);
    await page.eval(`(() => { const t = document.createElement("div"); t.id = "probe"; t.className = "rk-toast"; document.body.appendChild(t); })()`);
  }, 120_000);

  afterAll(async () => {
    await close?.();
    server?.close();
    if (out) rmSync(out, { recursive: true, force: true });
  });

  it("animates when the OS has no preference", async () => {
    await page.setReducedMotion("no-preference");
    expect(await animation("#probe")).toBe("rk-toast-in");
  });
  it("stops animations under prefers-reduced-motion by default", async () => {
    await page.setReducedMotion("reduce");
    expect(await animation("#probe")).toBe("none");
  });
  it("data-rk-motion=\"full\" on the root, an ancestor or the element opts out; any other value does not", async () => {
    await page.setReducedMotion("reduce");
    await page.eval(`document.documentElement.setAttribute("data-rk-motion", "full")`);
    expect(await animation("#probe")).toBe("rk-toast-in");
    await page.eval(`document.documentElement.setAttribute("data-rk-motion", "reduced")`);
    expect(await animation("#probe")).toBe("none");
    await page.eval(`document.documentElement.removeAttribute("data-rk-motion"); document.querySelector("#probe").setAttribute("data-rk-motion", "full")`);
    expect(await animation("#probe")).toBe("rk-toast-in");
    await page.eval(`document.querySelector("#probe").removeAttribute("data-rk-motion")`);
    expect(await animation("#probe")).toBe("none");
  });
});
