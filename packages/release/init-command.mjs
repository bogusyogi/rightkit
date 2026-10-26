#!/usr/bin/env node
// `right-release init --name <App Name> --id <reverse.dns.bundle.id> [--slug <lowercase>] [--dir <root>] [--dry-run]`
// (bogusyogi/rightkit#385): renames the desktop app template into a real app, in place. Ports PostRight's
// crates/tools/src/init.rs. The template identity (`RightApp`, `rightapp`, `RIGHTAPP`, `app.rightsuite.rightapp`)
// is rewritten in text files; nothing else is touched and no gate or build is run.
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
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

// Cargo writes Cargo.lock with its [[package]] tables sorted by name (then version) and each `dependencies` list sorted by name, and
// `cargo --locked` refuses a lock that differs from what it would write. Renaming the template's own crates moves them in that order,
// so the lock is re-sorted after the text rename; without it every `--locked` script fails on the first build of the new app.
const LOCK_FILE = "Cargo.lock";
const byteOrder = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const semverParts = (version) => {
  const [core, pre] = String(version ?? "").split("+")[0].split(/-(.*)/s);
  return { nums: core.split(".").map((part) => Number.parseInt(part, 10) || 0), pre: pre ?? null };
};
function compareSemver(a, b) {
  const x = semverParts(a);
  const y = semverParts(b);
  for (let i = 0; i < 3; i += 1) if (x.nums[i] !== y.nums[i]) return (x.nums[i] ?? 0) - (y.nums[i] ?? 0);
  if (x.pre === y.pre) return 0;
  if (x.pre === null) return 1;
  if (y.pre === null) return -1;
  return byteOrder(x.pre, y.pre);
}
const lockField = (block, key) => new RegExp(`^${key} = "([^"]*)"$`, "m").exec(block)?.[1] ?? "";
/** `"name"`, `"name version"` or `"name version (source)"`: the name and version of one `dependencies` entry. */
const lockDependency = (line) => {
  const [name = "", version = ""] = line.trim().replace(/^"|",?$/g, "").split(" ");
  return { name, version };
};

/** Sort one lock's [[package]] tables and their `dependencies` lists the way cargo writes them; everything else is kept byte for byte. */
export function sortCargoLock(text) {
  const crlf = text.includes("\r\n");
  const lf = crlf ? text.replace(/\r\n/g, "\n") : text;
  const first = /^\[\[package\]\]$/m.exec(lf);
  if (!first) return text;
  const head = lf.slice(0, first.index);
  let body = lf.slice(first.index);
  let tail = "";
  const other = /^\[(?!\[package\]\]$)/m.exec(body);
  if (other) {
    tail = body.slice(other.index);
    body = body.slice(0, other.index);
  }
  const blocks = body.replace(/\n+$/, "").split(/\n\n(?=\[\[package\]\]$)/m).map((block) => block.replace(
    /^dependencies = \[\n([\s\S]*?)\n\]$/m,
    (_, list) => `dependencies = [\n${list.split("\n").map((line) => ({ line, ...lockDependency(line) }))
      .sort((a, b) => byteOrder(a.name, b.name) || compareSemver(a.version, b.version)).map((entry) => entry.line).join("\n")}\n]`,
  ));
  blocks.sort((a, b) => byteOrder(lockField(a, "name"), lockField(b, "name")) || compareSemver(lockField(a, "version"), lockField(b, "version")));
  const out = `${head}${blocks.join("\n\n")}\n${tail ? `\n${tail}` : ""}`;
  return crlf ? out.replace(/\n/g, "\r\n") : out;
}

/** Rename the template identity in a Cargo.lock and restore cargo's order, so `cargo --locked` accepts the renamed lock. */
export function reidentifyCargoLock(text, identity) {
  return sortCargoLock(renameText(text, identity));
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
    } else if (entry.isFile() && (TEXT_EXT.has(path.extname(entry.name).slice(1)) || entry.name === LOCK_FILE)) {
      out.push(full);
    }
  }
  return out;
}

// `rightapps` is not the template identity: it is the RightApps distribution service (the `r2-rightapps` provider), which every app
// keeps. A spelling followed by a whole-word `s` is therefore left alone; `rightapp-engine`, `rightapp_protocol`, `RIGHTAPP_HOME` and
// the rest of the identity still rename.
const spelling = (word) => new RegExp(`${word}(?![sS](?![A-Za-z0-9]))`, "g");
const UPPER_WORD = spelling(UPPER);
const TITLE_WORD = spelling(TITLE);
const LOWER_WORD = spelling(LOWER);

/** Rewrite the template identity in one string; the bundle id first, then the three spellings. */
export function renameText(text, { name, id, slug }) {
  return text
    .replaceAll(TEMPLATE_ID, id)
    .replace(UPPER_WORD, () => slug.toUpperCase())
    .replace(TITLE_WORD, () => name)
    .replace(LOWER_WORD, () => slug);
}

export const LEGAL_MANIFEST = "legal/legal-manifest.json";
const sha256Hex = (bytes) => createHash("sha256").update(bytes).digest("hex");

/**
 * Recompute the `sha256` of every document and appendix of a legal manifest from the bytes `read(relativePath)` returns (paths are
 * relative to the manifest's directory; `read` returns Buffer, string or null for a missing file). Returns `{ text, changed, stale,
 * material }`: the manifest text with the new digests (2-space JSON, as `legal-snapshot` writes it), the entries whose digest moved,
 * and which of those are material agreements. Material documents feed the acceptance digest in src/generated/legal-contract.json, which
 * only `legal-snapshot` can recompute, so the caller warns about them.
 */
export function rehashLegalManifest(text, read) {
  const manifest = JSON.parse(text);
  const changed = [];
  const material = [];
  for (const entry of [...(manifest.documents ?? []), ...(manifest.appendices ?? [])]) {
    const bytes = typeof entry?.path === "string" ? read(entry.path) : null;
    if (bytes === null || bytes === undefined) continue;
    const digest = sha256Hex(typeof bytes === "string" ? Buffer.from(bytes) : bytes);
    if (entry.sha256 === digest) continue;
    entry.sha256 = digest;
    changed.push(entry.path);
    if (entry.material === true) material.push(entry.path);
  }
  return { text: changed.length ? `${JSON.stringify(manifest, null, 2)}\n` : text, changed, material };
}

/**
 * Rename the template under `root`. Returns the identity, the changed file paths (relative, sorted) and `warnings`.
 * The rename changes the bytes of text the legal manifest hashes (the privacy notice and the npm appendix carry the product name), so the
 * manifest's digests are recomputed from the renamed bytes in the same pass.
 */
export function initTemplate(root, options) {
  const identity = resolveIdentity(options);
  const changed = [];
  const warnings = [...identity.warnings];
  const renamed = new Map();
  const lockDigestsBefore = new Map();
  for (const file of walkTextFiles(root)) {
    if (path.basename(file) === SELF) continue;
    let before;
    try {
      before = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    const after = path.basename(file) === LOCK_FILE ? reidentifyCargoLock(before, identity) : renameText(before, identity);
    if (after === before) continue;
    if (path.basename(file) === LOCK_FILE) lockDigestsBefore.set(path.resolve(file), createHash("sha256").update(before).digest("hex"));
    renamed.set(path.resolve(file), after);
    if (!options.dryRun) writeFileSync(file, after);
    changed.push(path.relative(root, file).split(path.sep).join("/"));
  }
  // The rename changes Cargo.lock's bytes (the app's own crates), so every notice that records the lock's digest
  // (`Cargo.lock sha256 <hex>` / `sha256=<hex>`, checked by notice-lock-digest CA-14) would go stale: carry the new digest.
  const pairs = [...lockDigestsBefore].map(([file, old]) => ({ old, now: createHash("sha256").update(renamed.get(file)).digest("hex") }));
  if (pairs.length) {
    for (const file of walkTextFiles(root)) {
      if (path.basename(file) === SELF || path.basename(file) === LOCK_FILE) continue;
      const before = renamed.get(path.resolve(file)) ?? readFileSync(file, "utf8");
      let after = before;
      for (const { old, now } of pairs) after = after.split(old).join(now);
      if (after === before) continue;
      renamed.set(path.resolve(file), after);
      if (!options.dryRun) writeFileSync(file, after);
      const relative = path.relative(root, file).split(path.sep).join("/");
      if (!changed.includes(relative)) changed.push(relative);
    }
  }
  const manifestFile = path.join(root, ...LEGAL_MANIFEST.split("/"));
  if (existsSync(manifestFile)) {
    const legalDir = path.dirname(manifestFile);
    const read = (relative) => {
      const target = path.resolve(legalDir, relative);
      if (renamed.has(target)) return renamed.get(target);
      try { return readFileSync(target); } catch { return null; }
    };
    const rehashed = rehashLegalManifest(renamed.get(path.resolve(manifestFile)) ?? readFileSync(manifestFile, "utf8"), read);
    if (rehashed.changed.length) {
      if (!options.dryRun) writeFileSync(manifestFile, rehashed.text);
      if (!changed.includes(LEGAL_MANIFEST)) changed.push(LEGAL_MANIFEST);
    }
    for (const material of rehashed.material) {
      warnings.push(`the rename changed the material legal document legal/${material}, which feeds src/generated/legal-contract.json; run \`pnpm exec legal-snapshot\` to regenerate the acceptance contract`);
    }
  }
  return { ...identity, changed: changed.sort(), warnings };
}

/** What a person does after init: the steps no command can do for them, in order. */
export const NEXT_STEPS = Object.freeze([
  "pnpm install                      (installs the pinned packages and, through `prepare`, the pre-push hook)",
  "right-release doctor              (the build's own checks for this app, as rows)",
  "set the app's public legal host in legal-snapshot.config.json, replace legal/PRODUCT_SCHEDULE.md and legal/PRIVACY_NOTICE.md with reviewed text, then `pnpm exec legal-snapshot`",
  "register the repository on each host: add it to packages/release/ownership-repositories.json in RightKit (and its byte copy under packages/build-control-policy), then run `right-release ownership trust-refresh` on the Mac and the Dell (a person with administrator rights; an agent cannot)",
  "README.md in the template: \"Create a new app (checklist)\"",
]);

export function runInitCommand(argv, io = { stdout: console.log, stderr: console.error }) {
  const opts = parseInitArgs(argv);
  const result = initTemplate(path.resolve(opts.dir), opts);
  for (const warning of result.warnings) io.stderr(`warning: ${warning}`);
  io.stdout(`right-release init${opts.dryRun ? " --dry-run" : ""}: ${result.changed.length} file(s) ${opts.dryRun ? "would change" : "changed"}`);
  for (const file of result.changed) io.stdout(`  ${file}`);
  if (!opts.dryRun) {
    io.stdout("next:");
    for (const step of NEXT_STEPS) io.stdout(`  ${step}`);
  }
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
