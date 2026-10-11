// Pins 0.5.1 default output in a real browser: DOM of the portal components that cannot render on the
// server (context menu, toast rows), plus a hash of every element's computed style in each overlay state.
// A new option or a stylesheet restructure must leave these snapshots untouched.
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

// Computed properties the host decides, not the package. Each one resolves against an element's box, a font or the
// scrollbar, so the same CSS computes other values on another machine. Their values are not pinned. Listed here:
// width, height, min/max sizes, logical sizes, insets, gaps, grid tracks, flex-basis, the transform family and its
// origins, background and object positions and sizes, and outline offset. Spacing (margin, padding) and typography stay
// pinned, since they are declared in absolute units.
const HOST_DECIDED = /^(?:(?:min-|max-)?(?:width|height|inline-size|block-size)|top|right|bottom|left|inset(?:-(?:block|inline)(?:-(?:start|end))?)?|(?:row-|column-)?gap|grid-(?:template|auto)-(?:columns|rows|areas)|flex-basis|transform|transform-origin|translate|scale|rotate|perspective-origin|background-position(?:-[xy])?|background-size|object-position|outline-offset)$/;

/**
 * Hash of the computed style of every element under body, keyed by structural path.
 *
 * Pinned: the computed value of each property that the page's own stylesheets or inline styles declare, read from the
 * CSSOM, minus HOST_DECIDED. The set therefore follows the CSS and not the browser build.
 * Not pinned: the browser's full property list (it changes with the Chrome build); element boxes (x, y, width, height),
 * which move with fonts, the scrollbar (the launch flags keep scrollbars visible) and the device pixel ratio; and the
 * HOST_DECIDED properties. The toast stack's position is pinned by the "toast stack centring" tests below. Transition
 * and animation values are skipped because they are timing.
 */
const styleHashes = () =>
  page.eval<Record<string, string>>(`(() => {
    const hash = (s) => { let h = 2166136261; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
    const host = new RegExp(${JSON.stringify(HOST_DECIDED.source)});
    const declared = new Set();
    const collect = (rules) => { for (const rule of rules) { if (rule.style) for (let i = 0; i < rule.style.length; i++) declared.add(rule.style[i]); if (rule.cssRules) collect(rule.cssRules); } };
    for (const sheet of document.styleSheets) { try { collect(sheet.cssRules); } catch { /* not a same-origin sheet */ } }
    for (const el of document.querySelectorAll('*')) for (let i = 0; i < el.style.length; i++) declared.add(el.style[i]);
    const out = {};
    const walk = (el, path) => {
      if (el.id === 'log' || el.tagName === 'SCRIPT') return;
      const cs = getComputedStyle(el);
      const props = [];
      for (let i = 0; i < cs.length; i++) { const p = cs[i]; if (!declared.has(p) || host.test(p) || p === 'transition' || p.startsWith('animation')) continue; props.push(p + ':' + cs.getPropertyValue(p)); }
      const key = path + '/' + el.tagName.toLowerCase() + (el.className && typeof el.className === 'string' ? '.' + el.className.split(/\\s+/).join('.') : '');
      out[key] = hash(props.sort().join(';'));
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

  describe("toast stack centring (0.6.1)", () => {
    const rectOf = () => page.eval<{ left: number; width: number; top: number }>(`(() => { const r = document.querySelector('[data-rk-toasts]').getBoundingClientRect(); return { left: r.left, width: r.width, top: r.top }; })()`);
    const withToast = async () => {
      await open("p=macos&t=light");
      await page.click("#b-ok");
      await page.waitFor("document.querySelector('.rk-toast--success')");
      await page.sleep(400);
    };
    const inject = (css: string) => page.eval(`(() => { const s = document.createElement('style'); s.id = 'probe'; s.textContent = ${JSON.stringify(css)}; document.head.appendChild(s); })()`);
    const clearProbe = () => page.eval("document.getElementById('probe')?.remove()");

    it("sits in the horizontal centre of the viewport with no transform", async () => {
      await withToast();
      const r = await rectOf();
      expect(Math.abs(r.left + r.width / 2 - 550)).toBeLessThan(0.5);
      expect(await page.eval<string>("getComputedStyle(document.querySelector('[data-rk-toasts]')).transform")).toBe("none");
    });

    it("renders at exactly the 0.5.1 position (transform: translateX(-50%))", async () => {
      await withToast();
      const now = await rectOf();
      await inject("[data-rk-toasts]{translate:none !important;transform:translateX(-50%) !important}");
      const legacy = await rectOf();
      await clearProbe();
      expect(now).toEqual(legacy);
    });

    it("a host `-translate-x-1/2` utility (the `translate` property) no longer stacks with the centring", async () => {
      await withToast();
      const base = await rectOf();
      // Tailwind 4 emits `translate: var(--tw-translate-x) var(--tw-translate-y)` in @layer utilities.
      await inject("@layer utilities { .tw-center { --tw-translate-x: -50%; --tw-translate-y: 0; translate: var(--tw-translate-x) var(--tw-translate-y); } }");
      await page.eval("document.querySelector('[data-rk-toasts]').classList.add('tw-center')");
      expect(await rectOf()).toEqual(base);
      // The 0.5.1 transform centring plus the same utility moved the stack by half its width (the defect).
      await inject("[data-rk-toasts]{transform:translateX(-50%) !important}");
      expect((await rectOf()).left).toBeCloseTo(base.left - base.width / 2, 0);
      await clearProbe();
    });

    it("a host can move the centring with --rk-toast-translate", async () => {
      await withToast();
      const base = await rectOf();
      await page.eval("document.querySelector('[data-rk-toasts]').style.setProperty('--rk-toast-translate', '0 0')");
      expect((await rectOf()).left).toBeCloseTo(base.left + base.width / 2, 0);
      await page.eval("document.querySelector('[data-rk-toasts]').style.setProperty('--rk-toast-translate', 'none')");
      expect((await rectOf()).left).toBeCloseTo(base.left + base.width / 2, 0);
    });
  });
});
