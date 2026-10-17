#!/usr/bin/env node
// `right-release init --name <App Name> --id <reverse.dns.bundle.id> [--slug <lowercase>] [--dir <root>] [--dry-run]`
// (bogusyogi/rightkit#385): renames the desktop app template into a real app, in place. Ports PostRight's
// crates/tools/src/init.rs. The template identity (`RightApp`, `rightapp`, `RIGHTAPP`, `app.rightsuite.rightapp`)
// is rewritten in text files; nothing else is touched and no gate or build is run.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const SKIP_DIRS = new Set(["node_modules", "target", ".git", "dist", "gen"]);
export const TEXT_EXT = new Set(["json", "toml", "mjs", "ts", "tsx", "rs", "md", "html", "css", "yaml", "yml", "sh"]);
// Assembled so this file is not rewritten into the new identity when the template is renamed.
const TITLE = ["Right", "App"].join("");
const LOWER = ["right", "app"].join("");
const UPPER = ["RIGHT", "APP"].join("");
const TEMPLATE_ID = `app.rightsuite.${LOWER}`;
const SELF = path.basename(fileURLToPath(import.meta.url));

export const INIT_USAGE = "usage: right-release init --name <Product Name> --id <reverse.dns.bundle.id> [--slug <lowercase>] [--dir <root>] [--dry-run]";

export function parseInitArgs(argv) {
  const opts = { name: undefined, id: undefined, slug: undefined, dir: ".", dryRun: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (["--name", "--id", "--slug", "--dir"].includes(arg)) {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} needs a value\n${INIT_USAGE}`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--dry-run") opts.dryRun = true;
    else throw new Error(`unknown init argument: ${arg}\n${INIT_USAGE}`);
  }
  if (!opts.name || !opts.id) throw new Error(INIT_USAGE);
  return opts;
}

/** Validate and derive the identity. `slug` defaults to the lowercased name without spaces. */
export function resolveIdentity({ name, id, slug }) {
  const resolvedSlug = slug ?? name.replaceAll(" ", "").toLowerCase();
  if (!/^[A-Za-z][A-Za-z0-9 ]*$/.test(name)) throw new Error(`name must be letters, digits and spaces starting with a letter: ${name}\n${INIT_USAGE}`);
  if (!/^[a-z][a-z0-9]*$/.test(resolvedSlug)) throw new Error(`slug must be lowercase letters and digits starting with a letter: ${resolvedSlug}\n${INIT_USAGE}`);
  if (!/^[A-Za-z][A-Za-z0-9-]*(\.[A-Za-z][A-Za-z0-9-]*)+$/.test(id)) throw new Error(`id must be a reverse-DNS bundle id with at least two segments: ${id}\n${INIT_USAGE}`);
  const warnings = [];
  if (name.replaceAll(" ", "").toLowerCase() !== resolvedSlug) {
    warnings.push(`--name "${name}" lowercased is not slug "${resolvedSlug}"; check productName and installer names.`);
  }
  if (!id.toLowerCase().endsWith(`.${resolvedSlug}`)) {
    warnings.push(`--id "${id}" does not end in .${resolvedSlug}; check the bundle id.`);
  }
  return { name, id, slug: resolvedSlug, warnings };
}

export function walkTextFiles(dir, out = []) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) walkTextFiles(full, out);
    } else if (entry.isFile() && TEXT_EXT.has(path.extname(entry.name).slice(1))) {
      out.push(full);
    }
  }
  return out;
}

/** Rewrite the template identity in one string; the bundle id first, then the three spellings. */
export function renameText(text, { name, id, slug }) {
  return text
    .replaceAll(TEMPLATE_ID, id)
    .replaceAll(UPPER, slug.toUpperCase())
    .replaceAll(TITLE, name)
    .replaceAll(LOWER, slug);
}

/** Rename the template under `root`. Returns the identity and the changed file paths (relative, sorted). */
export function initTemplate(root, options) {
  const identity = resolveIdentity(options);
  const changed = [];
  for (const file of walkTextFiles(root)) {
    if (path.basename(file) === SELF) continue;
    let before;
    try {
      before = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    const after = renameText(before, identity);
    if (after === before) continue;
    if (!options.dryRun) writeFileSync(file, after);
    changed.push(path.relative(root, file).split(path.sep).join("/"));
  }
  return { ...identity, changed: changed.sort() };
}

export function runInitCommand(argv, io = { stdout: console.log, stderr: console.error }) {
  const opts = parseInitArgs(argv);
  const result = initTemplate(path.resolve(opts.dir), opts);
  for (const warning of result.warnings) io.stderr(`warning: ${warning}`);
  io.stdout(`right-release init${opts.dryRun ? " --dry-run" : ""}: ${result.changed.length} file(s) ${opts.dryRun ? "would change" : "changed"}`);
  for (const file of result.changed) io.stdout(`  ${file}`);
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    runInitCommand(process.argv.slice(2));
  } catch (error) {
    console.error(`right-release init: ${error.message}`);
    process.exitCode = 1;
  }
}
