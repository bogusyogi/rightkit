// Pins 0.5.1 default output in a real browser: DOM of the portal components that cannot render on the
// server (context menu, toast rows), plus a hash of every element's computed style and box in each
// overlay state. A new option or a stylesheet restructure must leave these snapshots untouched.
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
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

const open = async (query: string) => {
  await page.setViewport(1100, 720);
  await page.goto(`${origin}/index.html?${query}`);
  await page.waitFor("window.__rkReady === true");
};

/** Hash of computed style + rounded box for every element under body, keyed by structural path. */
const styleHashes = () =>
  page.eval<Record<string, string>>(`(() => {
    const hash = (s) => { let h = 2166136261; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
    const out = {};
    const walk = (el, path) => {
      if (el.id === 'log' || el.tagName === 'SCRIPT') return;
      const cs = getComputedStyle(el);
      const props = [];
      for (let i = 0; i < cs.length; i++) { const p = cs[i]; if (p === 'transition' || p.startsWith('animation')) continue; props.push(p + ':' + cs.getPropertyValue(p)); }
      let s = props.sort().join(';');
      const r = el.getBoundingClientRect();
      s += [r.x, r.y, r.width, r.height].map((n) => Math.round(n * 10) / 10).join(',');
      const key = path + '/' + el.tagName.toLowerCase() + (el.className && typeof el.className === 'string' ? '.' + el.className.split(/\\s+/).join('.') : '');
      out[key] = hash(s);
      let i = 0;
      for (const c of el.children) walk(c, key + '[' + (i++) + ']');
    };
    for (const c of document.body.children) walk(c, 'body');
    return out;
  })()`);

const outerOf = (selector: string) =>
  page.eval<string>(`(() => [...document.querySelectorAll(${JSON.stringify(selector)})].map((e) => e.outerHTML).join('\\n').replace(/rk-menu-[a-z0-9]{6}/g, 'rk-menu-X'))()`);

describe.skipIf(!chrome)("default output is pinned in a real browser", () => {
  beforeAll(async () => {
    out = mkdtempSync(join(tmpdir(), "rk-app-shell-default-"));
    execFileSync(process.execPath, [join(pkgRoot, "node_modules/vite/bin/vite.js"), "build", "--config", join(here, "harness/vite.config.mjs"), "--logLevel", "warn"], {
      cwd: pkgRoot,
      env: { ...process.env, RK_HARNESS_OUT: out },
      stdio: "pipe",
    });
    ({ server, origin } = await serveDir(out));
    const session = await launch(chrome!);
    page = session.page;
    close = session.close;
  }, 120_000);

  afterAll(async () => {
    await close?.();
    server?.close();
    if (out) rmSync(out, { recursive: true, force: true });
  });

  for (const theme of ["light", "dark"] as const) {
    it(`${theme}: computed style of the shell, palette, confirm, menu and toasts`, async () => {
      const states: Record<string, Record<string, string>> = {};
      await open(`p=macos&t=${theme}`);
      await page.sleep(300);
      states.shell = await styleHashes();

      await open(`p=macos&t=${theme}`);
      await page.press("k", ["meta"]);
      await page.waitFor("document.querySelector('.rk-palette')");
      await page.sleep(500);
      states.palette = await styleHashes();

      await open(`p=macos&t=${theme}`);
      await page.click("#b-confirm");
      await page.waitFor("document.querySelector('[role=alertdialog]')");
      await page.sleep(500);
      states.confirm = await styleHashes();

      await open(`p=macos&t=${theme}`);
      await page.click("#row-a", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.sleep(500);
      states.menu = await styleHashes();

      await open(`p=macos&t=${theme}`);
      await page.click("#b-ok");
      await page.click("#b-err");
      await page.waitFor("document.querySelector('.rk-toast--error')");
      await page.sleep(500);
      states.toasts = await styleHashes();

      if (process.env.RK_DUMP) writeFileSync(`${process.env.RK_DUMP}.${theme}.json`, JSON.stringify(states, null, 1));
      expect(states).toMatchSnapshot();
    });
  }

  it("context menu and toast rows render the same DOM", async () => {
    await open("p=macos&t=light");
    await page.click("#row-a", "right");
    await page.waitFor("document.querySelector('[role=menu]')");
    const menu = await outerOf(".rk-menu-scrim, .rk-menu");
    await page.press("Escape");
    await page.click("#b-ok");
    await page.click("#b-err");
    await page.waitFor("document.querySelector('.rk-toast--error')");
    const toasts = await outerOf("[data-rk-toasts]");
    expect({ menu, toasts }).toMatchSnapshot();
  });

  const spyDelays = () =>
    page.eval(`(() => { window.__delays = []; const st = window.setTimeout; window.setTimeout = function (fn, ms, ...a) { window.__delays.push(ms); return st.call(window, fn, ms, ...a); }; })()`);

  it("default timing: palette runs the command 90 ms after Enter", async () => {
    await open("p=macos&t=light");
    await spyDelays();
    await page.press("k", ["meta"]);
    await page.waitFor("document.querySelector('.rk-palette')");
    await page.type("hello");
    await page.press("Enter");
    await page.waitFor("window.__delays.includes(90)");
    await page.waitFor("document.getElementById('log').textContent.includes('command:hello')");
  });

  it("default timing: a dismissed toast leaves the DOM 150 ms later", async () => {
    await open("p=macos&t=light");
    await spyDelays();
    await page.click("#b-ok");
    await page.waitFor("document.querySelector('.rk-toast--success')");
    await page.click(".rk-toast--success .rk-toast__x");
    await page.waitFor("window.__delays.includes(150)");
    await page.waitFor("!document.querySelector('.rk-toast--success')");
  });
});
