// Pins 0.5.1 default output in a real browser: DOM of the portal components that cannot render on the
// server (context menu, toast rows), plus a hash of every element's computed style in each overlay state.
// A new option or a stylesheet restructure must leave these snapshots untouched.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
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

// Token values as shell.css declares them. Light is the base :root block; dark adds the not-light and
// data-theme="dark" blocks on top, in the same order the stylesheet applies them.
const shellCss = readFileSync(join(pkgRoot, "shell.css"), "utf8");
const BASE_BLOCK = /:root\s*\{([^}]*)\}/g;
const NOT_LIGHT_BLOCK = /:root:not\(\[data-theme="light"\]\)\s*\{([^}]*)\}/g;
const DARK_BLOCK = /:root\[data-theme="dark"\]\s*\{([^}]*)\}/g;
const declaredIn = (re: RegExp): Array<[string, string]> =>
  [...shellCss.matchAll(re)].flatMap((m) => [...m[1].matchAll(/(--rk-[\w-]+)\s*:\s*([^;]+);/g)].map((d): [string, string] => [d[1], d[2].trim()]));
const tokensFor = (theme: "light" | "dark"): Record<string, string> => {
  const out: Record<string, string> = {};
  for (const re of theme === "light" ? [BASE_BLOCK] : [BASE_BLOCK, NOT_LIGHT_BLOCK, DARK_BLOCK]) {
    for (const [name, value] of declaredIn(re)) out[name] = value;
  }
  return out;
};

/** [selector, computed property, token the property must equal]. */
type Check = [selector: string, prop: string, token: string];
const SHELL_CHECKS: Check[] = [
  [".rk-shell", "background-color", "--rk-chrome"],
  [".rk-shell", "color", "--rk-ink"],
  [".rk-plane", "background-color", "--rk-plane"],
];
const PALETTE_CHECKS: Check[] = [[".rk-palette__field", "color", "--rk-ink-3"]];
const CONFIRM_CHECKS: Check[] = [["[role=alertdialog] .rk-btn--danger", "background-color", "--rk-bad"]];
const MENU_CHECKS: Check[] = [
  [".rk-menu", "background-color", "--rk-elevated"],
  [".rk-menu", "color", "--rk-ink"],
];
const TOAST_CHECKS: Check[] = [
  [".rk-toast--error .rk-toast__glyph", "color", "--rk-bad"],
  [".rk-toast", "background-color", "--rk-elevated"],
];

/**
 * Computed value of each checked property against the value the token resolves to. The expected value is
 * resolved in the page through a probe element, so both sides use the browser's own serialisation.
 * Returns only the rows that differ.
 */
const mismatches = (checks: Check[], tokens: Record<string, string>) =>
  page.eval<Array<{ selector: string; prop: string; expected: string; actual: string }>>(`(() => {
    const checks = ${JSON.stringify(checks.map(([selector, prop, token]) => ({ selector, prop, value: tokens[token] })))};
    const probe = document.createElement('span');
    document.body.appendChild(probe);
    const rows = [];
    for (const c of checks) {
      probe.style.cssText = 'position:absolute;visibility:hidden;' + c.prop + ':' + c.value;
      const expected = getComputedStyle(probe).getPropertyValue(c.prop);
      const el = document.querySelector(c.selector);
      const actual = el ? getComputedStyle(el).getPropertyValue(c.prop) : 'missing element';
      if (actual !== expected) rows.push({ selector: c.selector, prop: c.prop, expected, actual });
    }
    probe.remove();
    return rows;
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
      const tokens = tokensFor(theme);
      for (const [, , token] of [...SHELL_CHECKS, ...PALETTE_CHECKS, ...CONFIRM_CHECKS, ...MENU_CHECKS, ...TOAST_CHECKS]) {
        expect(tokens[token], token).toBeDefined();
      }
      const bad: unknown[] = [];

      await open(`p=macos&t=${theme}`);
      await page.sleep(300);
      bad.push(...(await mismatches(SHELL_CHECKS, tokens)));

      await open(`p=macos&t=${theme}`);
      await page.press("k", ["meta"]);
      await page.waitFor("document.querySelector('.rk-palette')");
      await page.sleep(500);
      bad.push(...(await mismatches(PALETTE_CHECKS, tokens)));

      await open(`p=macos&t=${theme}`);
      await page.click("#b-confirm");
      await page.waitFor("document.querySelector('[role=alertdialog]')");
      await page.sleep(500);
      bad.push(...(await mismatches(CONFIRM_CHECKS, tokens)));

      await open(`p=macos&t=${theme}`);
      await page.click("#row-a", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.sleep(500);
      bad.push(...(await mismatches(MENU_CHECKS, tokens)));

      await open(`p=macos&t=${theme}`);
      await page.click("#b-ok");
      await page.click("#b-err");
      await page.waitFor("document.querySelector('.rk-toast--error')");
      await page.sleep(500);
      bad.push(...(await mismatches(TOAST_CHECKS, tokens)));

      expect(bad).toEqual([]);
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
