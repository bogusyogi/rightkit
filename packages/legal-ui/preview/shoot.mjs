#!/usr/bin/env node
// Screenshots all ten first-run LegalGate variants (5 apps x light/dark) via
// the shared QA headless-Chrome/CDP engine, then writes a contact sheet HTML
// referencing the PNGs. Requires the legal-ui-preview static server already
// running at PREVIEW_ORIGIN (default http://127.0.0.1:4310).
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const outDir = join(pkgRoot, ".preview");
mkdirSync(outDir, { recursive: true });

const origin = process.env.PREVIEW_ORIGIN ?? "http://127.0.0.1:4310";
const qaScript = "D:/Claude/tools/lib/qa-engine/qa.mjs";

// Mirrors preview/variants.ts APPS order/keys — kept in plain JS here so this
// script needs no TS loader to run standalone. LIGHT-ONLY (locked 2026-07-17);
// ids keep the -light suffix so PNG paths stay stable.
const APP_NAMES = {
  heardright: "HeardRight",
  viewright: "ViewRight",
  scraperight: "ScrapeRight",
  mailright: "MailRight",
  coderight: "CodeRight",
};
const IDS = Object.keys(APP_NAMES).map((appKey) => `${appKey}-light`);

/** The gate opens its OWN window at ONE size for every app — it does not inherit
 * the app's main window. Those vary from HeardRight's 660x400 (too small: the
 * gate cramps and scrolls) to MailRight's 1200x800 (too big: the content strands
 * in space). Adrian sized this off the ViewRight render, 2026-07-17.
 * The app-side wiring must open the first-run gate window at exactly this size. */
export const GATE_WINDOW = [960, 560];

const results = [];
for (const id of IDS) {
  const [appKey] = id.split("-");
  const [width, height] = GATE_WINDOW;
  const outFile = join(".preview", `${id}.png`);
  const url = `${origin}/?id=${id}`;
  console.log(`[shoot] ${id} <- ${url} @ ${width}x${height}`);
  const result = spawnSync(
    process.execPath,
    [qaScript, "--shot", "--url", url, "--out", outFile, "--width", String(width), "--height", String(height)],
    { cwd: pkgRoot, stdio: "inherit" },
  );
  if (result.status !== 0) throw new Error(`screenshot failed for ${id} (exit ${result.status})`);
  results.push({ id, appName: `${APP_NAMES[appKey]} — ${width}×${height}`, file: `${id}.png` });
}

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>@rightkit/legal-ui — LegalGate contact sheet</title>
<style>
  body { margin: 0; background: #0b0b0a; color: #eee; font: 14px/1.4 -apple-system, Segoe UI, sans-serif; padding: 32px; }
  h1 { font-size: 18px; font-weight: 600; margin: 0 0 4px; }
  p.sub { color: #999; margin: 0 0 28px; }
  .row { margin-bottom: 40px; }
  .row h2 { font-size: 14px; text-transform: uppercase; letter-spacing: 0.08em; color: #ccc; margin: 0 0 12px; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; max-width: 1400px; }
  figure { margin: 0; border: 1px solid #333; border-radius: 8px; overflow: hidden; background: #111; }
  figure img { display: block; width: 100%; height: auto; }
  figcaption { padding: 8px 12px; font-size: 12px; color: #aaa; border-top: 1px solid #333; }
</style>
</head>
<body>
<h1>@rightkit/legal-ui — LegalGate first-run contact sheet</h1>
<p class="sub">All five Right Suite apps, light-only (locked 2026-07-17), real brand tokens, Tanker wordmarks. Local-only — not published, not wired into any app.</p>
<div class="grid">
${results
  .map(
    (r) => `  <figure><img src="${r.file}" alt="${r.appName} light" /><figcaption>${r.appName}</figcaption></figure>`,
  )
  .join("\n")}
</div>
</body>
</html>
`;

writeFileSync(join(outDir, "contact-sheet.html"), html);
console.log(`[shoot] wrote ${join(outDir, "contact-sheet.html")}`);
