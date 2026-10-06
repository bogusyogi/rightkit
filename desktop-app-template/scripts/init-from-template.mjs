#!/usr/bin/env node
// Rename the template into a real app, in place:
//   node scripts/init-from-template.mjs --slug heardright2 --name "HeardRight2" --org app.orthiclabs
// slug: lowercase letters/digits only (it becomes the Rust lib name and sidecar prefix).
// Rewrites RightApp/rightapp/RIGHTAPP/app.rightsuite.rightapp in text files, then re-runs the check.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const opt = (n) => (args.includes(`--${n}`) ? args[args.indexOf(`--${n}`) + 1] : undefined);
const slug = opt("slug");
const name = opt("name");
const org = opt("org") ?? "app.rightsuite";
if (!slug || !/^[a-z][a-z0-9]*$/.test(slug) || !name || !/^[A-Za-z][A-Za-z0-9 ]*$/.test(name)) {
  console.error("usage: init-from-template.mjs --slug <lowercase> --name <Product Name> [--org <reverse.dns.prefix>]");
  process.exit(2);
}
const SKIP = new Set(["node_modules", "target", ".git", "dist", "gen"]);
const TEXT = /\.(json|toml|mjs|ts|tsx|rs|md|html|css|yaml|yml)$/;
const walk = (d) => readdirSync(d, { withFileTypes: true }).flatMap((e) =>
  SKIP.has(e.name) ? [] : e.isDirectory() ? walk(join(d, e.name)) : TEXT.test(e.name) ? [join(d, e.name)] : []);

for (const file of walk(root)) {
  if (file.endsWith("init-from-template.mjs")) continue;
  const before = readFileSync(file, "utf8");
  const after = before
    .replaceAll("app.rightsuite.rightapp", `${org}.${slug}`)
    .replaceAll("RIGHTAPP", slug.toUpperCase())
    .replaceAll("RightApp", name)
    .replaceAll("rightapp", slug);
  if (after !== before) writeFileSync(file, after);
}
// productName must lowercase to the package name (release/installer paths rely on it).
if (name.replace(/ /g, "").toLowerCase() !== slug) console.warn(`warning: --name "${name}" lowercased != slug "${slug}"; check productName/installer names.`);
const r = spawnSync(process.execPath, [join(root, "scripts", "check-template.mjs")], { stdio: "inherit" });
process.exit(r.status ?? 1);
