// Real-browser checks for the opt-in 0.6.0 options: each option alone and in the combinations a locked
// UI (ViewRight) needs, with keyboard, focus and Escape still working.
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
  await page.goto(`${origin}/variants.html?${query}`);
};
const log = () => page.eval<string>("document.getElementById('log').textContent");
const count = (sel: string) => page.eval<number>(`document.querySelectorAll(${JSON.stringify(sel)}).length`);
const attr = (sel: string, name: string) => page.eval<string | null>(`document.querySelector(${JSON.stringify(sel)})?.getAttribute(${JSON.stringify(name)}) ?? null`);
const openMenu = async (query: string) => {
  await open(`demo=menu&${query}`);
  await page.click("#area", "right");
  await page.waitFor("document.querySelector('[role=menu]')");
};
const activeLabel = () => page.eval<string | null>("document.querySelector('.rk-menu__item.is-active .rk-menu__label')?.textContent ?? null");

describe.skipIf(!chrome)("opt-in options in real headless Chrome", () => {
  beforeAll(async () => {
    out = mkdtempSync(join(tmpdir(), "rk-app-shell-options-"));
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

  describe("ContextMenu", () => {
    it("defaults: div rows with aria-disabled, lead column, first row highlighted, focus on the menu", async () => {
      await openMenu("");
      expect(await page.eval<string>("document.querySelector('.rk-menu__item').tagName")).toBe("DIV");
      expect(await attr(".rk-menu__item", "aria-disabled")).toBe("true");
      expect(await count(".rk-menu__lead")).toBe(4);
      expect(await activeLabel()).toBe("Copy");
      expect(await page.eval<boolean>("document.activeElement === document.querySelector('[role=menu]')")).toBe(true);
    });

    it("itemAs=button: button[role=menuitem] with native disabled; disabled rows cannot run; clicking an enabled row runs it", async () => {
      await openMenu("itemAs=button");
      expect(await page.eval<string[]>("[...document.querySelectorAll('.rk-menu__item')].map((e) => e.tagName + ':' + e.getAttribute('role') + ':' + e.getAttribute('type'))")).toEqual(Array(4).fill("BUTTON:menuitem:button"));
      expect(await page.eval<boolean>("document.querySelector('.rk-menu__item').disabled")).toBe(true);
      expect(await attr(".rk-menu__item", "aria-disabled")).toBe(null);
      expect(await page.eval<number>("document.querySelectorAll('.rk-menu__item[tabindex=\"-1\"]').length")).toBe(4);
      await page.click(".rk-menu__item:first-child"); // disabled: nothing happens, menu stays
      expect(await count("[role=menu]")).toBe(1);
      await page.press("ArrowDown"); // Copy -> Paste (Cut disabled is skipped)
      expect(await activeLabel()).toBe("Paste");
      await page.eval("[...document.querySelectorAll('.rk-menu__item')].find((e) => e.textContent.includes('Select all')).click()");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await log()).toContain("menu:all");
    });

    it("showLead=false drops the lead column only", async () => {
      await openMenu("lead=0");
      expect(await count(".rk-menu__lead")).toBe(0);
      expect(await count(".rk-menu__label")).toBe(4);
    });

    it("scrimClassName lands on the scrim", async () => {
      await openMenu("scrim=my-scrim");
      expect(await count(".rk-menu-scrim.my-scrim")).toBe(1);
    });

    it("renderItem replaces row content but keeps role, state classes and keyboard", async () => {
      await openMenu("render=1");
      expect(await count(".rk-menu__item > i.custom-row")).toBe(4);
      expect(await count(".rk-menu__lead")).toBe(0);
      expect(await page.eval<string>("document.querySelector('.rk-menu__item.is-active i').textContent")).toBe("Copy");
      await page.press("Enter");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await log()).toContain("menu:copy");
    });

    it("autoFocus=false: focus and selection stay in the field, yet arrows, Home/End, Enter and Escape drive the menu", async () => {
      await open("demo=menu&autoFocus=0");
      const openFromKeyboardContext = () => page.eval("document.getElementById('area').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 120, clientY: 120 }))");
      await page.eval("(() => { const f = document.getElementById('field'); f.focus(); f.setSelectionRange(2, 7); })()");
      await openFromKeyboardContext(); // no pointer press, so the field keeps focus like a keyboard-opened menu
      await page.waitFor("document.querySelector('[role=menu]')");
      expect(await page.eval<string>("document.activeElement.id")).toBe("field");
      expect(await page.eval<string>("(() => { const f = document.getElementById('field'); return f.selectionStart + ',' + f.selectionEnd; })()")).toBe("2,7");
      expect(await activeLabel()).toBe("Copy");
      await page.press("ArrowDown");
      expect(await activeLabel()).toBe("Paste");
      await page.press("End");
      expect(await activeLabel()).toBe("Select all");
      await page.press("Home");
      expect(await activeLabel()).toBe("Copy");
      expect(await attr("[role=menu]", "aria-activedescendant")).not.toBe(null);
      await page.press("Enter");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await log()).toContain("menu:copy");
      expect(await page.eval<string>("document.activeElement.id")).toBe("field"); // focus restored to the opener
      await openFromKeyboardContext();
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await page.eval<string>("document.activeElement.id")).toBe("field");
    });

    it("autoFocus=false: Ctrl/Cmd chords pass through to the page (Cmd+C is not swallowed)", async () => {
      await open("demo=menu&autoFocus=0");
      await page.eval("(() => { window.__seen = []; window.addEventListener('keydown', (e) => window.__seen.push(e.key + ':' + e.defaultPrevented)); })()");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.press("c", ["meta"]);
      expect(await page.eval<string[]>("window.__seen")).toEqual(["c:false"]);
      expect(await count("[role=menu]")).toBe(1);
    });

    it("autoFocus=false: Tab closes the menu like the focused menu does", async () => {
      await open("demo=menu&autoFocus=0");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.press("Tab");
      await page.waitFor("!document.querySelector('[role=menu]')");
    });

    it("initialHighlight=none: nothing highlighted; ArrowDown starts at the first row, ArrowUp at the last; Enter with none does nothing", async () => {
      await openMenu("highlight=none");
      expect(await activeLabel()).toBe(null);
      expect(await attr("[role=menu]", "aria-activedescendant")).toBe(null);
      await page.press("Enter");
      expect(await count("[role=menu]")).toBe(1);
      await page.press("ArrowDown");
      expect(await activeLabel()).toBe("Copy");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=menu]')");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.press("ArrowUp");
      expect(await activeLabel()).toBe("Select all");
    });

    it("autoFocus=false + initialHighlight=none + itemAs=button together stay keyboard operable", async () => {
      await open("demo=menu&autoFocus=0&highlight=none&itemAs=button");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.press("ArrowUp");
      expect(await activeLabel()).toBe("Select all");
      await page.press("Enter");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await log()).toContain("menu:all");
    });

    it("exitMs: the chosen action runs at once, the menu plays is-closing and unmounts after the delay; input during exit is ignored", async () => {
      await openMenu("exit=200&scrim=s");
      await page.press("Enter");
      const started = Date.now();
      expect(await log()).toContain("menu:copy"); // not delayed
      expect(await log()).toContain("menu:exit-start");
      expect(await count("[role=menu].is-closing")).toBe(1);
      expect(await count(".rk-menu-scrim.is-closing")).toBe(1);
      expect(await page.eval<string>("document.querySelector('[role=menu]').style.getPropertyValue('--rk-menu-exit-ms')")).toBe("200ms");
      await page.press("Enter"); // ignored while exiting
      await page.click(".rk-menu-scrim"); // scrim is click-through while exiting, nothing happens
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(Date.now() - started).toBeGreaterThanOrEqual(150);
      const l = await log();
      expect(l.split("menu:copy").length - 1).toBe(1);
      expect(l.split("menu:exit-start").length - 1).toBe(1);
      expect(l.split("menu:closed").length - 1).toBe(1);
    });

    it("exitMs with exitClassName, Escape and scrim click", async () => {
      await openMenu("exit=120&exitClass=bye");
      await page.press("Escape");
      expect(await count("[role=menu].bye")).toBe(1);
      expect(await count("[role=menu].is-closing")).toBe(0);
      await page.waitFor("!document.querySelector('[role=menu]')");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.clickAt(5, 5);
      expect(await count(".rk-menu-scrim.bye")).toBe(1);
      await page.waitFor("!document.querySelector('[role=menu]')");
    });

    it("B1: a second right-click during an exit animation opens a new menu that the old exit does not close", async () => {
      await openMenu("exit=250");
      await page.press("Escape");
      await page.sleep(20);
      await page.click("#area", "right"); // lands on the page: the exiting scrim is click-through
      await page.sleep(500);
      expect(await count("[role=menu]")).toBe(1);
      expect(await count("[role=menu].is-closing")).toBe(0);
      expect((await log()).split("menu:closed").length - 1).toBe(0); // the old exit was cancelled; onClose never fired for the new menu
      await page.press("ArrowDown");
      expect(await activeLabel()).toBe("Paste");
      await page.press("Enter");
      await page.waitFor("!document.querySelector('[role=menu]')");
      expect(await log()).toContain("menu:paste");
    });

    it("B1 (useContextMenu): re-opening during the exit mounts a fresh menu that the old exit cannot close", async () => {
      await open("demo=hook");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      await page.press("Escape");
      await page.sleep(20);
      await page.click("#area", "right");
      await page.sleep(500);
      expect(await count("[role=menu]")).toBe(1);
      expect(await count("[role=menu].is-closing")).toBe(0);
      await page.press("Enter");
      expect(await log()).toContain("hook:alpha");
    });

    it("N1: a composing Space/Enter (IME) is neither prevented nor runs the highlighted item when autoFocus=false", async () => {
      await open("demo=menu&autoFocus=0");
      await page.click("#area", "right");
      await page.waitFor("document.querySelector('[role=menu]')");
      const prevented = await page.eval<boolean[]>(`(() => {
        const out = [];
        for (const init of [{ key: 'Enter', isComposing: true }, { key: ' ', isComposing: true }, { key: 'Process', keyCode: 229 }]) {
          const e = new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true });
          document.getElementById('field').dispatchEvent(e);
          out.push(e.defaultPrevented);
        }
        return out;
      })()`);
      expect(prevented).toEqual([false, false, false]);
      expect(await count("[role=menu]")).toBe(1);
      expect(await log()).not.toContain("menu:copy");
    });

    describe("keyboard=escape-only", () => {
      const eo = "keyboard=escape-only&itemAs=button&autoFocus=0";
      it("no highlight, items are tabbable buttons, Tab/Space/Enter are native, Escape closes", async () => {
        await open(`demo=menu&${eo}`);
        await page.eval("document.getElementById('page-btn').focus()");
        await page.eval("document.getElementById('area').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 120, clientY: 120 }))");
        await page.waitFor("document.querySelector('[role=menu]')");
        expect(await count(".rk-menu__item.is-active")).toBe(0);
        expect(await attr("[role=menu]", "aria-activedescendant")).toBe(null);
        expect(await count(".rk-menu__item[tabindex]")).toBe(0);
        // Space on a focused page button outside the menu still activates it (nothing captured page-wide).
        await page.press(" ");
        expect(await log()).toContain("page:btn");
        expect(await count("[role=menu]")).toBe(1);
        // Tab walks into the item buttons (Cut is disabled and skipped natively).
        await page.eval("document.getElementById('field').focus()");
        const seen: string[] = [];
        for (let i = 0; i < 6 && seen.length < 3; i++) {
          await page.press("Tab");
          const t = await page.eval<string>("document.activeElement.closest('.rk-menu__item') ? document.activeElement.textContent : ''");
          if (t) seen.push(t);
        }
        expect(seen.map((t) => t.replace(/^c/, "").replace(/(⌘|Ctrl)C$/, ""))).toEqual(["Copy", "Paste", "Select all"]);
        expect(await count("[role=menu]")).toBe(1);
        // Enter on the focused item activates it exactly once.
        await page.press("Enter");
        await page.waitFor("!document.querySelector('[role=menu]')");
        expect((await log()).split("menu:all").length - 1).toBe(1);
      });

      it("Escape closes from the page, restores focus, and the listener is gone afterwards", async () => {
        await open(`demo=menu&${eo}`);
        await page.eval("document.getElementById('field').focus()");
        await page.eval("document.getElementById('area').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 120, clientY: 120 }))");
        await page.waitFor("document.querySelector('[role=menu]')");
        await page.press("Escape");
        await page.waitFor("!document.querySelector('[role=menu]')");
        expect(await page.eval<string>("document.activeElement.id")).toBe("field");
        const prevented = await page.eval<boolean>("(() => { const e = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }); document.getElementById('field').dispatchEvent(e); return e.defaultPrevented; })()");
        expect(prevented).toBe(false);
      });

      it("IME guard applies to Escape; arrows and Enter on the page are not captured", async () => {
        await open(`demo=menu&${eo}`);
        await page.click("#area", "right");
        await page.waitFor("document.querySelector('[role=menu]')");
        const r = await page.eval<boolean[]>(`(() => {
          const send = (init) => { const e = new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true }); document.getElementById('field').dispatchEvent(e); return e.defaultPrevented; };
          return [send({ key: 'Escape', isComposing: true }), send({ key: 'ArrowDown' }), send({ key: 'Enter' }), send({ key: ' ' }), send({ key: 'Tab' })];
        })()`);
        expect(r).toEqual([false, false, false, false, false]);
        expect(await count("[role=menu]")).toBe(1);
      });
    });

    it("default (no exitMs): closes immediately, no exit class or variable", async () => {
      await openMenu("");
      await page.press("Escape");
      await page.waitFor("!document.querySelector('[role=menu]')", 300);
      expect(await log()).not.toContain("exit-start");
    });
  });

  describe("ModalShell and Dialog", () => {
    it("default dismisses on backdrop mousedown", async () => {
      await open("demo=modal");
      await page.eval("document.querySelector('.rk-overlay').dispatchEvent(new MouseEvent('mousedown', { bubbles: true }))");
      await page.waitFor("document.getElementById('closed')");
    });

    it("dismissOn=click: mousedown alone does nothing; a press that starts inside and ends on the backdrop does not close; a full backdrop click does", async () => {
      await open("demo=modal&dismissOn=click");
      await page.eval("document.querySelector('.rk-overlay').dispatchEvent(new MouseEvent('mousedown', { bubbles: true }))");
      await page.sleep(250);
      expect(await count(".rk-modal")).toBe(1);
      await page.eval("(() => { document.querySelector('.rk-modal').dispatchEvent(new MouseEvent('mousedown', { bubbles: true })); document.querySelector('.rk-overlay').dispatchEvent(new MouseEvent('click', { bubbles: true })); })()");
      await page.sleep(250);
      expect(await count(".rk-modal")).toBe(1);
      await page.clickAt(5, 5);
      await page.waitFor("document.getElementById('closed')");
    });

    it("dismissOn=click still closes on Escape and keeps the focus trap", async () => {
      await open("demo=modal&dismissOn=click");
      await page.press("Tab");
      expect(await page.eval<boolean>("!!document.activeElement.closest('.rk-modal')")).toBe(true);
      await page.press("Escape");
      await page.waitFor("document.getElementById('closed')");
    });

    it("overlayClassName and overlayStyle reach the backdrop", async () => {
      await open("demo=modal&overlayClass=my-overlay&overlayBg=rgb(1,2,3)");
      expect(await count(".rk-overlay.my-overlay")).toBe(1);
      expect(await page.eval<string>("getComputedStyle(document.querySelector('.rk-overlay')).backgroundColor")).toBe("rgb(1, 2, 3)");
    });

    it("ariaLabel replaces aria-labelledby", async () => {
      await open("demo=modal&ariaLabel=Plain%20dialog");
      expect(await attr(".rk-modal", "aria-label")).toBe("Plain dialog");
      expect(await attr(".rk-modal", "aria-labelledby")).toBe(null);
      await open("demo=modal");
      expect(await attr(".rk-modal", "aria-label")).toBe("Plain");
    });

    it("ActionDialog: three actions in order with variants and classes, explicit alertdialog, autoFocus action, trap, Escape", async () => {
      await open("demo=action&role=alertdialog");
      expect(await count("[role=alertdialog]")).toBe(1);
      expect(await page.eval<string[]>("[...document.querySelectorAll('.rk-modal__actions .rk-btn')].map((b) => b.textContent)")).toEqual(["Don't save", "Cancel", "Save"]);
      expect(await count(".rk-btn--danger.act-dont")).toBe(1);
      expect(await count(".rk-btn--primary.act-save")).toBe(1);
      expect(await page.eval<string>("document.activeElement.textContent")).toBe("Save");
      await page.press("Tab");
      expect(await page.eval<string>("document.activeElement.textContent")).toBe("Don't save"); // wrapped, trapped
      await page.press("Tab");
      expect(await page.eval<string>("document.activeElement.textContent")).toBe("Cancel");
      await page.click(".act-save");
      expect(await log()).toContain("action:save");
      await page.press("Escape");
      await page.waitFor("document.getElementById('closed')");
      expect(await log()).toContain("action:closed");
    });

    it("ActionDialog defaults to role=dialog and, with no autoFocus action, focuses the first control", async () => {
      await open("demo=action&focus=0");
      expect(await count("[role=dialog]")).toBe(1);
      expect(await count("[role=alertdialog]")).toBe(0);
      expect(await page.eval<string>("document.activeElement.textContent")).toBe("Don't save");
    });
  });

  describe("CommandPalette", () => {
    it("inputAriaLabel is independent of the placeholder", async () => {
      await open("demo=palette&inputAria=Search%20files");
      expect(await attr("input[role=combobox]", "aria-label")).toBe("Search files");
      expect(await attr("input[role=combobox]", "placeholder")).toBe("Search or run a command…");
      await open("demo=palette");
      expect(await attr("input[role=combobox]", "aria-label")).toBe("Search or run a command…");
    });

    it("enterDelayMs delays running the command", async () => {
      await open("demo=palette&delay=400");
      await page.waitFor("document.activeElement === document.querySelector('input[role=combobox]')");
      await page.type("theme");
      await page.press("Enter");
      const t = Date.now();
      await page.waitFor("document.getElementById('log').textContent.includes('cmd:theme')");
      expect(Date.now() - t).toBeGreaterThanOrEqual(300);
      await open("demo=palette&delay=0");
      await page.waitFor("document.activeElement === document.querySelector('input[role=combobox]')");
      await page.type("theme");
      await page.press("Enter");
      await page.waitFor("document.getElementById('log').textContent.includes('cmd:theme')", 1000);
    });

    it("rank replaces the built-in filtering; rankCommands is the built-in", async () => {
      await open("demo=palette&rank=only-theme");
      expect(await page.eval<string[]>("[...document.querySelectorAll('[role=option]')].map((o) => o.dataset.command)")).toEqual(["theme"]);
      await open("demo=palette&rank=reverse");
      expect(await page.eval<string[]>("[...document.querySelectorAll('[role=option]')].map((o) => o.dataset.command)")).toEqual(["theme", "save", "open"]); // reversed rank, then grouped by first-seen group
      await open("demo=palette");
      expect(await page.eval<string[]>("[...document.querySelectorAll('[role=option]')].map((o) => o.dataset.command)")).toEqual(["open", "save", "theme"]);
    });

    it("searchIcon and renderIcon replace the icons; arrow selection still works", async () => {
      await open("demo=palette&searchIcon=1&renderIcon=1&iconClass=ic");
      expect(await count("#my-search")).toBe(1);
      expect(await count(".rk-palette__field svg")).toBe(0);
      expect(await count(".rk-palette__icon.ic > u.my-icon")).toBe(3);
      expect(await attr(".my-icon", "data-active")).toBe("true");
      await page.press("ArrowDown");
      expect(await attr("[role=option][aria-selected=true]", "data-command")).toBe("save");
      await open("demo=palette&searchIcon=none");
      expect(await count(".rk-palette__field svg")).toBe(0);
    });
  });

  describe("ToastViewport", () => {
    const push = (kind: string, msg = "Hello") => page.eval(`window.__bus.push(${JSON.stringify(msg)}, { kind: ${JSON.stringify(kind)}, ttl: 0 })`);

    it("roles per kind: ViewRight mapping (error alert, others no role); default unchanged", async () => {
      await open("demo=toast&roles=vr");
      await push("info");
      await push("error", "bad");
      await page.waitFor("document.querySelector('.rk-toast--error')");
      expect(await attr(".rk-toast--info", "role")).toBe(null);
      expect(await attr(".rk-toast--error", "role")).toBe("alert");
      await open("demo=toast");
      await push("info");
      await page.waitFor("document.querySelector('.rk-toast--info')");
      expect(await attr(".rk-toast--info", "role")).toBe("status");
      await open("demo=toast&roles=log");
      await push("success");
      await push("error", "bad");
      await page.waitFor("document.querySelector('.rk-toast--error')");
      expect(await attr(".rk-toast--success", "role")).toBe("log");
      expect(await attr(".rk-toast--error", "role")).toBe("alert");
    });

    it("rowClassName (function) and exitClassName with exitMs", async () => {
      await open("demo=toast&rowClass=1&exitClass=bye&exit=500");
      await push("success");
      await page.waitFor("document.querySelector('.row-success')");
      expect(await count(".rk-toast.row-success")).toBe(1);
      await page.click(".rk-toast__x");
      expect(await count(".rk-toast.is-leaving.bye.row-leaving")).toBe(1);
      const t = Date.now();
      await page.waitFor("!document.querySelector('.rk-toast')", 3000);
      expect(Date.now() - t).toBeGreaterThanOrEqual(300);
    });

    it("renderGlyph replaces the glyph element; renderRow replaces the row and keeps role and classes", async () => {
      await open("demo=toast&glyph=1");
      await push("warning");
      await page.waitFor("document.querySelector('.rk-toast')");
      expect(await count(".rk-toast__glyph")).toBe(0);
      expect(await count(".rk-toast > svg.my-glyph[data-kind=warning]")).toBe(1);
      await open("demo=toast&row=1");
      await push("error", "custom row");
      await page.waitFor("document.querySelector('.my-row')");
      expect(await attr(".my-row", "role")).toBe("alert");
      expect(await page.eval<boolean>("document.querySelector('.my-row').classList.contains('rk-toast--error')")).toBe(true);
      await page.click(".my-row");
      await page.waitFor("!document.querySelector('.my-row')", 2000);
    });

    it("N3: changing exitMs while a toast is exiting does not strand it", async () => {
      await open("demo=toast&exit=600&rerender=1");
      await push("success");
      await page.waitFor("document.querySelector('.rk-toast')");
      await page.click(".rk-toast__x");
      await page.sleep(100);
      await page.eval("window.__setExit(50)");
      await page.waitFor("!document.querySelector('.rk-toast')", 3000);
    });

    it("--rk-toast-width sets the stack width; the default stays 26rem", async () => {
      await open("demo=toast");
      await push("info");
      await page.waitFor("document.querySelector('.rk-toast')");
      expect(await page.eval<number>("document.querySelector('[data-rk-toasts]').getBoundingClientRect().width")).toBe(416);
      await page.eval("document.documentElement.style.setProperty('--rk-toast-width', '20rem')");
      expect(await page.eval<number>("document.querySelector('[data-rk-toasts]').getBoundingClientRect().width")).toBe(320);
    });
  });
});
