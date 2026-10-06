// Hidden-browser UI scenario: real Vite dev server + real headless Chrome, audio MUTED, Tauri IPC
// served by the in-page browser-QA mock (src/lib/host.ts). Skips cleanly when no Chrome is found.
// Never plays audio: --mute-audio is mandatory and --autoplay-policy keeps media from starting.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { scenario, assert, REPO } from "./lib.mjs";

const CHROME = [
  process.env.CHROME_PATH,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
].find((p) => p && existsSync(p));
const PORT = 5191;

async function waitForHttp(url, ms = 60_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { if ((await fetch(url)).ok) return; } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`dev server did not start at ${url}`);
}

scenario("renders shell and round-trips engine doctor through the IPC seam", async (t) => {
  if (!CHROME) return t.skip("no Chrome found (set CHROME_PATH)");
  const { default: puppeteer } = await import("puppeteer-core");
  const vite = spawn(process.execPath, ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(PORT), "--strictPort"], {
    cwd: REPO, stdio: "ignore",
  });
  let browser;
  try {
    await waitForHttp(`http://127.0.0.1:${PORT}/`);
    browser = await puppeteer.launch({
      executablePath: CHROME,
      headless: true,
      args: ["--mute-audio", "--autoplay-policy=user-gesture-required", "--no-first-run", "--no-default-browser-check"],
    });
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(String(e)));
    await page.goto(`http://127.0.0.1:${PORT}/?qa=1&qaPlatform=windows`);
    await page.waitForSelector('[data-testid="app-shell"]');
    assert.equal(await page.$eval('[data-testid="platform"]', (n) => n.textContent), "windows");
    await page.click("button.bg-brand-action");
    await page.waitForFunction(() => document.querySelector('[data-testid="doctor"]')?.textContent?.includes("ok"));
    assert.deepEqual(errors, []);
  } finally {
    await browser?.close();
    vite.kill("SIGTERM");
  }
});
