#!/usr/bin/env node
// Packaged macOS app identity assertions (M19 owner gap 3): codesign Identifier,
// TeamIdentifier, hardened runtime, no ad-hoc signature, expected cdhash, and
// lipo architectures. Asserts only what the caller declares; it never signs.
// Usage: right-release identity-scan --target <path> [--identifier <id>] [--team <OU>]
//          [--cdhash <hex>] [--arch <arch>]... [--allow-adhoc]
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";

/** Parse `codesign -dv --verbose=4` output (stderr text) into fields. */
export function parseCodesignDisplay(text) {
  const out = { authorities: [] };
  for (const line of String(text).split(/\r?\n/)) {
    const m = /^([A-Za-z][A-Za-z0-9 ]*)=(.*)$/.exec(line.trim());
    if (!m) continue;
    const [, key, value] = m;
    if (key === "Authority") out.authorities.push(value);
    else if (key === "Identifier") out.identifier = value;
    else if (key === "TeamIdentifier") out.team = value;
    else if (key === "CDHash") out.cdhash = value.toLowerCase();
    else if (key === "Signature") out.signature = value;
  }
  const flags = /flags=0x[0-9a-f]+\(([^)]*)\)/i.exec(String(text));
  out.flags = flags ? flags[1].split(",").map((s) => s.trim()).filter(Boolean) : [];
  out.adhoc = out.signature === "adhoc" || out.flags.includes("adhoc");
  out.runtime = out.flags.includes("runtime");
  return out;
}

/** Parse `lipo -archs` output. */
export function parseLipoArchs(text) {
  return String(text).trim().split(/\s+/).filter(Boolean);
}

/** Return the list of failures for the declared expectations (empty = pass). */
export function assertPackagedIdentity(display, archs, expected = {}) {
  const failures = [];
  if (!expected.allowAdhoc && display.adhoc) failures.push("signature is ad-hoc; a release must be Developer ID signed");
  if (expected.identifier && display.identifier !== expected.identifier) {
    failures.push(`Identifier must be ${expected.identifier} (got ${display.identifier ?? "none"})`);
  }
  if (expected.team && display.team !== expected.team) {
    failures.push(`TeamIdentifier must be ${expected.team} (got ${display.team ?? "none"})`);
  }
  if (expected.cdhash && display.cdhash !== String(expected.cdhash).toLowerCase()) {
    failures.push(`CDHash must be ${expected.cdhash} (got ${display.cdhash ?? "none"})`);
  }
  if (!expected.allowAdhoc && !display.runtime) failures.push("hardened runtime flag is missing");
  for (const arch of expected.archs ?? []) {
    if (!archs.includes(arch)) failures.push(`missing architecture ${arch} (has ${archs.join(",") || "none"})`);
  }
  return failures;
}

function capture(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  return { status: r.status ?? 1, output: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

function main(argv) {
  const expected = { archs: [] };
  let target = "";
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--target") target = argv[++i] ?? "";
    else if (a === "--identifier") expected.identifier = argv[++i];
    else if (a === "--team") expected.team = argv[++i];
    else if (a === "--cdhash") expected.cdhash = argv[++i];
    else if (a === "--arch") expected.archs.push(argv[++i]);
    else if (a === "--allow-adhoc") expected.allowAdhoc = true;
    else { console.error(`identity-scan: unknown argument ${a}`); return 2; }
  }
  if (!target) { console.error("usage: right-release identity-scan --target <path> [--identifier <id>] [--team <OU>] [--cdhash <hex>] [--arch <arch>]... [--allow-adhoc]"); return 2; }
  const display = capture("codesign", ["-dv", "--verbose=4", target]);
  if (display.status !== 0) { console.error(`identity-scan: codesign could not read ${target}:\n${display.output}`); return 1; }
  let archs = [];
  if (expected.archs.length) {
    const lipo = capture("lipo", ["-archs", target]);
    if (lipo.status !== 0) { console.error(`identity-scan: lipo failed:\n${lipo.output}`); return 1; }
    archs = parseLipoArchs(lipo.output);
  }
  const failures = assertPackagedIdentity(parseCodesignDisplay(display.output), archs, expected);
  for (const f of failures) console.error(`identity-scan: ${f}`);
  if (!failures.length) console.log(`identity-scan: ${target} ok`);
  return failures.length ? 1 : 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exit(main(process.argv.slice(2)));
