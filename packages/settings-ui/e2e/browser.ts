/**
 * Minimal real-Chrome driver over the DevTools protocol (Node's built-in WebSocket): launches
 * headless Chrome muted, serves the built harness from a local static server, sends real input
 * events and captures screenshots. No Puppeteer/Playwright dependency.
 */
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { extname, join, normalize } from "node:path";

export const CHROME_CANDIDATES = [
  process.env.CHROME_PATH,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
].filter((p): p is string => Boolean(p));

export const findChrome = (): string | undefined => CHROME_CANDIDATES.find((p) => existsSync(p));

const MIME: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".json": "application/json" };

export function serveDir(dir: string): Promise<{ server: Server; origin: string }> {
  return new Promise((resolve) => {
    const server = createServer((req, res) => {
      const path = normalize(decodeURIComponent((req.url ?? "/").split("?")[0])).replace(/^(\.\.[/\\])+/, "");
      let file = join(dir, path === "/" || path === "." ? "index.html" : path);
      if (!existsSync(file) || statSync(file).isDirectory()) file = join(dir, "index.html");
      res.setHeader("content-type", MIME[extname(file)] ?? "application/octet-stream");
      res.end(readFileSync(file));
    });
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolve({ server, origin: `http://127.0.0.1:${typeof addr === "object" && addr ? addr.port : 0}` });
    });
  });
}

export interface Page {
  goto(url: string): Promise<void>;
  eval<T = unknown>(expression: string): Promise<T>;
  /** Real key press via Input.dispatchKeyEvent. `mods`: any of meta ctrl alt shift. */
  press(key: string, mods?: Array<"meta" | "ctrl" | "alt" | "shift">): Promise<void>;
  type(text: string): Promise<void>;
  click(selector: string, button?: "left" | "right"): Promise<void>;
  clickAt(x: number, y: number, button?: "left" | "right"): Promise<void>;
  rect(selector: string): Promise<{ x: number; y: number; width: number; height: number }>;
  setViewport(width: number, height: number): Promise<void>;
  setColorScheme(scheme: "light" | "dark"): Promise<void>;
  screenshot(path: string, clip?: { x: number; y: number; width: number; height: number; scale?: number }): Promise<void>;
  waitFor(expression: string, timeoutMs?: number): Promise<void>;
  sleep(ms: number): Promise<void>;
}

const MOD: Record<string, number> = { alt: 1, ctrl: 2, meta: 4, shift: 8 };
let navigationSerial = 0;
// DOM `code` and Windows virtual key for keys the app-shell binds.
const KEYS: Record<string, { code: string; vk: number; text?: string }> = {
  Enter: { code: "Enter", vk: 13, text: "\r" },
  Escape: { code: "Escape", vk: 27 },
  Tab: { code: "Tab", vk: 9 },
  ArrowDown: { code: "ArrowDown", vk: 40 },
  ArrowUp: { code: "ArrowUp", vk: 38 },
  Home: { code: "Home", vk: 36 },
  End: { code: "End", vk: 35 },
  "\\": { code: "Backslash", vk: 220, text: "\\" },
  "?": { code: "Slash", vk: 191, text: "?" },
  "/": { code: "Slash", vk: 191, text: "/" },
};
function keyInfo(key: string) {
  if (KEYS[key]) return KEYS[key];
  if (/^[a-z]$/i.test(key)) return { code: `Key${key.toUpperCase()}`, vk: key.toUpperCase().charCodeAt(0), text: key };
  if (/^[0-9]$/.test(key)) return { code: `Digit${key}`, vk: key.charCodeAt(0), text: key };
  return { code: key, vk: 0, text: key.length === 1 ? key : undefined };
}

export async function launch(chrome: string) {
  const profile = mkdtempSync(join(tmpdir(), "rk-app-shell-chrome-"));
  const proc: ChildProcess = spawn(
    chrome,
    ["--headless=new", "--mute-audio", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--hide-scrollbars=false", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows", "--disable-features=CalculateNativeWinOcclusion", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--window-size=1100,720", "about:blank"],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  const wsUrl = await new Promise<string>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("Chrome did not expose a DevTools endpoint")), 20000);
    let buf = "";
    proc.stderr!.on("data", (d) => {
      buf += String(d);
      const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    proc.on("exit", () => reject(new Error("Chrome exited early")));
  });
  const port = new URL(wsUrl).port;
  const target = (await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" })).json()) as { webSocketDebuggerUrl: string };
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise<void>((resolve, reject) => {
    ws.onopen = () => resolve();
    ws.onerror = () => reject(new Error("DevTools socket error"));
  });
  let id = 0;
  const pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  ws.onmessage = (event) => {
    const msg = JSON.parse(String(event.data));
    if (msg.id && pending.has(msg.id)) {
      const p = pending.get(msg.id)!;
      pending.delete(msg.id);
      msg.error ? p.reject(new Error(msg.error.message)) : p.resolve(msg.result);
    }
  };
  const send = <T = any>(method: string, params: object = {}) =>
    new Promise<T>((resolve, reject) => {
      const n = ++id;
      const timer = setTimeout(() => {
        pending.delete(n);
        reject(new Error(`DevTools call ${method} did not answer in 10s ${JSON.stringify(params).slice(0, 200)}`));
      }, 10000);
      pending.set(n, { resolve: (v) => (clearTimeout(timer), resolve(v)), reject: (e) => (clearTimeout(timer), reject(e)) });
      ws.send(JSON.stringify({ id: n, method, params }));
    });
  await send("Page.enable");
  await send("Runtime.enable");
  // A background/occluded headless tab neither focuses controls nor answers navigation reliably; pin it active.
  await send("Emulation.setFocusEmulationEnabled", { enabled: true });
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  const page: Page = {
    async goto(url) {
      // Force a unique URL so same-query reloads cannot satisfy waits from prior DOM.
      const target = new URL(url);
      target.searchParams.set("__rk_nav", String(++navigationSerial));
      await send("Page.navigate", { url: target.href });
      await page.waitFor(`location.href === ${JSON.stringify(target.href)}`, 15000);
      await page.waitFor("window.__ready === true", 15000);
    },
    async eval<T>(expression: string) {
      const r = await send<any>("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
      if (r.exceptionDetails) throw new Error(`eval failed: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
      return r.result.value as T;
    },
    async press(key, mods = []) {
      const info = keyInfo(key);
      const modifiers = mods.reduce((n, m) => n | MOD[m], 0);
      const common = { key, code: info.code, windowsVirtualKeyCode: info.vk, nativeVirtualKeyCode: info.vk, modifiers };
      const printable = info.text !== undefined && !(modifiers & (MOD.ctrl | MOD.meta));
      await send("Input.dispatchKeyEvent", { type: printable ? "keyDown" : "rawKeyDown", ...common, ...(printable ? { text: info.text } : {}) });
      await send("Input.dispatchKeyEvent", { type: "keyUp", ...common });
      await sleep(30);
    },
    async type(text) {
      await page.eval("(document.activeElement instanceof HTMLElement) && document.activeElement.focus()");
      for (const char of text) {
        const info = keyInfo(char);
        const common = { key: char, code: info.code, windowsVirtualKeyCode: info.vk, nativeVirtualKeyCode: info.vk, text: char };
        await send("Input.dispatchKeyEvent", { type: "keyDown", ...common });
        await send("Input.dispatchKeyEvent", { type: "keyUp", ...common });
      }
      await sleep(30);
    },
    async rect(selector) {
      const r = await page.eval<{ x: number; y: number; width: number; height: number } | null>(
        `(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`,
      );
      if (!r) throw new Error(`no element for ${selector}`);
      return r;
    },
    async clickAt(x, y, button = "left") {
      await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
      await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button, clickCount: 1, buttons: button === "left" ? 1 : 2 });
      await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button, clickCount: 1 });
      await sleep(60);
    },
    async click(selector, button = "left") {
      // Keep the target as the active document control before dispatching real mouse input.
      await send("Page.bringToFront");
      await page.eval(`document.querySelector(${JSON.stringify(selector)})?.focus()`);
      const r = await page.rect(selector);
      await page.clickAt(r.x + r.width / 2, r.y + r.height / 2, button);
    },
    async setViewport(width, height) {
      await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 2, mobile: false });
    },
    async setColorScheme(scheme) {
      await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: scheme }] });
    },
    async screenshot(path, clip) {
      const r = await send<{ data: string }>("Page.captureScreenshot", { format: "png", ...(clip ? { clip: { scale: 1, ...clip } } : {}) });
      writeFileSync(path, Buffer.from(r.data, "base64"));
    },
    async waitFor(expression, timeoutMs = 5000) {
      const start = Date.now();
      for (;;) {
        if (await page.eval<boolean>(`Boolean(${expression})`).catch(() => false)) return;
        if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for: ${expression}`);
        await sleep(40);
      }
    },
    sleep,
  };

  return {
    page,
    async close() {
      try {
        await send("Browser.close");
      } catch {
        /* already gone */
      }
      ws.close();
      // Wait for Chrome to exit before removing its profile: it keeps writing
      // into the directory until the process is gone.
      if (proc.exitCode === null && proc.signalCode === null) {
        const exited = new Promise<void>((resolve) => proc.once("exit", () => resolve()));
        proc.kill("SIGKILL");
        await exited;
      }
      rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    },
  };
}
