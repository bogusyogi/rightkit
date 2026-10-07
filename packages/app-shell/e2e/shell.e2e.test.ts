import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, statSync } from "node:fs";
import type { Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { findChrome, launch, serveDir, type Page } from "./browser.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const chrome = findChrome();
const shotsDir = process.env.RK_SHOTS_DIR ?? mkdtempSync(join(tmpdir(), "rk-app-shell-shots-"));

let page: Page;
let close: () => Promise<void>;
let server: Server;
let origin: string;
let out: string;

const log = () => page.eval<string>("document.getElementById('log').textContent");
const open = async (query: string, w = 1100, h = 720) => {
  await page.setViewport(w, h);
  await page.goto(`${origin}/index.html?${query}`);
};
const exists = (sel: string) => page.eval<boolean>(`!!document.querySelector(${JSON.stringify(sel)})`);
const focusedIn = (sel: string) => page.eval<boolean>(`!!document.activeElement && !!document.activeElement.closest(${JSON.stringify(sel)})`);

describe.skipIf(!chrome)("app-shell in real headless Chrome (muted)", () => {
  beforeAll(async () => {
    mkdirSync(shotsDir, { recursive: true });
    out = mkdtempSync(join(tmpdir(), "rk-app-shell-build-"));
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

  it("renders the fused default layout: titlebar over sidebar, 12px SVG joins on the active row, native-lights space on macOS", async () => {
    await open("p=macos&t=light");
    expect(await page.eval<string>("document.querySelector('.rk-item.is-active .rk-item__label').textContent")).toBe("Video");
    expect(await page.eval<number>("document.querySelectorAll('.rk-item.is-active svg[data-rk-fuse]').length")).toBe(2);
    expect(await exists(".rk-top__lights")).toBe(true);
    expect(await exists(".rk-caption")).toBe(false);
    // Fused: titlebar, sidebar and body share one chrome colour; the plane is a different surface.
    const colours = await page.eval<{ top: string; side: string; plane: string }>(
      `(() => { const c = (s) => getComputedStyle(document.querySelector(s)).backgroundColor; return { top: c('.rk-top'), side: c('.rk-side'), plane: c('.rk-plane') }; })()`,
    );
    expect(colours.top).toBe(colours.side);
    expect(colours.plane).not.toBe(colours.side);
    // The active row meets the plane edge exactly (it is what the curves bridge).
    const edges = await page.eval<{ row: number; plane: number }>(
      `(() => ({ row: document.querySelector('.rk-item.is-active').getBoundingClientRect().right, plane: document.querySelector('.rk-plane').getBoundingClientRect().left }))()`,
    );
    expect(Math.abs(edges.row - edges.plane)).toBeLessThanOrEqual(1);
    await page.screenshot(join(shotsDir, "default-macos-light.png"));
    await open("p=windows&t=light");
    expect(await exists(".rk-top__lights")).toBe(false);
    expect(await exists(".rk-caption__close")).toBe(true);
  });

  it("navigates by click and arrow keys, and keeps one roving tab stop", async () => {
    await open("p=macos&t=light");
    await page.click("[data-nav-id=jobs]");
    expect(await log()).toContain("nav:jobs");
    expect(await page.eval<string>("document.querySelector('.rk-top__title').textContent")).toBe("Jobs");
    await page.eval("document.querySelector('[data-nav-id=jobs]').focus()");
    await page.press("ArrowDown");
    expect(await page.eval<string>("document.activeElement.dataset.navId")).toBe("gallery");
    await page.press("Home");
    expect(await page.eval<string>("document.activeElement.dataset.navId")).toBe("home");
    expect(await page.eval<number>("document.querySelectorAll('.rk-item[tabindex=\"0\"]').length")).toBe(1);
  });

  it("opens the command palette with Cmd+K, filters, runs a command with Enter, and restores focus", async () => {
    await open("p=macos&t=light");
    await page.eval("document.querySelector('[data-nav-id=home]').focus()");
    await page.press("k", ["meta"]);
    await page.waitFor("document.querySelector('.rk-palette')");
    expect(await page.eval<string>("document.activeElement.getAttribute('role')")).toBe("combobox");
    await page.type("greet");
    await page.waitFor("document.querySelectorAll('[role=option]').length === 1");
    expect(await page.eval<string>("document.querySelector('[role=option]').dataset.command")).toBe("app.hello");
    expect(await page.eval<string>("document.querySelector('.rk-palette__group').textContent")).toBe("App");
    await page.screenshot(join(shotsDir, "palette-macos-light.png"));
    await page.press("Enter");
    await page.waitFor("!document.querySelector('.rk-palette')");
    await page.waitFor("document.getElementById('ran').dataset.count === '1'");
    expect(await log()).toContain("command:hello");
    // Nav destinations are palette commands too (default layout registers them).
    await page.press("k", ["meta"]);
    await page.waitFor("document.querySelector('.rk-palette')");
    await page.type("movie");
    await page.waitFor("document.querySelectorAll('[role=option]').length === 1");
    expect(await page.eval<string>("document.querySelector('[role=option]').dataset.command")).toBe("nav:video");
    await page.press("Escape");
    await page.waitFor("!document.querySelector('.rk-palette')");
    expect(await focusedIn(".rk-side")).toBe(true);
  });

  it("dispatches registry shortcuts: mod chords, sequences, bare keys only outside fields, and the help overlay lists them live", async () => {
    await open("p=macos&t=light");
    await page.waitFor("window.__rkReady === true");
    await page.press("s", ["meta"]);
    expect(await log()).toContain("shortcut:save");
    await page.press("g");
    await page.press("i");
    expect(await log()).toContain("shortcut:goto-inbox");
    await page.press("x");
    expect(await log()).toContain("shortcut:bare");
    const before = (await log()).split("shortcut:bare").length;
    await page.click("#typing");
    await page.press("x");
    await page.press("s", ["meta"]); // chords with a modifier still fire inside fields
    expect((await log()).split("shortcut:bare").length).toBe(before);
    expect((await log()).match(/shortcut:save/g)).toHaveLength(2);
    await page.eval("document.activeElement.blur()");
    await page.press("?");
    await page.waitFor("document.querySelector('.rk-help')");
    const rows = await page.eval<Record<string, string>>(
      "Object.fromEntries([...document.querySelectorAll('[data-shortcut]')].map((li) => [li.dataset.shortcut, li.querySelector('.rk-help__keys').textContent]))",
    );
    expect(rows["app.save"]).toBe("⌘S");
    expect(rows["app.goto-inbox"]).toContain("then");
    expect(rows["shell.palette"]).toBe("⌘K");
    await page.screenshot(join(shotsDir, "help-macos-light.png"));
    await page.press("Escape");
    await page.waitFor("!document.querySelector('.rk-help')");
  });

  it("uses Ctrl for `mod` on Windows", async () => {
    await open("p=windows&t=light");
    await page.press("s", ["meta"]);
    expect(await log()).not.toContain("shortcut:save");
    await page.press("s", ["ctrl"]);
    expect(await log()).toContain("shortcut:save");
    await page.press("k", ["ctrl"]);
    await page.waitFor("document.querySelector('.rk-palette')");
    expect(await page.eval<string>("document.querySelector('.rk-search__text, .rk-palette') ? 'ok' : ''")).toBe("ok");
    await page.press("Escape");
  });

  it("shows toasts as an aria-live stack: success/error roles, action, auto-dismiss, manual dismiss", async () => {
    await open("p=macos&t=light");
    await page.click("#b-ok");
    await page.waitFor("document.querySelector('.rk-toast--success')");
    await page.click("#b-err");
    await page.waitFor("document.querySelector('.rk-toast--error')");
    expect(await page.eval<string>("document.querySelector('.rk-toast--error').getAttribute('role')")).toBe("alert");
    expect(await page.eval<string>("document.querySelector('[data-rk-toasts]').getAttribute('aria-live')")).toBe("polite");
    await page.screenshot(join(shotsDir, "toasts-macos-light.png"));
    await page.click(".rk-toast--error .rk-toast__action");
    expect(await log()).toContain("toast:retry");
    await page.waitFor("!document.querySelector('.rk-toast--error')");
    await page.click(".rk-toast--success .rk-toast__x");
    await page.waitFor("!document.querySelector('.rk-toast--success')");
    await page.click("#b-short");
    await page.waitFor("document.querySelector('.rk-toast--info')");
    await page.waitFor("!document.querySelector('.rk-toast--info')", 3000); // ttl 400ms
  });

  it("confirm dialog: focus trap cycles, Escape resolves false, danger starts on Cancel, Delete resolves true, focus restored", async () => {
    await open("p=macos&t=light");
    await page.click("#b-confirm");
    await page.waitFor("document.querySelector('[role=alertdialog]')");
    expect(await page.eval<string>("document.activeElement.textContent")).toBe("Cancel");
    await page.press("Tab");
    expect(await page.eval<string>("document.activeElement.textContent")).toBe("Delete");
    await page.press("Tab");
    expect(await page.eval<string>("document.activeElement.textContent")).toBe("Cancel"); // wrapped, never reached the page behind
    await page.screenshot(join(shotsDir, "confirm-macos-light.png"));
    await page.press("Escape");
    await page.waitFor("!document.querySelector('[role=alertdialog]')");
    expect(await log()).toContain("confirm:false");
    expect(await page.eval<string>("document.activeElement.id")).toBe("b-confirm");
    await page.click("#b-confirm");
    await page.waitFor("document.querySelector('[role=alertdialog]')");
    await page.click("[role=alertdialog] .rk-btn--danger");
    await page.waitFor("!document.querySelector('[role=alertdialog]')");
    expect(await log()).toContain("confirm:true");
  });

  it("prompt dialog validates and resolves the typed value", async () => {
    await open("p=macos&t=light");
    await page.click("#b-prompt");
    await page.waitFor("document.querySelector('[role=dialog] input')");
    expect(await page.eval<string>("document.activeElement.value")).toBe("Untitled"); // autofocused
    await page.eval("document.querySelector('[role=dialog] input').select()");
    await page.type("   "); // replace the selection with blanks
    await page.press("Enter");
    await page.waitFor("document.querySelector('.rk-modal__error')");
    expect(await page.eval<string>("document.querySelector('.rk-modal__error').textContent")).toBe("Name required");
    await page.eval("document.querySelector('[role=dialog] input').select()");
    await page.type("Final cut");
    await page.press("Enter");
    await page.waitFor("!document.querySelector('[role=dialog]')");
    expect(await log()).toContain("prompt:Final cut");
  });

  it("app-owned context menu: opens at the pointer inside the viewport, arrows + Enter run the item, disabled is skipped, Escape closes", async () => {
    await open("p=macos&t=light");
    await page.click("#row-a", "right");
    await page.waitFor("document.querySelector('[role=menu]')");
    const labels = await page.eval<string[]>("[...document.querySelectorAll('[role^=menuitem]')].map((e) => e.textContent.replace(/\\s+/g, ' ').trim())");
    expect(labels[0]).toBe("Open alpha.mp4");
    expect(labels).toContain("Delete");
    expect(await page.eval<string>("document.querySelector('[role=menuitemcheckbox]').getAttribute('aria-checked')")).toBe("true");
    const box = await page.rect("[role=menu]");
    expect(box.x + box.width).toBeLessThanOrEqual(1100);
    expect(box.y + box.height).toBeLessThanOrEqual(720);
    await page.screenshot(join(shotsDir, "context-menu-macos-light.png"));
    await page.press("ArrowDown");
    await page.press("ArrowDown");
    await page.press("ArrowDown"); // Pinned -> Delete (Unavailable is disabled)
    await page.press("ArrowDown"); // wraps to Open
    await page.press("End");
    await page.press("Enter");
    await page.waitFor("!document.querySelector('[role=menu]')");
    expect(await log()).toContain("menu:delete");
    await page.click("#row-a", "right");
    await page.waitFor("document.querySelector('[role=menu]')");
    await page.press("Escape");
    await page.waitFor("!document.querySelector('[role=menu]')");
    await page.click("#row-a", "right");
    await page.waitFor("document.querySelector('[role=menu]')");
    await page.clickAt(5, 400);
    await page.waitFor("!document.querySelector('[role=menu]')");
  });

  it("theme: appearance menu switches data-theme and the plane token changes; dark follows the OS when set to system", async () => {
    await open("p=macos");
    const plane = () => page.eval<string>("getComputedStyle(document.querySelector('.rk-plane')).backgroundColor");
    await page.setColorScheme("light");
    const light = await plane();
    await page.click("[aria-label=Appearance]");
    await page.waitFor("document.querySelector('[role=menu]')");
    await page.eval("[...document.querySelectorAll('[role^=menuitem]')].find((e) => e.textContent.includes('Dark')).click()");
    await page.waitFor("document.documentElement.dataset.theme === 'dark'");
    const dark = await plane();
    expect(dark).not.toBe(light);
    await page.screenshot(join(shotsDir, "default-macos-dark.png"));
    await page.click("[aria-label=Appearance]");
    await page.waitFor("document.querySelector('[role=menu]')");
    expect(await page.eval<string>("document.querySelector('[role=menuitemcheckbox][aria-checked=true]').textContent")).toContain("Dark");
    await page.eval("[...document.querySelectorAll('[role^=menuitem]')].find((e) => e.textContent.includes('System')).click()");
    await page.waitFor("!document.documentElement.dataset.theme");
    await page.setColorScheme("dark");
    expect(await plane()).toBe(dark);
    await page.setColorScheme("light");
  });

  it("collapses the sidebar from the titlebar toggle and Cmd+\\, and goes to an overlay on narrow windows", async () => {
    await open("p=macos&t=light");
    expect(await page.eval<boolean>("!document.querySelector('.rk-side').hidden")).toBe(true);
    await page.click("[aria-label='Toggle sidebar']");
    await page.waitFor("document.querySelector('.rk-side').hidden");
    await page.press("\\", ["meta"]);
    await page.waitFor("!document.querySelector('.rk-side').hidden");
    await open("p=macos&t=light", 600, 700);
    expect(await page.eval<string>("getComputedStyle(document.querySelector('.rk-side')).position")).toBe("absolute");
    await page.screenshot(join(shotsDir, "narrow-macos-light.png"));
  });

  it("custom layout (MailRight-style, no AppShell): shared palette, shortcuts, toast and menu still work around app-owned chrome", async () => {
    await open("layout=custom&p=windows&t=light");
    expect(await exists(".rk-side")).toBe(false);
    expect(await exists(".rk-caption__close")).toBe(true);
    expect(await page.eval<string>("document.getElementById('custom-bar').textContent")).toBe("3 unread");
    await page.press("k", ["ctrl"]);
    await page.waitFor("document.querySelector('.rk-palette')");
    await page.type("hello");
    await page.press("Enter");
    await page.waitFor("document.getElementById('ran').dataset.count === '1'");
    await page.press("s", ["ctrl"]);
    expect(await log()).toContain("shortcut:save");
    await page.click("#b-ok");
    await page.waitFor("document.querySelector('.rk-toast--success')");
    await page.click("#mail-W", "right");
    await page.waitFor("document.querySelector('[role=menu]')");
    await page.press("Enter");
    expect(await log()).toContain("menu:archive");
    await page.screenshot(join(shotsDir, "custom-windows-light.png"));
    await open("layout=custom&p=windows&t=dark");
    await page.screenshot(join(shotsDir, "custom-windows-dark.png"));
  });

  it("captures the default layout on Windows in dark for review", async () => {
    await open("p=windows&t=dark");
    await page.click("#b-ok");
    await page.waitFor("document.querySelector('.rk-toast--success')");
    await page.screenshot(join(shotsDir, "default-windows-dark.png"));
    await page.press("k", ["ctrl"]);
    await page.waitFor("document.querySelector('.rk-palette')");
    await page.screenshot(join(shotsDir, "palette-windows-dark.png"));
    for (const f of ["default-windows-dark.png", "palette-windows-dark.png", "default-macos-light.png"]) expect(statSync(join(shotsDir, f)).size).toBeGreaterThan(5000);
  });
});

void dirname;
