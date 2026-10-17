#!/usr/bin/env node
// Fails when a built web bundle (vite `dist/`) carries dev, mock or fixture code (replaces per-app
// checkReleaseBundle.mjs copies). Scans text files for forbidden marker strings and flags chunk file
// names that match a forbidden name pattern. Read-only: it never writes, builds or follows a symlink.
// Usage: right-release dist-scan --dist <dir> --forbid <string>... [--forbid-file <path>]
//          [--forbid-name <regex>]... [--ext js,mjs,css,html,json,map]
// A --forbid-file is JSON (`["a","b"]` or `{ "forbid": [...], "forbidNames": [...] }`) or plain text with
// one marker per line (blank lines and `#` comments ignored).
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { basename, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const DEFAULT_EXTENSIONS = Object.freeze(["js", "mjs", "css", "html", "json", "map"]);
export const DEFAULT_FORBIDDEN_NAMES = Object.freeze([/^(devSource|mockDataSource|fixtures)[-.]/]);

export function parseForbidFile(text, name = "forbid file") {
  const trimmed = text.trim();
  if (trimmed.startsWith("[") || trimmed.startsWith("{")) {
    let data;
    try { data = JSON.parse(trimmed); } catch (error) { throw new Error(`${name}: invalid JSON (${error.message})`); }
    const forbid = Array.isArray(data) ? data : data.forbid ?? [];
    const forbidNames = Array.isArray(data) ? [] : data.forbidNames ?? [];
    if (!Array.isArray(forbid) || !Array.isArray(forbidNames) || ![...forbid, ...forbidNames].every((s) => typeof s === "string" && s)) throw new Error(`${name}: entries must be non-empty strings`);
    return { forbid, forbidNames };
  }
  return { forbid: trimmed.split(/\r?\n/).map((l) => l.trim()).filter((l) => l && !l.startsWith("#")), forbidNames: [] };
}

/** Scan `dist`. Returns { dist, files, scanned, markers, offenders: [{file, reason}] }. */
export function scanDist(dist, { forbid = [], forbidNames = [], extensions = DEFAULT_EXTENSIONS } = {}) {
  const root = resolve(dist);
  if (!existsSync(root) || !statSync(root).isDirectory()) throw new Error(`No ${root}: build first.`);
  const nameRes = [...DEFAULT_FORBIDDEN_NAMES, ...forbidNames.map((pattern) => {
    try { return new RegExp(pattern); } catch (error) { throw new Error(`--forbid-name ${JSON.stringify(pattern)} is not a regular expression (${error.message})`); }
  })];
  const extRe = new RegExp(`\\.(${extensions.map((e) => e.replace(/[^\w]/g, "")).filter(Boolean).join("|")})$`);
  const offenders = [];
  let files = 0;
  let scanned = 0;
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) { walk(path); continue; }
      files += 1;
      const rel = relative(root, path);
      if (nameRes.some((re) => re.test(basename(path)))) offenders.push({ file: rel, reason: "forbidden chunk name" });
      if (!extRe.test(path)) continue;
      scanned += 1;
      const text = readFileSync(path, "utf8");
      for (const marker of forbid) if (text.includes(marker)) offenders.push({ file: rel, reason: `marker: ${marker}` });
    }
  };
  walk(root);
  return { dist: root, files, scanned, markers: forbid.length, offenders };
}

export function distScanFailed(result) {
  return result.offenders.length > 0 || result.files === 0 || result.markers === 0;
}

const USAGE = "usage: right-release dist-scan --dist <dir> --forbid <string>... [--forbid-file <path>] [--forbid-name <regex>]... [--ext js,css,...]";

export function parseArgs(argv) {
  const out = { dist: null, forbid: [], forbidNames: [], extensions: undefined };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const value = () => { const v = argv[++i]; if (v === undefined || v.startsWith("--")) throw new Error(`${a} needs a value`); return v; };
    if (a === "--dist") out.dist = value();
    else if (a === "--forbid") out.forbid.push(value());
    else if (a === "--forbid-name") out.forbidNames.push(value());
    else if (a === "--ext") out.extensions = value().split(",").filter(Boolean);
    else if (a === "--forbid-file") {
      const file = value();
      const parsed = parseForbidFile(readFileSync(file, "utf8"), file);
      out.forbid.push(...parsed.forbid);
      out.forbidNames.push(...parsed.forbidNames);
    } else throw new Error(`unknown argument ${a}`);
  }
  if (!out.dist) throw new Error("--dist is required");
  return out;
}

export function main(argv) {
  let result;
  try {
    const args = parseArgs(argv);
    if (args.forbid.length === 0) throw new Error("no --forbid markers given");
    result = scanDist(args.dist, args);
  } catch (error) {
    console.error(`right-release dist-scan: ${error.message}\n${USAGE}`);
    return 2;
  }
  if (distScanFailed(result)) {
    console.error(`Release bundle contains forbidden code:\n${result.offenders.map((o) => `${o.file}: ${o.reason}`).join("\n") || `no files in ${result.dist}`}`);
    return 1;
  }
  console.log(`Release bundle clean: ${result.markers} markers absent from ${result.dist} (${result.scanned} files scanned)`);
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) process.exit(main(process.argv.slice(2)));
