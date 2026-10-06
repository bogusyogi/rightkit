#!/usr/bin/env node
// rightkit-theme: build-time CLI over @rightkit/theme (no runtime role in an app).
//   generate <brand.json> --out <file.css> [--format suite|tailwind|both] [--check] [--allow-contrast-failures]
//   contrast <brand.json>
//   tokens   <file...>            hard-coded colour scan
// Machine output: one JSON line per command on stdout; human detail on stderr. Exit 1 on any failure.
import { checkBrandFile, generateFile, scanFiles, checkGeneratedCss } from "../dist/node.js";

const [cmd, ...rest] = process.argv.slice(2);
const VALUE_FLAGS = new Set(["--out", "--format"]);
const positional = [];
const flags = new Map();
for (let i = 0; i < rest.length; i += 1) {
  const a = rest[i];
  if (VALUE_FLAGS.has(a)) flags.set(a.slice(2), rest[(i += 1)]);
  else if (a.startsWith("--")) flags.set(a.slice(2), true);
  else positional.push(a);
}
const flag = (name) => flags.get(name);
const has = (name) => flags.has(name);

const out = (obj) => process.stdout.write(`${JSON.stringify(obj)}\n`);
const fail = (event, detail) => {
  out({ event, ok: false, ...detail });
  process.exit(1);
};

try {
  if (cmd === "generate") {
    const brandPath = positional[0];
    const outPath = flag("out");
    if (!brandPath || !outPath) fail("usage", { usage: "rightkit-theme generate <brand.json> --out <file.css> [--format suite|tailwind|both] [--check]" });
    const format = flag("format") ?? "suite";
    const { brand, report, logo } = checkBrandFile(brandPath);
    if (logo.length) fail("brand_logo_mismatch", { problems: logo });
    if (!report.pass && !has("allow-contrast-failures")) fail("brand_contrast_failed", { brand: brand.code ?? brand.name, failures: report.failures });
    const result = generateFile(brandPath, outPath, { format, check: has("check") });
    if (has("check") && result.changed) fail("brand_theme_drift", { out: outPath, hint: "run rightkit-theme generate" });
    const problems = format === "tailwind" ? [] : checkGeneratedCss(result.css, brand);
    if (problems.length) fail("brand_theme_incomplete", { problems });
    out({ event: "brand_theme_synced", ok: true, brand: brand.code ?? brand.name, out: outPath, changed: result.changed, contrastPass: report.pass });
  } else if (cmd === "contrast") {
    const { brand, report } = checkBrandFile(positional[0]);
    for (const r of report.results) process.stderr.write(`${r.pass ? "PASS" : "FAIL"} [${r.mode}] ${r.id}: ${r.ratio} (min ${r.min})\n`);
    if (!report.pass) fail("brand_contrast_failed", { brand: brand.code ?? brand.name, failures: report.failures });
    out({ event: "brand_contrast_ok", ok: true, brand: brand.code ?? brand.name, checks: report.results.length });
  } else if (cmd === "tokens") {
    const findings = scanFiles(positional);
    for (const f of findings.slice(0, 80)) process.stderr.write(`${f.file}:${f.line} ${f.value}  ${f.text}\n`);
    if (findings.length) fail("hardcoded_colors", { count: findings.length, findings: findings.slice(0, 80) });
    out({ event: "tokens_ok", ok: true, files: positional.length });
  } else {
    fail("usage", { usage: "rightkit-theme generate|contrast|tokens ..." });
  }
} catch (e) {
  fail("error", { message: e instanceof Error ? e.message : String(e) });
}
