// Cascade behaviour of the shipped stylesheets in real headless Chrome: the layered entry loses to host
// utilities and unlayered host CSS, renders identically to shell.css when the host adds nothing, and
// the structure file works on its own. Pages are static; no framework involved.
import { copyFileSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import type { Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { findChrome, launch, serveDir, type Page } from "./browser.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const chrome = findChrome();

const FIXTURE = `
<div id="fx">
  <div class="rk-shell rk-shell--fused" style="height:300px"><header class="rk-top"><div class="rk-top__side"><span class="rk-top__lights"></span></div><div class="rk-top__title">T</div><div class="rk-top__drag"></div></header>
    <div class="rk-body"><aside class="rk-side"><nav class="rk-nav"><div class="rk-group"><h6>G</h6><button class="rk-item is-active"><span class="rk-item__label">One</span></button><button class="rk-item"><span class="rk-item__label">Two</span></button></div></nav></aside><main class="rk-plane"><div class="rk-page"><div class="rk-page__scroll">
      <button id="primary" class="rk-btn rk-btn--primary bg-hot">Go</button><button id="secondary" class="rk-btn rk-btn--secondary">Cancel</button><button class="rk-btn rk-btn--danger rk-btn--sm">Del</button>
      <input class="rk-input" value="x"><div class="rk-card">Card</div><span class="rk-badge rk-badge--ok">ok</span><div class="rk-seg"><button class="rk-seg__item is-on">A</button></div>
      <button class="rk-toggle" role="switch" aria-checked="true"><span class="rk-toggle__knob"></span></button>
    </div></div></main></div></div>
  <div class="rk-overlay rk-overlay--center"><div class="rk-modal" style="width:300px"><h2 class="rk-modal__title">Title</h2><p class="rk-modal__desc">d</p><div class="rk-modal__actions"><button id="inmodal" class="font-big">Plain</button><button id="inmodal2">Inherit</button></div></div></div>
  <div class="rk-menu" style="left:20px;top:20px;width:200px"><div class="rk-menu__item is-active"><span class="rk-menu__lead">✓</span><span class="rk-menu__label">Copy</span></div><div class="rk-menu__sep"></div><div class="rk-menu__item is-disabled"><span class="rk-menu__label">Cut</span></div></div>
  <div class="rk-toasts" data-rk-toasts><div class="rk-toast rk-toast--error"><span class="rk-toast__glyph">!</span><span class="rk-toast__msg">m</span><button class="rk-toast__x">x</button></div></div>
</div>`;

const HASH = `(() => {
  const hash = (s) => { let h = 2166136261; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
  const out = {};
  const walk = (el, path) => {
    const cs = getComputedStyle(el);
    const props = [];
    for (let i = 0; i < cs.length; i++) { const p = cs[i]; if (p === 'transition' || p.startsWith('animation')) continue; props.push(p + ':' + cs.getPropertyValue(p)); }
    const r = el.getBoundingClientRect();
    const key = path + '/' + el.tagName.toLowerCase() + '.' + (el.getAttribute('class') || '');
    out[key] = hash(props.sort().join(';') + [r.x, r.y, r.width, r.height].map((n) => Math.round(n * 10) / 10).join(','));
    let i = 0;
    for (const c of el.children) walk(c, key + '[' + (i++) + ']');
  };
  walk(document.getElementById('fx'), '');
  return out;
})()`;

const ORDER = "<style>@layer theme, base, rk, components, utilities;</style>";
const HOST = `<style>
@layer base { button { background-color: rgb(250, 0, 0); } }
@layer utilities { .bg-hot { background-color: rgb(1, 2, 3); } .font-big { font-size: 31px; } }
</style>`;
const page = (head: string) => `<!doctype html><html><head><meta charset="utf-8">${head}</head><body style="margin:0">${FIXTURE}<script>window.__ready = true;</script></body></html>`;

const PAGES: Record<string, string> = {
  "unlayered.html": page('<link rel="stylesheet" href="shell.css">'),
  "plain-layered.html": page(`${ORDER}<link rel="stylesheet" href="shell.layered.css">`),
  "unlayered-host.html": page(`<link rel="stylesheet" href="shell.css">${HOST}`),
  "layered-host.html": page(`${ORDER}<link rel="stylesheet" href="shell.layered.css">${HOST}`),
  "layered-unlayered-host.html": page(`${ORDER}<link rel="stylesheet" href="shell.layered.css">${HOST}<style>.rk-btn--primary { background-color: rgb(9, 9, 9); }</style>`),
  "layered-no-order.html": page(`<link rel="stylesheet" href="shell.layered.css">${HOST}`),
  "halves.html": page('<link rel="stylesheet" href="shell.structure.css"><link rel="stylesheet" href="shell.visual.css">'),
  "structure-only.html": page('<link rel="stylesheet" href="shell.structure.css">'),
};

let browserPage: Page;
let close: () => Promise<void>;
let server: Server;
let dir: string;
let origin: string;

const go = async (name: string) => {
  await browserPage.setViewport(1100, 720);
  await browserPage.setColorScheme("light");
  await browserPage.goto(`${origin}/${name}`);
  await browserPage.moveTo(1, 719);
  await browserPage.sleep(450); // let the entrance animations settle so styles are comparable
};
const css = (selector: string, prop: string) => browserPage.eval<string>(`getComputedStyle(document.querySelector(${JSON.stringify(selector)})).${prop}`);

describe.skipIf(!chrome)("stylesheet cascade in real headless Chrome", () => {
  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), "rk-app-shell-cascade-"));
    for (const f of ["shell.css", "shell.layered.css", "shell.structure.css", "shell.visual.css"]) copyFileSync(join(pkgRoot, f), join(dir, f));
    for (const [name, html] of Object.entries(PAGES)) writeFileSync(join(dir, name), html);
    writeFileSync(join(dir, "index.html"), "<!doctype html>"); // serveDir falls back to it (favicon requests)
    ({ server, origin } = await serveDir(dir));
    const session = await launch(chrome!);
    browserPage = session.page;
    close = session.close;
  }, 60_000);

  afterAll(async () => {
    await close?.();
    server?.close();
    if (dir) rmSync(dir, { recursive: true, force: true });
  });

  it("the problem in 0.5.x: unlayered shell.css beats a Tailwind utility on the same element, and its button reset beats a font utility", async () => {
    await go("unlayered-host.html");
    expect(await css("#primary", "backgroundColor")).toBe("rgb(112, 64, 196)"); // --rk-brand wins over .bg-hot
    expect(await css("#inmodal", "fontSize")).toBe("13px"); // `font: inherit` beats .font-big
  });

  it("shell.layered.css: a host utility in @layer utilities overrides a layered rk rule and the button font reset (G1, G2)", async () => {
    await go("layered-host.html");
    expect(await css("#primary", "backgroundColor")).toBe("rgb(1, 2, 3)");
    expect(await css("#inmodal", "fontSize")).toBe("31px");
    expect(await css("#inmodal2", "fontSize")).toBe("13px"); // untouched buttons still inherit
  });

  it("with the documented order line, a lower base layer (preflight-style button reset) does not beat rk", async () => {
    await go("layered-host.html");
    expect(await css("#secondary", "backgroundColor")).not.toBe("rgb(250, 0, 0)");
    expect(await css("#secondary", "backgroundColor")).toMatch(/^rgba\(35, 31, 26, 0\.08/); // --rk-fill-2
  });

  it("without the order line rk is declared before base, so base wins: the order line is required", async () => {
    await go("layered-no-order.html");
    expect(await css("#secondary", "backgroundColor")).toBe("rgb(250, 0, 0)");
  });

  it("unlayered host CSS beats any rk rule regardless of specificity or source order", async () => {
    await go("layered-unlayered-host.html");
    expect(await css("#primary", "backgroundColor")).toBe("rgb(9, 9, 9)");
  });

  it("with no host CSS the layered entry renders identically to shell.css (computed style and box of every element)", async () => {
    await go("unlayered.html");
    const unlayered = await browserPage.eval<Record<string, string>>(HASH);
    await go("plain-layered.html");
    const layered = await browserPage.eval<Record<string, string>>(HASH);
    expect(Object.keys(unlayered).length).toBeGreaterThan(40);
    expect(Object.keys(layered).filter((k) => layered[k] !== unlayered[k])).toEqual([]);
  });

  it("structure + visual loaded as two files equal shell.css", async () => {
    await go("unlayered.html");
    const whole = await browserPage.eval<Record<string, string>>(HASH);
    await go("halves.html");
    const halves = await browserPage.eval<Record<string, string>>(HASH);
    expect(Object.keys(halves).filter((k) => halves[k] !== whole[k])).toEqual([]);
  });

  it("structure alone keeps overlays, menu and toasts functional and adds no decoration", async () => {
    await go("structure-only.html");
    expect(await css(".rk-overlay", "position")).toBe("fixed");
    expect(await css(".rk-overlay", "zIndex")).toBe("70");
    expect(await css(".rk-overlay--center", "alignItems")).toBe("center");
    expect(await css(".rk-menu", "position")).toBe("fixed");
    expect(await css(".rk-menu", "zIndex")).toBe("81");
    expect(await css(".rk-toasts", "position")).toBe("fixed");
    expect(await css(".rk-toast", "pointerEvents")).toBe("auto");
    expect(await css(".rk-body", "display")).toBe("flex");
    expect(await css(".rk-menu", "backgroundColor")).toBe("rgba(0, 0, 0, 0)");
    expect(await css("#primary", "borderRadius")).toBe("0px");
    expect(await css(".rk-modal", "boxShadow")).toBe("none");
  });
});
