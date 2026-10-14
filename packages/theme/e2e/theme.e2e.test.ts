import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import type { Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { findChrome, launch, serveDir, type Page } from "./browser.js";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const cli = join(pkgRoot, "bin", "rightkit-theme.mjs");
const shellCss = join(pkgRoot, "..", "react-shell", "shell.css");
const chrome = findChrome();
const shotsDir = process.env.RK_SHOTS_DIR ?? mkdtempSync(join(tmpdir(), "rk-theme-shots-"));

const genright = {
  name: "GenRight",
  code: "GR",
  colors: { brand: "#9567F5", "brand-action": "#7040C4", "brand-action-hover": "#6034AA", "brand-soft": "#F2ECFE", "brand-ink": "#6234AD", "brand-border": "#CDB8FA" },
  wordmark: "GenRight",
  wordmarkFont: '"Tanker", sans-serif',
};
const ember = { name: "HeardRight", code: "HR", colors: { brand: "#FF5630" } };
const teal = { name: "Tealight", code: "TL", colors: { brand: "#0C6AA5" }, dark: { brand: "#4DB1F0" } };
// Light brand colour as the only accent: white text on it fails AA, so the contrast gate must refuse it.
const pale = { name: "Pale", code: "PL", colors: { brand: "#BFD9FF", "brand-ink": "#BFD9FF" } };

const work = mkdtempSync(join(tmpdir(), "rk-theme-e2e-"));
const run = (...args: string[]) => {
  const r = spawnSync(process.execPath, [cli, ...args], { encoding: "utf8" });
  const line = r.stdout.trim().split("\n").filter(Boolean).pop() ?? "{}";
  return { code: r.status, json: JSON.parse(line) as Record<string, unknown>, stderr: r.stderr };
};
const write = (name: string, value: unknown) => {
  const p = join(work, name);
  writeFileSync(p, typeof value === "string" ? value : JSON.stringify(value, null, 2));
  return p;
};

describe("rightkit-theme CLI on real files", () => {
  afterAll(() => rmSync(work, { recursive: true, force: true }));

  it("generates suite + tailwind CSS from the real GenRight brand shape, and is idempotent", () => {
    const brand = write("genright.json", genright);
    const out = join(work, "brand.css");
    const first = run("generate", brand, "--out", out, "--format", "both");
    expect(first.code).toBe(0);
    expect(first.json).toMatchObject({ event: "brand_theme_synced", ok: true, brand: "GR", changed: true, contrastPass: true });
    const css = readFileSync(out, "utf8");
    // Light accent for controls is the action colour; dark accent is the logo violet lifted to AA on the elevated surface; accent-ink is chosen per mode.
    expect(css).toMatch(/:root \{[^}]*--rk-brand: #7040C4;[^}]*--rk-brand-dark: #986BF5;[^}]*--rk-wordmark-font: "Tanker", sans-serif;/s);
    expect(css).toMatch(/:root\[data-theme="dark"\] \{[^}]*--rk-brand-ink: #986BF5;[^}]*--rk-accent-ink: #1A1208;/s);
    expect(css).toMatch(/@media \(prefers-color-scheme: dark\) \{\s+:root:not\(\[data-theme="light"\]\) \{/);
    expect(css).toMatch(/@theme \{[^}]*--color-brand-action: #7040C4;/s);
    const again = run("generate", brand, "--out", out, "--format", "both");
    expect(again.json.changed).toBe(false);
  });

  it("--check detects drift between the brand file and the committed stylesheet", () => {
    const brand = write("drift.json", ember);
    const out = join(work, "drift.css");
    expect(run("generate", brand, "--out", out).code).toBe(0);
    expect(run("generate", brand, "--out", out, "--check").code).toBe(0);
    writeFileSync(out, readFileSync(out, "utf8").replace("#FF5630", "#FF0000"));
    const drift = run("generate", brand, "--out", out, "--check");
    expect(drift.code).toBe(1);
    expect(drift.json.event).toBe("brand_theme_drift");
  });

  it("derives a readable dark accent when the brand file has none", () => {
    const brand = write("ember.json", { name: "Dim", colors: { brand: "#2A1F6B" } });
    const out = join(work, "dim.css");
    expect(run("generate", brand, "--out", out, "--allow-contrast-failures").code).toBe(0);
    const css = readFileSync(out, "utf8");
    const dark = /--rk-brand-dark: (#[0-9A-F]{6})/.exec(css)![1];
    expect(dark).not.toBe("#2A1F6B");
    // Real contrast against the shell's dark plane (#17171A) and elevated surface (#1F1F23).
    const lum = (hex: string) => {
      const n = parseInt(hex.slice(1), 16);
      const c = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => {
        const s = v / 255;
        return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    };
    expect((lum(dark) + 0.05) / (lum("#1F1F23") + 0.05)).toBeGreaterThanOrEqual(4.5);
  });

  it("refuses a brand whose accent text fails WCAG AA, naming the failing pair", () => {
    const brand = write("pale.json", pale);
    const r = run("contrast", brand);
    expect(r.code).toBe(1);
    expect(r.json.event).toBe("brand_contrast_failed");
    expect(JSON.stringify(r.json.failures)).toContain("accent text on plane");
    expect(run("generate", brand, "--out", join(work, "pale.css")).code).toBe(1);
    expect(run("generate", brand, "--out", join(work, "pale.css"), "--allow-contrast-failures").code).toBe(0);
  });

  it("rejects malformed brand files with a precise path", () => {
    const bad = write("bad.json", { name: "X", colors: { brand: "purple" } });
    const r = run("generate", bad, "--out", join(work, "bad.css"));
    expect(r.code).toBe(1);
    expect(String(r.json.message)).toContain("colors.brand");
    const unknown = write("bad2.json", { name: "X", colors: { brand: "#123456" }, tokens: { light: { sparkle: "#fff" } } });
    expect(String(run("contrast", unknown).json.message)).toContain("tokens.light.sparkle");
  });

  it("flags hard-coded colours outside approved contexts, with file and line", () => {
    const css = write("app.css", [".a { color: var(--rk-ink); }", ".b { color: #12ab34; }", ":root { --x: #abcdef; }", ".c { background: rgba(1,2,3,.4); } /* rightkit-theme: allow */", ".d { box-shadow: 0 0 1px #345; }"].join("\n"));
    const r = run("tokens", css);
    expect(r.code).toBe(1);
    const findings = r.json.findings as Array<{ line: number; value: string }>;
    expect(findings.map((f) => [f.line, f.value])).toEqual([[2, "#12ab34"]]);
    const clean = write("clean.css", ".a { color: var(--rk-ink); }\n:root { --x: #abcdef; }\n");
    expect(run("tokens", clean).code).toBe(0);
  });
});

describe.skipIf(!chrome)("generated brand CSS in real headless Chrome (muted), light and dark", () => {
  let page: Page;
  let close: () => Promise<void>;
  let server: Server;
  let origin: string;
  let site: string;

  const sheet = (b: typeof genright, id: string) => `
    <section class="brand" id="${id}" style="--n:'${b.name}'">
      <h3>${b.name}</h3>
      <div class="row"><button class="rk-btn rk-btn--primary">Primary action</button><button class="rk-btn rk-btn--secondary">Secondary</button><span class="rk-badge rk-badge--accent">Accent</span>
      <button class="rk-toggle" role="switch" aria-checked="true"><span class="rk-toggle__knob"></span></button></div>
      <p class="link">Accent text link style</p>
      <div class="sw"><i style="background:var(--rk-brand-mark)"></i><i style="background:var(--rk-accent)"></i><i style="background:var(--rk-brand-soft,var(--rk-accent-soft))"></i><i style="background:var(--rk-brand-border,var(--rk-line))"></i></div>
    </section>`;

  beforeAll(async () => {
    mkdirSync(shotsDir, { recursive: true });
    site = mkdtempSync(join(tmpdir(), "rk-theme-site-"));
    copyFileSync(shellCss, join(site, "shell.css"));
    const brands: Array<[string, typeof genright]> = [["gr", genright], ["hr", ember as typeof genright], ["tl", teal as typeof genright]];
    for (const [id, b] of brands) {
      const brandPath = join(site, `${id}.json`);
      writeFileSync(brandPath, JSON.stringify(b));
      const r = spawnSync(process.execPath, [cli, "generate", brandPath, "--out", join(site, `${id}.css`), "--allow-contrast-failures"], { encoding: "utf8" });
      expect(r.status).toBe(0);
    }
    const scoped = brands
      .map(([id]) => {
        const text = readFileSync(join(site, `${id}.css`), "utf8");
        const root = /:root \{[^}]*\}/.exec(text)![0].replace(":root {", `#${id} {\n  --rk-accent: var(--rk-brand); --rk-accent-soft: color-mix(in srgb, var(--rk-accent) 14%, transparent);`);
        const dark = /:root\[data-theme="dark"\] \{[^}]*\}/.exec(text)![0].replace(':root[data-theme="dark"] {', `:root[data-theme="dark"] #${id} {\n  --rk-accent: var(--rk-brand-dark); --rk-accent-soft: color-mix(in srgb, var(--rk-accent) 18%, transparent);`);
        return root + dark;
      })
      .join("\n");
    writeFileSync(
      join(site, "sheet.html"),
      `<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="shell.css"><style>${scoped}
         body{margin:0;background:var(--rk-plane);color:var(--rk-ink);font:14px system-ui;padding:24px;display:grid;grid-template-columns:repeat(3,1fr);gap:16px}
         .brand{background:var(--rk-elevated);border-radius:12px;box-shadow:var(--rk-card);padding:16px}
         .row{display:flex;gap:10px;align-items:center;margin:12px 0}
         .link{color:var(--rk-brand-ink,var(--rk-accent))} .sw{display:flex;gap:8px}.sw i{width:36px;height:36px;border-radius:8px;display:block;box-shadow:var(--rk-card)}
       </style><body>${brands.map(([id, b]) => sheet(b, id)).join("")}<script>window.__ready=true</script>`,
    );
    writeFileSync(
      join(site, "index.html"),
      `<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="shell.css">
       ${brands.map(([id]) => `<link rel="stylesheet" href="${id}.css" media="all" data-brand="${id}">`).join("")}
       <style>
         body{margin:0;background:var(--rk-plane);color:var(--rk-ink);font:14px system-ui;padding:24px;display:grid;grid-template-columns:repeat(3,1fr);gap:16px}
         .brand{background:var(--rk-elevated);border-radius:12px;box-shadow:var(--rk-card);padding:16px}
         .row{display:flex;gap:10px;align-items:center;margin:12px 0}
         .link{color:var(--rk-brand-ink,var(--rk-accent))} .sw{display:flex;gap:8px}.sw i{width:36px;height:36px;border-radius:8px;display:block;box-shadow:var(--rk-card)}
       </style>
       <body><script>window.__ready=true</script>`,
    );
    ({ server, origin } = await serveDir(site));
    const s = await launch(chrome!);
    page = s.page;
    close = s.close;
    await page.setViewport(1100, 360);
  }, 60_000);

  afterAll(async () => {
    await close?.();
    server?.close();
    if (site) rmSync(site, { recursive: true, force: true });
  });

  /** One brand at a time: only its stylesheet applies, so computed colours are attributable. */
  const load = async (id: string, theme: "light" | "dark" | "system", scheme: "light" | "dark") => {
    await page.setColorScheme(scheme);
    await page.goto(`${origin}/index.html`);
    await page.eval(`(() => {
      document.querySelectorAll('link[data-brand]').forEach(l => { l.disabled = l.dataset.brand !== ${JSON.stringify(id)}; });
      ${theme === "system" ? "document.documentElement.removeAttribute('data-theme')" : `document.documentElement.setAttribute('data-theme', ${JSON.stringify(theme)})`};
    })()`);
    await page.sleep(80);
  };
  const css = (name: string) => page.eval<string>(`getComputedStyle(document.documentElement).getPropertyValue(${JSON.stringify(name)}).trim()`);

  it("applies the GenRight accent pair per mode through the shell's own --rk-accent", async () => {
    await load("gr", "light", "light");
    expect((await css("--rk-accent")).toUpperCase()).toBe("#7040C4");
    expect((await css("--rk-accent-ink")).toUpperCase()).toBe("#FFFFFF");
    await load("gr", "dark", "light");
    expect((await css("--rk-accent")).toUpperCase()).toBe("#986BF5");
    expect((await css("--rk-accent-ink")).toUpperCase()).toBe("#1A1208");
  });

  it("follows the OS in system mode, and an explicit choice beats the OS", async () => {
    await load("gr", "system", "dark");
    expect((await css("--rk-accent")).toUpperCase()).toBe("#986BF5");
    await load("gr", "system", "light");
    expect((await css("--rk-accent")).toUpperCase()).toBe("#7040C4");
    await load("gr", "light", "dark");
    expect((await css("--rk-accent")).toUpperCase()).toBe("#7040C4");
    await load("gr", "dark", "light");
    expect((await css("--rk-accent")).toUpperCase()).toBe("#986BF5");
  });

  it("paints a primary button in the brand colour with legible text, and captures all brands in both modes", async () => {
    for (const theme of ["light", "dark"] as const) {
      // Show every brand side by side: enable all stylesheets only for the capture by scoping each card.
      await load("gr", theme, theme);
      const buttons = await page.eval<{ bg: string; fg: string }>(
        `(() => { const b = document.createElement('button'); b.className='rk-btn rk-btn--primary'; document.body.appendChild(b); const s = getComputedStyle(b); const r = { bg: s.backgroundColor, fg: s.color }; b.remove(); return r; })()`,
      );
      expect(buttons.bg).toBe(theme === "light" ? "rgb(112, 64, 196)" : "rgb(152, 107, 245)");
      expect(buttons.fg).toBe(theme === "light" ? "rgb(255, 255, 255)" : "rgb(26, 18, 8)");
    }
    // Contact sheet: three brands scoped to their own card, driven by the same generated variables.
    for (const theme of ["light", "dark"] as const) {
      await page.setColorScheme(theme);
      await page.goto(`${origin}/sheet.html`);
      await page.eval(`document.documentElement.setAttribute('data-theme', ${JSON.stringify(theme)})`);
      await page.sleep(120);
      const rgbs = await page.eval<string[]>(`['gr','hr','tl'].map(id => getComputedStyle(document.querySelector('#'+id+' .rk-btn--primary')).backgroundColor)`);
      expect(new Set(rgbs).size).toBe(3);
      await page.screenshot(join(shotsDir, `theme-brands-${theme}.png`));
    }
  });
});
