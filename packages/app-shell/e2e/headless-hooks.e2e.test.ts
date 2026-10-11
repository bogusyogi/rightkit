// Real-key, real-pointer checks of the @rightkit/platform-ui/headless hooks (0.2.2), configured the way
// ViewRight's locked markup needs them. The harness page (harness/hooks.tsx) renders its own markup and uses
// no app-shell component. platform-ui has no browser harness of its own, so its e2e lives here.
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

const open = async (query: string) => {
  await page.setViewport(1100, 720);
  // The previous page stays alive until the navigation commits: clear its ready flag so goto() waits for the new one.
  await page.eval("window.__ready = false").catch(() => undefined);
  await page.goto(`${origin}/hooks.html?${query}`);
  await page.eval("new Promise((r) => requestAnimationFrame(() => r(true)))");
};
const log = () => page.eval<string>("document.getElementById('log').textContent");
const logLines = async () => (await log()).split("\n").filter(Boolean);
const exists = (sel: string) => page.eval<boolean>(`Boolean(document.querySelector(${JSON.stringify(sel)}))`);
const activeId = () => page.eval<string>("document.activeElement?.id || document.activeElement?.getAttribute('role') || document.activeElement?.tagName");
const attr = (sel: string, name: string) => page.eval<string | null>(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(name)}) ?? null`);

describe.skipIf(!chrome)("platform-ui headless hooks in real headless Chrome", () => {
  beforeAll(async () => {
    out = mkdtempSync(join(tmpdir(), "rk-hooks-"));
    execFileSync(process.execPath, [join(pkgRoot, "node_modules/vite/bin/vite.js"), "build", "--config", join(here, "harness/vite.config.mjs"), "--logLevel", "warn"], {
      cwd: pkgRoot,
      env: { ...process.env, RK_HARNESS_OUT: out, RK_HARNESS_DEV: "1" },
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

  describe("menu family (useDismiss + useExitTransition + fitToViewport)", () => {
    const openMenu = async (query = "", at?: { x: number; y: number }) => {
      await open(`demo=menu&${query}`);
      if (at) await page.clickAt(at.x, at.y, "right");
      else await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
    };

    it("a press outside (on the backdrop) closes: mousedown mode, then the layer unmounts after the exit", async () => {
      await openMenu();
      await page.clickAt(900, 500);
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await logLines()).toEqual(["menu:exit-start", "menu:gone"]);
    });

    it("a press inside the panel does not close it (and mousedown default is prevented by the app's own markup)", async () => {
      await openMenu();
      await page.click("[role=menu]");
      expect(await exists("[role=menu]")).toBe(true);
    });

    it("right-clicking the backdrop closes it and suppresses the native menu: no second menu opens", async () => {
      await openMenu();
      await page.clickAt(900, 500, "right");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await exists("[role=menu]")).toBe(false);
    });

    it("Escape closes with the window capture listener and, configured like ViewRight, neither prevents nor stops it", async () => {
      await openMenu();
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=menu]')");
      const lines = await logLines();
      expect(lines).toContain("win-escape prevented=false");
      expect(lines.some((l) => l.startsWith("doc-escape"))).toBe(true); // not stopped
      expect(lines).toContain("menu:gone");
    });

    it("click-target mode: a click on the backdrop closes it; a drag from the panel to the sibling backdrop does not (the click lands on their common ancestor, as with a plain onClick on the scrim)", async () => {
      await openMenu("dismiss=click-target");
      const r = await page.rect("[role=menu] button");
      await page.drag(r.x + 20, r.y + 10, 900, 500);
      await page.sleep(300);
      expect(await exists("[role=menu]")).toBe(true);
      await page.clickAt(900, 500);
      await page.waitFor("!document.querySelector('[role=menu]')");
    });

    it("window blur and resize close only when asked", async () => {
      await openMenu();
      await page.eval("window.dispatchEvent(new Event('blur'))");
      await page.setViewport(1000, 700);
      await page.sleep(200);
      expect(await exists("[role=menu]")).toBe(true);
      await openMenu("blur=1");
      await page.eval("window.dispatchEvent(new Event('blur'))");
      await page.waitFor("!document.querySelector('[role=menu]')");
      await openMenu("resize=1");
      await page.setViewport(1000, 650);
      await page.sleep(300);
      await page.waitFor("!document.querySelector('[role=menu]')");
    });

    it("immediately: choosing an item starts the exit and runs the action in the same tick", async () => {
      await openMenu();
      await page.click("[role=menu] button:nth-child(2)");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await logLines()).toEqual(["menu:exit-start", "menu:run Copy", "menu:gone"]);
    });

    it("onActionWhileClosing: a second click during the exit is dropped by default and runs with run", async () => {
      await openMenu("exit=500");
      await page.click("[role=menu] button:nth-child(2)");
      await page.click("[role=menu] button:nth-child(2)");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect((await logLines()).filter((l) => l === "menu:run Copy")).toHaveLength(1);
      await openMenu("exit=500&whileClosing=run");
      await page.click("[role=menu] button:nth-child(2)");
      await page.click("[role=menu] button:nth-child(2)");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect((await logLines()).filter((l) => l === "menu:run Copy")).toHaveLength(2);
    });

    it("settled: the menu stays open, pending, until the action's promise settles; then it exits", async () => {
      await openMenu("after=settled");
      await page.click("[role=menu] button:nth-child(2)");
      await page.sleep(250);
      expect(await attr("[role=menu]", "data-pending")).toBe("true");
      expect(await attr("[role=menu]", "data-closing")).toBe("false");
      expect(await logLines()).toEqual(["menu:run Copy"]);
      await page.eval("window.__release()");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await logLines()).toEqual(["menu:run Copy", "menu:exit-start", "menu:gone"]);
    });

    it("re-opening in place during the exit is safe: the old timer cannot close the new instance", async () => {
      await openMenu("exit=400&inplace=1");
      await page.clickAt(900, 500); // starts the 400 ms exit
      await page.waitFor("document.querySelector('[role=menu]')?.dataset.closing === 'true'");
      await page.sleep(150);
      await page.eval("window.__exit.open()");
      await page.sleep(700);
      expect(await attr("[role=menu]", "data-closing")).toBe("false");
      expect(await logLines()).not.toContain("menu:gone");
      await page.clickAt(900, 500);
      await page.waitFor("!document.querySelector('[role=menu]')", 3000);
    });

    it("fitToViewport keeps a menu opened at the bottom-right corner inside the viewport", async () => {
      await openMenu("", { x: 1095, y: 715 });
      const r = await page.rect("[role=menu]");
      expect(r.x + r.width).toBeLessThanOrEqual(1100 - 8 + 0.5);
      expect(r.y + r.height).toBeLessThanOrEqual(720 - 8 + 0.5);
      expect(r.x).toBeGreaterThanOrEqual(8);
    });

    it("listeners are gone after the menu closes: Escape no longer reaches the hook", async () => {
      await openMenu();
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=menu]')");
      await page.eval("document.getElementById('log').textContent = ''");
      await page.press("Escape");
      expect(await logLines()).toEqual(["doc-escape prevented=false"]);
    });
  });

  describe("modal family (useFocusTrap + useDismiss + useExitTransition)", () => {
    const openModal = async (query = "") => {
      await open(`demo=modal&${query}`);
      await page.click("#opener");
      await page.waitFor("document.querySelector('[role=dialog]')");
      await page.sleep(60);
    };

    it("defaults (0.2.1): first control focused, Tab wraps both ways, Escape closes and restores focus to the opener", async () => {
      await openModal();
      expect(await activeId()).toBe("first");
      await page.press("Tab");
      await page.press("Tab");
      expect(await activeId()).toBe("last");
      await page.press("Tab");
      expect(await activeId()).toBe("first");
      await page.press("Tab", ["shift"]);
      expect(await activeId()).toBe("last");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      await page.sleep(50);
      expect(await activeId()).toBe("opener");
    });

    it("Escape defaults: prevented and stopped, so nothing beyond the window capture listener sees it", async () => {
      await openModal();
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      const lines = await logLines();
      expect(lines).toContain("win-escape prevented=true");
      expect(lines.some((l) => l.startsWith("doc-escape"))).toBe(false);
    });

    it("ViewRight's Escape (stopPropagation only): stopped, not prevented", async () => {
      await openModal("pd=0");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      const lines = await logLines();
      expect(lines).toContain("win-escape prevented=false");
      expect(lines.some((l) => l.startsWith("doc-escape"))).toBe(false);
    });

    it("neither prevented nor stopped: the page sees an untouched Escape", async () => {
      await openModal("pd=0&sp=0");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      const lines = await logLines();
      expect(lines).toContain("win-escape prevented=false");
      expect(lines.some((l) => l.startsWith("doc-escape"))).toBe(true);
    });

    it("preventDefault and stopPropagation can be set independently: prevented but not stopped", async () => {
      await openModal("sp=0");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      const lines = await logLines();
      expect(lines).toContain("win-escape prevented=true");
      expect(lines.some((l) => l.startsWith("doc-escape"))).toBe(true);
    });

    it("captureOpener render (default) vs effect, when React's own autoFocus already moved focus at commit", async () => {
      await openModal("raf=1");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      await page.sleep(50);
      expect(await activeId()).toBe("opener");
      await openModal("raf=1&capture=effect");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      await page.sleep(50);
      expect(await activeId()).not.toBe("opener"); // the opener captured was the dialog's own input, now gone
    });

    it("restore false leaves focus where it lands; restoreTiming <ms> restores late", async () => {
      await openModal("restore=none");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      await page.sleep(100);
      expect(await activeId()).not.toBe("opener");
      await openModal("restore=300");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      expect(await activeId()).not.toBe("opener");
      await page.waitFor("document.activeElement?.id === 'opener'", 2000);
    });

    it("trap false: Tab is native (focus leaves the dialog)", async () => {
      await openModal("trap=0");
      await page.press("Tab");
      await page.press("Tab");
      await page.press("Tab");
      expect(await activeId()).not.toBe("first");
      expect(await page.eval<boolean>("document.activeElement.closest('[role=dialog]') === null")).toBe(true);
    });

    it("an empty dialog: Tab is swallowed by default, native with emptyList pass; fallback none leaves focus on the opener", async () => {
      await openModal("empty=1");
      expect(await activeId()).toBe("dialog");
      await page.press("Tab");
      expect(await activeId()).toBe("dialog");
      await openModal("empty=1&emptyList=pass");
      await page.press("Tab");
      expect(await activeId()).not.toBe("dialog");
      await openModal("empty=1&fallback=none");
      expect(await activeId()).toBe("opener");
    });

    it("[hidden] controls are skipped by default", async () => {
      await openModal("hidden=1");
      expect(await activeId()).toBe("first");
      await openModal("hidden=1&skipHidden=0");
      expect(await activeId()).not.toBe("first");
    });

    it("outside press modes: mousedown closes at once; click needs press and release outside; click-target closes after a drag from inside", async () => {
      const inside = async () => {
        const r = await page.rect("[role=dialog]");
        return { x: r.x + 20, y: r.y + 20 };
      };
      await openModal("outside=mousedown");
      await page.clickAt(30, 30);
      await page.waitFor("!document.querySelector('[role=dialog]')");

      await openModal("outside=click");
      const p = await inside();
      await page.drag(p.x, p.y, 30, 30);
      await page.sleep(300);
      expect(await exists("[role=dialog]")).toBe(true);
      await page.drag(30, 30, p.x, p.y);
      await page.sleep(300);
      expect(await exists("[role=dialog]")).toBe(true);
      await page.clickAt(30, 30);
      await page.waitFor("!document.querySelector('[role=dialog]')");

      await openModal("outside=click-target");
      const q2 = await inside();
      await page.drag(q2.x, q2.y, 30, 30);
      await page.waitFor("!document.querySelector('[role=dialog]')");
    });

    it("an inner click never dismisses in any mode", async () => {
      for (const mode of ["mousedown", "click", "click-target"]) {
        await openModal(`outside=${mode}`);
        await page.click("#mid");
        await page.sleep(250);
        expect(await exists("[role=dialog]"), mode).toBe(true);
      }
    });
  });

  describe("palette family (useListNavigation + useExitTransition + useDismiss)", () => {
    const openPalette = async (query = "") => {
      await open(`demo=palette&${query}`);
      await page.click("#opener");
      await page.waitFor("document.querySelector('[role=dialog]')");
      await page.sleep(60);
    };
    const activeIdx = () => page.eval<number>("[...document.querySelectorAll('[role=option]')].findIndex((e) => e.getAttribute('aria-selected') === 'true')");
    const descendant = () => attr("#q", "aria-activedescendant");

    it("ids and roles are the app's: aria-controls and option ids come from the options, not from the hook", async () => {
      await openPalette();
      expect(await attr("#q", "role")).toBe("combobox");
      expect(await attr("#q", "aria-controls")).toBe("vr-palette-results");
      expect(await attr("[role=listbox]", "id")).toBe("vr-palette-results");
      expect(await descendant()).toBe("vr-palette-option-0");
      expect(await attr("[role=option]", "id")).toBe("vr-palette-option-0");
    });

    it("ViewRight profile: ArrowDown clamps at the last item, ArrowUp at the first, Home/End stay with the caret", async () => {
      await openPalette();
      await page.type("open");
      expect(await page.eval<number>("document.querySelectorAll('[role=option]').length")).toBe(3);
      for (let i = 0; i < 6; i++) await page.press("ArrowDown");
      expect(await activeIdx()).toBe(2);
      await page.press("ArrowUp");
      await page.press("ArrowUp");
      await page.press("ArrowUp");
      expect(await activeIdx()).toBe(0);
      await page.press("ArrowDown");
      await page.press("Home");
      expect(await activeIdx()).toBe(1);
      await page.press("End");
      expect(await activeIdx()).toBe(1);
    });

    it("ViewRight profile: the stored index survives a shrinking list (clamped on read) and comes back when the list grows", async () => {
      await openPalette();
      for (let i = 0; i < 10; i++) await page.press("ArrowDown");
      expect(await activeIdx()).toBe(10);
      await page.type("open");
      expect(await descendant()).toBe("vr-palette-option-2");
      await page.eval("(() => { const q = document.getElementById('q'); q.select(); })()");
      await page.press("Backspace");
      expect(await activeIdx()).toBe(10);
    });

    it("app profile: wraps, Home/End move the selection, and typing resets it to the first result", async () => {
      await openPalette("profile=app");
      await page.press("ArrowUp");
      expect(await activeIdx()).toBe(23);
      await page.press("ArrowDown");
      expect(await activeIdx()).toBe(0);
      await page.press("End");
      expect(await activeIdx()).toBe(23);
      await page.press("Home");
      expect(await activeIdx()).toBe(0);
      await page.press("ArrowDown");
      await page.press("ArrowDown");
      await page.type("c");
      expect(await activeIdx()).toBe(0);
    });

    it("hoverSelects always: every mouse move over an option selects it", async () => {
      await openPalette();
      const r = await page.rect("[role=option]:nth-child(4)");
      await page.moveTo(r.x + 10, r.y + 10);
      expect(await activeIdx()).toBe(3);
      await page.press("ArrowDown");
      expect(await activeIdx()).toBe(4);
      const r2 = await page.rect("[role=option]:nth-child(2)");
      await page.moveTo(r2.x + 10, r2.y + 10);
      expect(await activeIdx()).toBe(1);
    });

    it("hoverSelects after-pointer-move: a resting pointer does not take the keyboard selection back", async () => {
      await openPalette("profile=app");
      const r = await page.rect("[role=option]:nth-child(4)");
      await page.moveTo(r.x + 10, r.y + 10);
      expect(await activeIdx()).toBe(3);
      await page.press("ArrowDown");
      expect(await activeIdx()).toBe(4);
      await page.moveTo(r.x + 10, r.y + 10); // same coordinates: not a pointer move
      expect(await activeIdx()).toBe(4);
      await page.moveTo(r.x + 12, r.y + 10); // a real move
      expect(await activeIdx()).toBe(3);
    });

    it("emptyIndex: ArrowDown on an empty list then results arriving: clamp selects the first item, preserve selects nothing", async () => {
      await openPalette();
      await page.type("zzz");
      await page.press("ArrowDown");
      for (let i = 0; i < 3; i++) await page.press("Backspace");
      expect(await activeIdx()).toBe(0);

      await openPalette("emptyIndex=preserve");
      await page.type("zzz");
      await page.press("ArrowDown");
      for (let i = 0; i < 3; i++) await page.press("Backspace");
      expect(await activeIdx()).toBe(-1);
      expect(await descendant()).toBe(null);
      await page.press("ArrowDown");
      expect(await activeIdx()).toBe(0);
      await page.press("ArrowUp"); // at the first item, clamped
      expect(await activeIdx()).toBe(0);
    });

    it("emptyIndex preserve: ArrowUp from nothing selected goes to the first item", async () => {
      await openPalette("emptyIndex=preserve");
      await page.type("zzz");
      await page.press("ArrowDown");
      for (let i = 0; i < 3; i++) await page.press("Backspace");
      await page.press("ArrowUp");
      expect(await activeIdx()).toBe(0);
    });

    it("imeGuard: keys during a composition are ignored by default and handled with imeGuard false", async () => {
      const compose = `(() => { const q = document.getElementById('q'); q.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', isComposing: true, bubbles: true, cancelable: true })); })()`;
      await openPalette();
      await page.eval(compose);
      expect(await activeIdx()).toBe(0);
      await openPalette("ime=0");
      await page.eval(compose);
      expect(await activeIdx()).toBe(1);
    });

    it("scrollOn: a list length change re-scrolls to the active option only with active-or-length", async () => {
      const run = async (query: string) => {
        await openPalette(query);
        for (let i = 0; i < 10; i++) await page.press("ArrowDown");
        await page.eval("document.querySelector('[role=listbox]').scrollTop = 0");
        await page.type("Command 1"); // 24 -> 11 items, the stored index 10 is still valid: same option id
        await page.sleep(100);
        return page.eval<boolean>(`(() => { const l = document.querySelector('[role=listbox]').getBoundingClientRect(); const o = document.querySelector('[aria-selected=true]').getBoundingClientRect(); return o.top >= l.top - 1 && o.bottom <= l.bottom + 1; })()`);
      };
      expect(await run("")).toBe(false);
      expect(await run("scrollOn=active-or-length")).toBe(true);
    });

    it("scrollIntoView keeps the active option visible while arrowing past the fold", async () => {
      await openPalette();
      for (let i = 0; i < 18; i++) await page.press("ArrowDown");
      const visible = await page.eval<boolean>(`(() => { const l = document.querySelector('[role=listbox]').getBoundingClientRect(); const o = document.querySelector('[aria-selected=true]').getBoundingClientRect(); return o.top >= l.top - 1 && o.bottom <= l.bottom + 1; })()`);
      expect(visible).toBe(true);
    });

    it("Enter runs the highlighted item synchronously with the exit start, once; Escape inside the dialog closes without running", async () => {
      await openPalette();
      await page.type("reopen");
      await page.press("Enter");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      expect((await logLines()).filter((l) => l.startsWith("run"))).toEqual(["run Reopen closed tab"]);
      await openPalette();
      await page.eval("document.getElementById('log').textContent = ''");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      const lines = await logLines();
      expect(lines.some((l) => l.startsWith("run"))).toBe(false);
      // Element-scoped Escape with stopPropagation off: the page still sees it, and it was prevented.
      expect(lines).toContain("doc-escape prevented=true");
    });

    it("a click on an option runs it; a click on the backdrop closes (click-target)", async () => {
      await openPalette();
      await page.click("[role=option]:nth-child(2)");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      expect(await logLines()).toContain("run Open URL");
      await openPalette();
      await page.clickAt(30, 30);
      await page.waitFor("!document.querySelector('[role=dialog]')");
    });
  });

  describe("React StrictMode (development build: effects run twice)", () => {
    it("modal: focus lands on the first control, is not yanked back to the opener by the simulated cleanup, and is restored on the real close", async () => {
      await open("demo=modal&strict=1&raf=1");
      await page.click("#opener");
      await page.waitFor("document.querySelector('[role=dialog]')");
      await page.sleep(100);
      expect(await page.eval<number>("window.__observerMounts")).toBe(2); // one mount, effects ran twice: StrictMode is live
      expect(await activeId()).toBe("first");
      await page.press("Tab");
      expect(await activeId()).toBe("mid");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      await page.sleep(50);
      expect(await activeId()).toBe("opener");
    });

    it("effect capture: closing returns focus to the real opener, not to the first control the simulated remount recorded", async () => {
      for (const query of ["demo=modal&strict=1&capture=effect", "demo=modal&strict=1&capture=effect&when=always&restore=sync"]) {
        await open(query);
        await page.click("#opener");
        await page.waitFor("document.querySelector('[role=dialog]')");
        await page.sleep(100);
        expect(await page.eval<number>("window.__observerMounts"), query).toBe(2);
        expect(await activeId(), query).toBe("first");
        await page.press("Escape");
        await page.waitFor("!document.querySelector('[role=dialog]')");
        await page.sleep(50);
        expect(await activeId(), query).toBe("opener");
      }
    });

    it("render capture with restoreWhen always and sync timing also returns to the opener", async () => {
      await open("demo=modal&strict=1&when=always&restore=sync");
      await page.click("#opener");
      await page.waitFor("document.querySelector('[role=dialog]')");
      await page.sleep(100);
      expect(await activeId()).toBe("first");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      await page.sleep(50);
      expect(await activeId()).toBe("opener");
    });

    it("menu: the simulated unmount does not close the open menu or leave a stray timer; the real close still exits once", async () => {
      await open("demo=menu&strict=1&exit=200");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.sleep(500);
      expect(await exists("[role=menu]")).toBe(true);
      await page.clickAt(900, 500);
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect((await logLines()).filter((l) => l === "menu:exit-start" || l === "menu:gone")).toEqual(["menu:exit-start", "menu:gone"]);
    });

    it("palette: Enter runs the item once", async () => {
      await open("demo=palette&strict=1");
      await page.click("#opener");
      await page.waitFor("document.querySelector('[role=dialog]')");
      await page.type("reopen");
      await page.press("Enter");
      await page.waitFor("!document.querySelector('[role=dialog]')");
      expect((await logLines()).filter((l) => l.startsWith("run"))).toEqual(["run Reopen closed tab"]);
    });
  });
});
