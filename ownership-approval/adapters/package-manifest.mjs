#!/usr/bin/env node
import { createHash } from "node:crypto";
import { lstatSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const DEFAULT_MANIFEST_RELATIVE = "ownership/adapters/adapter-files.json";
const POLICY_FILES = new Set(["ownership/trust.json", "ownership/public-check.json", "ownership/public-check-app.pem"]);
function policyPath(relative) {
  return POLICY_FILES.has(relative) || relative === "ownership/installer" || relative.startsWith("ownership/installer/");
}
function digest(file) { return createHash("sha256").update(readFileSync(file)).digest("hex"); }

export function createProtectedFileManifest(root, { manifestRelative = DEFAULT_MANIFEST_RELATIVE } = {}) {
  const base = path.resolve(root);
  const files = [];
  function walk(dir) {
    for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const file = path.join(dir, entry.name);
      const relative = path.relative(base, file).replaceAll("\\", "/");
      const stat = lstatSync(file);
      if (stat.isSymbolicLink()) throw new Error(`protected package may not contain symlinks: ${relative}`);
      // Policy is independently protected & validated. Enrollment may update it
      // without replacing the installed scanner's code inventory.
      if (policyPath(relative)) {
        if (relative === "ownership/installer" ? !stat.isDirectory() : !stat.isFile()) throw new Error(`protected policy path has an invalid file type: ${relative}`);
        continue;
      }
      if (entry.isDirectory()) { files.push({ path: relative, type: "directory", sha256: null }); walk(file); }
      else if (entry.isFile() && relative !== manifestRelative) files.push({ path: relative, type: "file", sha256: digest(file) });
      else if (!entry.isFile()) throw new Error(`protected package contains a special file: ${relative}`);
    }
  }
  walk(base);
  return { schema: 1, manifestRelative, files };
}

export function verifyProtectedFileManifest(root, manifestPath = path.join(root, ...DEFAULT_MANIFEST_RELATIVE.split("/"))) {
  try {
    const expected = JSON.parse(readFileSync(manifestPath, "utf8"));
    if (expected?.schema !== 1 || expected.manifestRelative !== DEFAULT_MANIFEST_RELATIVE || !Array.isArray(expected.files)) return { ok: false, problems: ["protected file manifest is malformed"] };
    const actual = createProtectedFileManifest(root, { manifestRelative: DEFAULT_MANIFEST_RELATIVE });
    const validEntry = (item) => item && typeof item.path === "string" && !path.posix.isAbsolute(item.path) && !item.path.split("/").some((part) => !part || part === "." || part === "..") && ((item.type === "file" && /^[a-f0-9]{64}$/.test(item.sha256 ?? "")) || (item.type === "directory" && item.sha256 === null));
    if (!expected.files.every(validEntry) || new Set(expected.files.map((item) => item.path)).size !== expected.files.length) return { ok: false, problems: ["protected file manifest entries are malformed or duplicated"] };
    const a = new Map(actual.files.map((item) => [item.path, JSON.stringify(item)]));
    const e = new Map(expected.files.map((item) => [item.path, JSON.stringify(item)]));
    const problems = [];
    for (const [file, entry] of e) if (a.get(file) !== entry) problems.push(`protected file changed or is missing: ${file}`);
    for (const file of a.keys()) if (!e.has(file)) problems.push(`unexpected protected file: ${file}`);
    return { ok: problems.length === 0, problems };
  } catch (error) { return { ok: false, problems: [`protected file manifest cannot be verified: ${error.message}`] }; }
}

function main(args) {
  const value = (name) => { const index = args.indexOf(name); return index < 0 ? undefined : args[index + 1]; };
  const root = value("--root");
  if (args.includes("--verify")) {
    if (!root) throw new Error("usage: node package-manifest.mjs --verify --root <install-tree>");
    const result = verifyProtectedFileManifest(root);
    process.stdout.write(`${JSON.stringify(result)}\n`);
    if (!result.ok) process.exitCode = 1;
    return;
  }
  const output = value("--out");
  if (!root || !output) throw new Error("usage: node package-manifest.mjs --root <install-tree> --out <content-hash-manifest.json>");
  const manifest = createProtectedFileManifest(root);
  writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`, { flag: "wx" });
  process.stdout.write(`${JSON.stringify({ ok: true, files: manifest.files.length, manifestPath: output })}\n`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(process.argv.slice(2)); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
