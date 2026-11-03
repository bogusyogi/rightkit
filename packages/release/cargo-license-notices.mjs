#!/usr/bin/env node
// `right-release license-notices cargo [--config <file>] [--output <file>] [--check]`: writes the third-party Cargo license appendix
// (one file) for an app's Rust dependency closure with `cargo about`, run through the managed cargo client (`runCargoSync`: the
// RightKit broker on a managed host, never a bare-cargo fallback on a managed Mac).
//
//   legal: { cargoNotices: { output: "legal/THIRD_PARTY_CARGO_LICENSES.html", template: "legal/about.hbs",
//                            config?: "legal/about.toml", manifestPath?: "src-tauri/Cargo.toml", features?: ["a"], allFeatures?: true } }
//
// `output` and `template` are app-relative and may not leave the app. `manifestPath` and `config` (also per entry) are app-relative but may
// climb out of the app (`../../engine/Cargo.toml`) while they stay inside the repository root. Several workspaces merge into one appendix with
// `manifests: [{ label, manifestPath, config? }]` (instead of `manifestPath`): one `cargo about generate` per entry, each body in a section
// named by its label. After a successful run a `Build provenance: <product> <version>; inputs: <Cargo.lock path> sha256=<hex>; generated-by=right-release
// license-notices cargo` line is written before `</body>`; `productName` and `version` default to the config app name and package.json version. The run is `cargo about generate --frozen
// --fail [--config <config>] [--manifest-path <path>] [--all-features | --features a,b] <template>`; cargo-about writes the rendered
// appendix to stdout, which is normalised to LF and one trailing newline so the same lockfile always produces the same bytes. `--fail`
// makes an unaccepted license or an unresolved clarification an error instead of a warning. `--check` writes nothing and exits 1 when
// the file on disk differs.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { CommandSpecError, loadAppConfig, relativeInside } from "./command-spec.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { escapeHtml } from "./npm-license-notices.mjs";

const fail = (message) => { throw new CommandSpecError(message); };
const KEYS = ["output", "template", "config", "manifestPath", "manifests", "features", "allFeatures", "productName", "version"];
const ENTRY_KEYS = ["label", "manifestPath", "config"];

/** The repository root above `start`: the nearest directory holding `.git`, else `start` itself. */
export function findRepositoryRoot(start, exists = existsSync) {
  const original = path.resolve(start);
  let current = original;
  while (true) {
    if (exists(path.join(current, ".git"))) return current;
    const parent = path.dirname(current);
    if (parent === current) return original;
    current = parent;
  }
}

/**
 * A manifest or about-config path: app-relative, but it may climb out of the app (`../../engine/Cargo.toml`) as long as it stays inside the
 * repository root (#1135). Returns the forward-slash path to hand to cargo, which runs in the app root.
 */
function repoBoundPath(value, label, appRoot, repoRoot) {
  if (typeof value !== "string" || !value.trim()) fail(`${label} must be a non-empty relative path`);
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value)) fail(`${label} must be relative to the app root and stay inside the repository: ${value}`);
  const relative = path.relative(repoRoot, path.resolve(appRoot, value));
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) fail(`${label} must stay inside the repository root ${repoRoot}: ${value}`);
  return value.replaceAll("\\", "/");
}

/** Resolve and validate `legal.cargoNotices` plus flags. `appRoot` and `repoRoot` bound manifest and about-config paths (default: the app root for both). */
export function resolveCargoNoticeOptions(config, flags = {}, { appRoot = ".", repoRoot = appRoot } = {}) {
  const declared = config?.legal?.cargoNotices ?? {};
  if (typeof declared !== "object" || declared === null || Array.isArray(declared)) fail("legal.cargoNotices must be an object");
  const unknown = Object.keys(declared).filter((key) => !KEYS.includes(key));
  if (unknown.length) fail(`legal.cargoNotices has unknown key(s): ${unknown.join(", ")} (allowed: ${KEYS.join(", ")})`);
  const output = flags.output ?? declared.output;
  if (output === undefined) fail("no output file: set legal.cargoNotices.output or pass --output");
  relativeInside(output, "legal.cargoNotices.output");
  if (declared.template === undefined) fail("legal.cargoNotices.template (the cargo-about handlebars template) is required");
  relativeInside(declared.template, "legal.cargoNotices.template");
  const bound = (value, label) => repoBoundPath(value, label, appRoot, repoRoot);
  if (declared.config !== undefined) bound(declared.config, "legal.cargoNotices.config");
  if (declared.manifestPath !== undefined) bound(declared.manifestPath, "legal.cargoNotices.manifestPath");
  if (declared.manifests !== undefined) {
    if (declared.manifestPath !== undefined) fail("legal.cargoNotices: give manifestPath or manifests, not both");
    if (!Array.isArray(declared.manifests) || declared.manifests.length === 0) fail("legal.cargoNotices.manifests must be a non-empty list of { label, manifestPath, config? }");
  }
  const labels = new Set();
  const manifests = (declared.manifests ?? [{ label: undefined, manifestPath: declared.manifestPath, config: declared.config }]).map((entry, index) => {
    const where = `legal.cargoNotices.manifests[${index}]`;
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) fail(`${where} must be an object`);
    const entryUnknown = Object.keys(entry).filter((key) => !ENTRY_KEYS.includes(key));
    if (entryUnknown.length) fail(`${where} has unknown key(s): ${entryUnknown.join(", ")} (allowed: ${ENTRY_KEYS.join(", ")})`);
    if (declared.manifests !== undefined) {
      if (typeof entry.label !== "string" || !entry.label.trim()) fail(`${where}.label must be a non-empty string`);
      if (labels.has(entry.label)) fail(`${where}.label ${entry.label} is used twice`);
      labels.add(entry.label);
      bound(entry.manifestPath, `${where}.manifestPath`);
      if (entry.config !== undefined) bound(entry.config, `${where}.config`);
    }
    return { label: entry.label, manifestPath: entry.manifestPath, config: entry.config ?? declared.config };
  });
  const features = declared.features ?? [];
  if (!Array.isArray(features) || features.some((feature) => typeof feature !== "string" || !/^[A-Za-z0-9_/-]+$/.test(feature))) fail("legal.cargoNotices.features must be a list of feature names");
  if (declared.allFeatures !== undefined && typeof declared.allFeatures !== "boolean") fail("legal.cargoNotices.allFeatures must be a boolean");
  if (declared.allFeatures === true && features.length) fail("legal.cargoNotices: give allFeatures or features, not both");
  for (const key of ["productName", "version"]) if (declared[key] !== undefined && (typeof declared[key] !== "string" || !declared[key].trim())) fail(`legal.cargoNotices.${key} must be a non-empty string`);
  return { output, template: declared.template, config: declared.config, manifestPath: declared.manifestPath, manifests, features, allFeatures: declared.allFeatures === true, productName: declared.productName, version: declared.version };
}

/** The `cargo about` argument list (after the managed client adds `cargo`) for one manifest entry. */
export function cargoAboutArgs(options, entry = options.manifests?.[0] ?? { config: options.config, manifestPath: options.manifestPath }) {
  return [
    "about", "generate", "--frozen", "--fail",
    ...(entry.config ? ["--config", entry.config] : []),
    ...(entry.manifestPath ? ["--manifest-path", entry.manifestPath] : []),
    ...(options.allFeatures ? ["--all-features"] : options.features.length ? ["--features", options.features.join(",")] : []),
    options.template,
  ];
}

const normalise = (text) => `${String(text).replace(/\r\n/g, "\n").replace(/\s+$/, "")}\n`;

const BODY = /^([\s\S]*?<body\b[^>]*>)([\s\S]*?)(<\/body>[\s\S]*)$/i;

/** One output, or several merged under their labels: the first output's head and tail, each body in a labelled section. */
export function mergeCargoAboutOutputs(outputs) {
  if (outputs.length === 1 && outputs[0].label === undefined) return outputs[0].text;
  const parts = outputs.map(({ label, text }) => {
    const match = BODY.exec(text);
    return { label, head: match?.[1] ?? "", body: match ? match[2] : text, tail: match?.[3] ?? "" };
  });
  const sections = parts.map(({ label, body }) => `<section data-cargo-manifest="${escapeHtml(label ?? "")}">\n<h2>${escapeHtml(label ?? "")}</h2>\n${body.trim()}\n</section>`);
  return `${parts[0].head}\n${sections.join("\n")}\n${parts[0].tail}`;
}

/** The lock a manifest resolves against: the nearest Cargo.lock at or above the manifest's directory, within the repository. */
function lockFor({ appRoot, repoRoot, manifestPath, exists }) {
  let current = path.dirname(path.resolve(appRoot, manifestPath ?? "Cargo.toml"));
  while (true) {
    const candidate = path.join(current, "Cargo.lock");
    if (exists(candidate)) return candidate;
    if (current === repoRoot || path.dirname(current) === current) return null;
    current = path.dirname(current);
  }
}

/** Insert the Build provenance line before `</body>` (the end when there is none): the lock(s) the appendix was generated from, as CA-14 reads them. */
export function withCargoProvenance(html, { productName, version, locks }) {
  const inputs = locks.map(({ ref, sha256 }) => `${ref} sha256=${sha256}`).join(", ");
  const line = `<p>Build provenance: ${escapeHtml(productName)} ${escapeHtml(version)}; inputs: ${escapeHtml(inputs)}; generated-by=right-release license-notices cargo</p>\n`;
  const close = html.search(/<\/body>/i);
  return normalise(close === -1 ? `${html.replace(/\s+$/, "")}\n${line}` : `${html.slice(0, close)}${line}${html.slice(close)}`);
}

/** Generate (or check) the Cargo appendix for the app at `appRoot`. `run` is the managed cargo runner (injectable). */
export function generateCargoNotices({ appRoot, config, flags = {}, check = false, run = runCargoSync, env = process.env, exists = existsSync, readText = (file) => readFileSync(file, "utf8"), readBytes = (file) => readFileSync(file), writeText = (file, text) => { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, text); } }) {
  const repoRoot = findRepositoryRoot(appRoot, exists);
  const options = resolveCargoNoticeOptions(config, flags, { appRoot: path.resolve(appRoot), repoRoot });
  if (!exists(path.join(appRoot, options.template))) fail(`legal.cargoNotices.template ${options.template} does not exist`);
  for (const entry of options.manifests) {
    const where = entry.label ? ` (${entry.label})` : "";
    if (entry.config !== undefined && !exists(path.resolve(appRoot, entry.config))) fail(`legal.cargoNotices.config ${entry.config}${where} does not exist`);
    if (entry.manifestPath !== undefined && !exists(path.resolve(appRoot, entry.manifestPath))) fail(`legal.cargoNotices.manifestPath ${entry.manifestPath}${where} does not exist`);
  }
  const outputs = options.manifests.map((entry) => {
    const result = run(cargoAboutArgs(options, entry), { cwd: appRoot, env, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
    const where = entry.label ? ` for ${entry.label}` : "";
    if (result?.error) fail(`cargo about could not start${where} (${result.error.message}); install cargo-about on the build host`);
    if (result?.status !== 0) fail(`cargo about${where} exited ${result?.status ?? "without status"}${result?.stderr ? `:\n${String(result.stderr).trim()}` : ""}`);
    const text = normalise(result.stdout ?? "");
    if (text.trim() === "") fail(`cargo about produced no output${where}`);
    return { label: entry.label, text };
  });
  // The lock each manifest resolves against, hashed as bytes, recorded repository-root-relative: a stale appendix no longer matches its lock (CA-14, #1445).
  const locks = [];
  for (const entry of options.manifests) {
    const lockFile = lockFor({ appRoot, repoRoot, manifestPath: entry.manifestPath, exists });
    if (!lockFile) fail(`no Cargo.lock at or above ${entry.manifestPath ?? "the app root"}${entry.label ? ` (${entry.label})` : ""}: the appendix is generated with --frozen from a committed lock`);
    const ref = path.relative(repoRoot, lockFile).replaceAll("\\", "/");
    if (!locks.some((lock) => lock.ref === ref)) locks.push({ ref, sha256: createHash("sha256").update(readBytes(lockFile)).digest("hex") });
  }
  const productName = options.productName ?? config?.productName ?? config?.app;
  let version = options.version;
  if (version === undefined) {
    const packageJson = path.join(appRoot, "package.json");
    if (exists(packageJson)) version = JSON.parse(readText(packageJson)).version;
  }
  if (typeof productName !== "string" || !productName.trim()) fail("no product name: set legal.cargoNotices.productName or the config app name");
  if (typeof version !== "string" || !version.trim()) fail("no product version: set legal.cargoNotices.version or give the app a package.json version");
  const text = withCargoProvenance(mergeCargoAboutOutputs(outputs), { productName, version, locks });
  const target = path.join(appRoot, options.output);
  if (check) {
    const current = exists(target) ? normalise(readText(target)) : null;
    return { target, bytes: Buffer.byteLength(text), upToDate: current === text };
  }
  writeText(target, text);
  return { target, bytes: Buffer.byteLength(text), upToDate: true };
}

export function parseCargoNoticeArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), output: undefined, check: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--config" || arg === "--output") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg.slice(2)] = value;
      i += 1;
    } else if (arg === "--check") opts.check = true;
    else throw new Error(`unknown license-notices argument: ${arg}`);
  }
  return opts;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseCargoNoticeArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  const outcome = generateCargoNotices({ appRoot, config, flags: { output: opts.output }, check: opts.check });
  if (opts.check) {
    if (!outcome.upToDate) {
      console.error(`right-release license-notices: ${outcome.target} is out of date; run right-release license-notices cargo`);
      return 1;
    }
    console.log(`${outcome.target} is up to date`);
    return 0;
  }
  console.log(`wrote ${outcome.target} (${outcome.bytes} bytes)`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release license-notices: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
