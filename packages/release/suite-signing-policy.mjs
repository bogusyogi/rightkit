// The signing policy of a gate-run test or dev build (rightkit#1303, GATE-CONTRACT "One signing identity per product, every build"). Pure: it reads
// nothing but what its caller hands it, so the gate's route decision (rightkit-gate.mjs routePlan, from the commit's own bytes), the `sign` step
// (suite-signing.mjs, from the snapshot) and the tests all judge the same text.
//
//   - The identity is derived, never declared: the app's tauri.conf.json `identifier` is the one id. Mac install.bundleId must equal it, and a
//     dev or test Tauri `--config` that overrides it is refused. The team is DEFAULT_TEAM, the signer subject WINDOWS_SIGNER_SUBJECT.
//   - A suite that builds a product (tauri build/bundle, or a cargo build of the tauri package or a sidecar package) must be named by a later `sign`
//     step; a Windows dev `install.kind: "tauri"` is refused (`tauri-authenticode` is the signed route); `sign-qa-bundle` and RIGHTKIT_DEV_SIGNING
//     (ad hoc) are refused.
//   - These refusals apply on Mac and Dell only (process.platform darwin or win32). Linux builds sign nothing and hosted CI is exempt.
import path from "node:path";
import { createRequire } from "node:module";
import { DEFAULT_TEAM, WINDOWS_SIGNER_SUBJECT } from "./signing-identity.mjs";

export const SIGN_TEAM = DEFAULT_TEAM;
export const SIGN_SUBJECT = WINDOWS_SIGNER_SUBJECT;
/** The environment variable a successful `sign` step sets for the suites after it: the signed copy of the first file. */
export const QA_APP_BINARY_ENV = "RIGHTKIT_QA_APP_BINARY";
/** The environment variable a successful `sign` step sets beside it: the path of the signing evidence file (signed files, sha256 before and after, identity). */
export const QA_SIGNING_EVIDENCE_ENV = "RIGHTKIT_QA_SIGNING_EVIDENCE";
export const SIGN_STEP_KEYS = Object.freeze(["name", "sign"]);
export const WINDOWS_TAURI_KIND = "tauri";
export const WINDOWS_SIGNED_KIND = "tauri-authenticode";

// The package's TOML parser (smol-toml, a dependency of this package), loaded on first use: the installed gate must start, and run its checks, in an app
// with no node_modules, so it is never imported at module load. Where it cannot be loaded, manifests read as unparsed and the policy stays conservative.
let tomlParser;
function parseToml(text) {
  if (tomlParser === undefined) { try { tomlParser = createRequire(import.meta.url)("smol-toml").parse; } catch { tomlParser = null; } }
  if (tomlParser === null) return undefined;
  return tomlParser(text);
}

export class SuiteSigningError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "SuiteSigningError";
    this.code = code;
    Object.assign(this, details);
  }
}
const refuse = (code, message, details) => { throw new SuiteSigningError(code, message, details); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const SIGNING_ID = /^[A-Za-z0-9][A-Za-z0-9.-]{0,254}$/;

/** A relative path with no `..`, no drive, no root: the form a sign file path takes inside a receipt's output directory. Returns it with `/`. */
export function containedRelative(value, label) {
  if (typeof value !== "string" || !value.trim()) refuse("SIGN_STEP_PATH", `${label} must be a non-empty relative path`);
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.startsWith("/") || normalized.split("/").includes("..") || normalized.split("/").includes("")) {
    refuse("SIGN_STEP_PATH", `${label} must be a relative path inside the earlier suite's output directory (no absolute path, no .., no empty segment): ${JSON.stringify(value)}`);
  }
  return normalized;
}

/**
 * Validate one `sign` spec: `{ files: [{ from, path }] }`. `from` names an EARLIER suite of the same list (`earlier` is the set of names declared
 * before this step), `path` is relative to that suite's carried-receipt output directory. Returns the spec with normalized paths.
 */
export function validateSignSpec(sign, label, earlier) {
  if (!isObject(sign)) refuse("SIGN_STEP_SHAPE", `${label} must be an object { files: [{ from, path }] }`);
  const unknown = Object.keys(sign).filter((key) => key !== "files");
  if (unknown.length) refuse("SIGN_STEP_SHAPE", `${label} has unknown key(s): ${unknown.join(", ")} (allowed: files). The identity is derived from the product, never declared here`);
  if (!Array.isArray(sign.files) || sign.files.length === 0) refuse("SIGN_STEP_SHAPE", `${label}.files must be a non-empty list of { from, path }`);
  const seen = new Set();
  const files = sign.files.map((item, index) => {
    const at = `${label}.files[${index}]`;
    if (!isObject(item)) refuse("SIGN_STEP_SHAPE", `${at} must be { from, path }`);
    const extra = Object.keys(item).filter((key) => key !== "from" && key !== "path");
    if (extra.length) refuse("SIGN_STEP_SHAPE", `${at} has unknown key(s): ${extra.join(", ")} (allowed: from, path)`);
    if (typeof item.from !== "string" || !NAME.test(item.from)) refuse("SIGN_STEP_FROM", `${at}.from must name an earlier suite`);
    if (!earlier.has(item.from)) refuse("SIGN_STEP_FROM", `${at}.from ${JSON.stringify(item.from)} is not a suite declared before this step (${[...earlier].join(", ") || "none"}); a sign step signs what an earlier suite built`);
    const rel = containedRelative(item.path, `${at}.path`);
    const key = `${item.from}\0${rel}`;
    if (seen.has(key)) refuse("SIGN_STEP_SHAPE", `${at} repeats ${item.from}:${rel}`);
    seen.add(key);
    return { from: item.from, path: rel };
  });
  return { files };
}

// ---- what a step is -------------------------------------------------------------------------------------------------------------

const base = (program) => path.basename(String(program ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
/** A config word as text: a string, or a typed `{ "$json": value }` template (the raw config a commit holds) as the JSON it expands to. */
const wordText = (word) => (typeof word === "string" ? word : isObject(word) && Object.hasOwn(word, "$json") ? JSON.stringify(word.$json) : JSON.stringify(word));

/** The words a step runs: its program, then its arguments, as text. */
export function stepWords(step) {
  const own = step?.node !== undefined ? ["node", String(step.node)] : step?.cmd !== undefined ? [String(step.cmd)] : [];
  return [...own, ...(Array.isArray(step?.args) ? step.args.map(wordText) : [])];
}

/** A suite entry merged with its runner's command and arguments (enough to read what it runs; journeys and env are not needed here). */
export function suiteStep(entry, runners = {}) {
  const runner = typeof entry.runner === "string" && isObject(runners[entry.runner]) ? runners[entry.runner] : {};
  return {
    cmd: entry.cmd ?? (entry.node === undefined ? runner.cmd : undefined),
    node: entry.node ?? (entry.cmd === undefined ? runner.node : undefined),
    args: [...(runner.args ?? []), ...(entry.args ?? [])],
    env: { ...(runner.env ?? {}), ...(entry.env ?? {}) },
  };
}

const firstWord = (words) => words.find((word) => !word.startsWith("-") && !word.startsWith("+"));

/** The package names and manifest paths a cargo build step names: `-p x`, `--package x`, `--package=x`, `--manifest-path p`. */
function cargoTargets(words) {
  const packages = []; const manifests = []; const excluded = [];
  // Words after `--` belong to the program, not to cargo.
  const end = words.indexOf("--");
  (end === -1 ? words : words.slice(0, end)).forEach((word, i) => {
    if (word === "--exclude") { if (words[i + 1] !== undefined) excluded.push(words[i + 1]); return; }
    if (word.startsWith("--exclude=")) { excluded.push(word.slice("--exclude=".length)); return; }
    if (word === "-p" || word === "--package") packages.push(words[i + 1]);
    else if (word.startsWith("--package=")) packages.push(word.slice("--package=".length));
    else if (word === "--manifest-path") manifests.push(words[i + 1]);
    else if (word.startsWith("--manifest-path=")) manifests.push(word.slice("--manifest-path=".length));
  });
  return { packages: packages.filter(Boolean), manifests: manifests.filter(Boolean), excluded: excluded.filter(Boolean) };
}

/** The `name` of a Cargo manifest's [package] table, or null. A line scan; declared bins and test paths use smol-toml (productTargets). */
export function cargoPackageName(text) {
  let inPackage = false;
  for (const line of String(text).split(/\r?\n/)) {
    const table = /^\s*\[([^\]]*)\]\s*(?:#.*)?$/.exec(line);
    if (table) { inPackage = table[1].trim() === "package"; continue; }
    const named = inPackage ? /^\s*name\s*=\s*"([^"]+)"\s*(?:#.*)?$/.exec(line) : null;
    if (named) return named[1];
  }
  return null;
}

/**
 * `{ packages, manifests, dirs, testsLaunch, testsUnknown }`: what counts as a product for a cargo step (the Tauri package and every sidecar package).
 * `dirs` maps a product package to its directory (null when no manifest path is known). `testsLaunch` holds the product packages whose integration tests
 * (`<dir>/tests/**.rs`) reference `CARGO_BIN_EXE_`, which launches the package's own bin; `testsUnknown` the product packages whose tests cannot be listed
 * (no directory, or no `listFiles`), which cannot be classified. `listFiles(dir)` lists the `.rs` files under an app-relative directory ([] when it has
 * none, null when it cannot be read).
 */
export function productTargets({ config = {}, readFile = () => null, devPlatform = null, listFiles = null } = {}) {
  const manifests = new Set(); const packages = new Set(); const dirs = new Map();
  const testsLaunch = new Set(); const testsUnknown = new Set();
  const launchBins = new Map(); // package -> Set(every bin its test sources launch; "*" when a name is not literal)
  const declaredBins = new Map(); // package -> Set(its declared bins), or null when its manifest cannot be read
  const tauriPackages = new Set();
  const norm = (p) => path.posix.normalize(String(p).replaceAll("\\", "/"));
  const dirOf = (manifest) => path.posix.dirname(norm(manifest));
  const addPackage = (name, manifest) => {
    packages.add(name);
    if (manifest !== undefined && !dirs.has(name)) dirs.set(name, dirOf(manifest));
    else if (!dirs.has(name)) dirs.set(name, null);
  };
  // A package with no declared manifest path is found in the workspace: the root Cargo.toml's `[workspace] members` (globs included, minus `exclude`),
  // the member whose `[package] name` is it. Read from the commit, never by cargo. A package that is missing or in two members stays unresolved.
  const workspaceDirs = () => {
    const found = new Map(); // package name -> Set of directories
    if (typeof listFiles !== "function") return found;
    const globRe = (pattern) => new RegExp(`^${pattern.replace(/[.+^${}()|\\]/g, "\\$&").replaceAll("**", "\u0000").replaceAll("*", "[^/]*").replaceAll("?", "[^/]").replaceAll("\u0000", ".*")}$`);
    const record = (dir, text) => { const name = typeof text === "string" ? cargoPackageName(text) : null; if (name) found.set(name, new Set([...(found.get(name) ?? []), dir])); };
    for (const rootManifest of new Set(["Cargo.toml", ...[...tauriManifests].map(norm)])) {
      const rootText = readFile(rootManifest);
      if (typeof rootText !== "string") continue;
      const rootDir = dirOf(rootManifest);
      const under = (rel) => path.posix.normalize(rootDir === "." ? rel : `${rootDir}/${rel}`);
      record(rootDir, rootText);
      const start = rootText.search(/^\s*\[workspace\]/m);
      if (start < 0) continue;
      const section = rootText.slice(start).replace(/^\s*\[workspace\]/, "").split(/^\s*\[(?!workspace)/m)[0];
      const list = (key) => { const m = new RegExp(`\\b${key}\\s*=\\s*\\[([\\s\\S]*?)\\]`).exec(section); return m ? [...m[1].matchAll(/"([^"]+)"/g)].map((item) => item[1]) : []; };
      const excluded = list("exclude").map((item) => globRe(under(item)));
      for (const member of list("members")) {
        const pattern = under(member);
        if (!/[*?]/.test(pattern)) { if (!excluded.some((re) => re.test(pattern))) record(pattern, readFile(`${pattern}/Cargo.toml`)); continue; }
        const prefix = pattern.split("/").slice(0, pattern.split("/").findIndex((part) => /[*?]/.test(part))).join("/");
        const listed = listFiles(prefix || ".");
        if (!Array.isArray(listed)) continue;
        const re = globRe(pattern);
        for (const file of listed) {
          if (path.posix.basename(file) !== "Cargo.toml") continue;
          const dir = path.posix.dirname(file);
          if (re.test(dir) && !excluded.some((ex) => ex.test(dir))) record(dir, readFile(file));
        }
      }
    }
    return found;
  };
  const finish = () => {
    if ([...packages].some((name) => (dirs.get(name) ?? null) === null)) {
      const located = workspaceDirs();
      for (const name of packages) {
        if ((dirs.get(name) ?? null) !== null) continue;
        const hits = located.get(name);
        if (hits && hits.size === 1) dirs.set(name, [...hits][0]);
      }
    }
    for (const name of packages) {
      const dir = dirs.get(name) ?? null;
      const at = (rel) => (dir === "." ? rel : `${dir}/${rel}`);
      const files = dir !== null && typeof listFiles === "function" ? listFiles(at("tests")) : null;
      if (!Array.isArray(files)) { testsUnknown.add(name); continue; }
      // The manifest, read once with the package's TOML parser: its [[test]] paths are test sources too, and its [[bin]] tables declare bins.
      const manifestText = readFile(at("Cargo.toml"));
      let manifest = null;
      if (typeof manifestText === "string") {
        try { manifest = parseToml(manifestText) ?? null; } catch { testsUnknown.add(name); continue; }
        // No parser available: [[test]] paths cannot be listed, so a manifest that declares any cannot be classified.
        if (manifest === null && /\[\[\s*test\s*\]\]|\btest\s*=\s*\[/.test(manifestText)) { testsUnknown.add(name); continue; }
      }
      const sources = new Set(files.filter((item) => /\.rs$/.test(item)));
      for (const entry of Array.isArray(manifest?.test) ? manifest.test : []) if (typeof entry?.path === "string") sources.add(norm(at(entry.path)));
      // Every CARGO_BIN_EXE_<bin> in ANY test source of the package (tests/** and [[test]] paths) counts for every cargo test of it: no narrowing by target.
      const bins = new Set();
      for (const file of sources) {
        const text = readFile(file);
        if (typeof text !== "string") { testsUnknown.add(name); continue; }
        if (!/CARGO_BIN_EXE_/.test(text)) continue;
        testsLaunch.add(name);
        const named = [...text.matchAll(/CARGO_BIN_EXE_([A-Za-z0-9_-]+)/g)].map((m) => m[1]);
        for (const bin of named.length ? named : ["*"]) bins.add(bin);
      }
      if (bins.size) launchBins.set(name, bins);
      // The bins the package declares: [[bin]] names (any TOML form), src/main.rs (the package's own name) and src/bin/*. A "*" launch needs all signed.
      if (manifest === null) { declaredBins.set(name, null); continue; }
      const declared = new Set();
      for (const entry of Array.isArray(manifest.bin) ? manifest.bin : []) if (typeof entry?.name === "string") declared.add(entry.name);
      if (typeof readFile(at("src/main.rs")) === "string") declared.add(typeof manifest.package?.name === "string" ? manifest.package.name : name);
      const srcBin = listFiles(at("src/bin"));
      if (Array.isArray(srcBin)) for (const file of srcBin) { const rel = file.slice(at("src/bin").length + 1).split("/"); if (/\.rs$/.test(file) && (rel.length === 1 || rel[1] === "main.rs")) declared.add(rel[0].replace(/\.rs$/, "")); }
      declaredBins.set(name, declared);
    }
    return { packages, manifests, dirs, testsLaunch, testsUnknown, launchBins, declaredBins, tauriPackages };
  };
  const installs = Object.entries(config?.development?.targets ?? {}).filter(([name]) => devPlatform === null || name === devPlatform).map(([, target]) => target?.install).filter(isObject);
  const tauriManifests = new Set([...installs.map((install) => install.manifestPath).filter((p) => typeof p === "string"), "src-tauri/Cargo.toml"]);
  for (const manifest of tauriManifests) {
    manifests.add(norm(manifest));
    const text = readFile(manifest);
    const name = typeof text === "string" ? cargoPackageName(text) : null;
    if (name) { addPackage(name, manifest); tauriPackages.add(name); }
  }
  const sidecars = Array.isArray(config?.sidecars?.sidecars) ? config.sidecars.sidecars : Array.isArray(config?.sidecars) ? config.sidecars : [];
  for (const sidecar of sidecars) {
    if (!isObject(sidecar)) continue;
    const hasManifest = typeof sidecar.manifestPath === "string";
    if (hasManifest) manifests.add(norm(sidecar.manifestPath));
    if (typeof sidecar.package === "string") addPackage(sidecar.package, hasManifest ? sidecar.manifestPath : undefined);
    else if (hasManifest) {
      const text = readFile(sidecar.manifestPath);
      const name = typeof text === "string" ? cargoPackageName(text) : null;
      if (name) addPackage(name, sidecar.manifestPath);
    }
  }
  return finish();
}

const CARGO_BUILD_VERBS = new Set(["build", "b", "rustc", "install", "run", "r"]);
const CARGO_TEST_VERBS = new Set(["test", "t", "bench"]);
/** The target-selection flags of a cargo step (before any `--`). */
/** The values of a repeatable `--flag value` / `--flag=value` option of a step (before any `--`). */
function flagValues(words, flag) {
  const end = words.indexOf("--");
  const own = end < 0 ? words : words.slice(0, end);
  const values = [];
  own.forEach((word, i) => { if (word === flag) { if (own[i + 1] !== undefined) values.push(own[i + 1]); } else if (word.startsWith(`${flag}=`)) values.push(word.slice(flag.length + 1)); });
  return values;
}
function cargoSelectors(words) {
  const end = words.indexOf("--");
  return new Set((end < 0 ? words : words.slice(0, end)).map((word) => word.replace(/=.*$/, "")).filter((word) => ["--lib", "--bin", "--bins", "--test", "--tests", "--example", "--examples", "--bench", "--benches", "--all-targets", "--doc"].includes(word)));
}

/**
 * Classify the executables a step produces, by artifact rather than by verb (rightkit#1303, Approver's Muse ruling). `{ product, launches, unknown }`:
 *   - product: a Tauri `build`/`bundle`; a cargo `build`/`run`/`install`/`rustc` that produces a bin of a product package (no selector, `--bin`,
 *     `--bins`, `--all-targets`): the bin must be signed by a LATER sign step. `launches` (also set by a cargo `run`): the step runs a product bin,
 *     as does a cargo `test`/`bench` that builds the integration tests of a product package whose tests reference `CARGO_BIN_EXE_`: a sign step
 *     must come EARLIER. A cargo step naming no package covers every product package.
 *   - harness (neither flag set): in-place Rust test binaries (`--lib`, `--bins` unit tests, `--tests` of a package that never launches its bin),
 *     `--lib`/`--test`/`--tests` builds and an explicitly selected `--example` or `--bench`; they need no product signing.
 *   - unknown: a product package's tests that cannot be read (no directory known, or no way to list them). Refused before anything runs.
 * Only a cargo `build`-like or `test`-like verb produces an executable; `check`, `clippy`, `doc`, `metadata` and the rest never do.
 */
export function classifyStepArtifacts(step, targets = { packages: new Set(), manifests: new Set() }) {
  // `packages`: the product packages the step builds or launches (a Tauri build builds only the shell's own package); `buildsAny`: a bare default build
  // with no product known, which may produce any product bin; `launchBins`: package -> Set of the bin names a launching step runs ("*" when the name
  // is not literal); `layout`: { target, dir }, the --target triple (null: none) and the profile directory the step's outputs land in.
  const out = { product: false, launches: false, unknown: [], packages: new Set(), buildsAny: false, launchBins: new Map(), layout: null };
  const words = stepWords(step);
  if (!words.length) return out;
  const program = base(words[0]);
  const rest = words.slice(1);
  const isTauriWord = (word) => word === "tauri" || base(word) === "cargo-tauri" || /(^|\/)tauri\.js$/.test(String(word).replaceAll("\\", "/"));
  const tauriAt = rest.findIndex(isTauriWord);
  if (program === "cargo-tauri" || tauriAt >= 0) {
    const after = program === "cargo-tauri" ? rest : rest.slice(tauriAt + 1);
    const command = firstWord(after);
    if (command === "build" || command === "bundle") return { ...out, product: true, packages: new Set(targets.tauriPackages ?? []), layout: stepLayout(after, { tauri: true }) };
  }
  const cargoWords = program === "rustright" && base(rest[0]) === "cargo" ? rest.slice(1) : program === "cargo" ? rest : null;
  if (!cargoWords) return out;
  const command = firstWord(cargoWords);
  out.layout = stepLayout(cargoWords, { command });
  const building = CARGO_BUILD_VERBS.has(command);
  const testing = CARGO_TEST_VERBS.has(command);
  if (!building && !testing) return out;
  const named = cargoTargets(cargoWords);
  const norm = (p) => path.posix.normalize(String(p).replaceAll("\\", "/"));
  const all = !named.packages.length && !named.manifests.length;
  const selected = new Set();
  if (all) for (const name of targets.packages) selected.add(name);
  else {
    for (const name of named.packages) if (targets.packages.has(name)) selected.add(name);
    for (const manifest of named.manifests) {
      if (!targets.manifests.has(norm(manifest))) continue;
      const hit = [...(targets.dirs ?? new Map())].find(([, dir]) => dir !== null && norm(manifest) === (dir === "." ? "Cargo.toml" : `${dir}/Cargo.toml`));
      if (hit) selected.add(hit[0]); else out.unknown.push(`${manifest} (a product manifest whose package is not known)`);
    }
  }
  // `--exclude <pkg>` (and `--exclude=<pkg>`, repeated) removes a package from a workspace-wide step.
  for (const name of named.excluded) selected.delete(name);
  const selectors = cargoSelectors(cargoWords);
  const none = selectors.size === 0;
  if (building) {
    // A bin is produced with no selector, `--bin`, `--bins` or `--all-targets`; `--example`, `--lib`, `--test(s)` and `--bench(es)` alone are harnesses.
    const bins = none || selectors.has("--bin") || selectors.has("--bins") || selectors.has("--all-targets");
    // No package named and no product known (a bare default): the workspace builds its default members, which include the product.
    if (bins && (selected.size > 0 || (all && targets.packages.size === 0))) {
      out.product = true;
      for (const name of selected) out.packages.add(name);
      if (selected.size === 0) out.buildsAny = true;
      if (command === "run" || command === "r") {
        out.launches = true;
        // `cargo run` starts `--bin <x>`, else the package's default bin, which carries the package's name.
        const named = flagValues(cargoWords, "--bin");
        for (const name of selected) out.launchBins.set(name, new Set(named.length ? named : [name]));
      }
    }
    return out;
  }
  const integration = none || selectors.has("--test") || selectors.has("--tests") || selectors.has("--all-targets");
  if (!integration) return out;
  for (const name of selected) {
    if (targets.testsUnknown?.has(name)) out.unknown.push(`${name}'s integration tests cannot be read, so it cannot be shown they never launch its bin`);
    else if (targets.testsLaunch?.has(name)) {
      out.launches = true;
      out.packages.add(name);
      // Every bin the package's test sources launch, whatever `--test` selects (no narrowing).
      const bins = targets.launchBins?.get(name);
      out.launchBins.set(name, bins?.size ? new Set(bins) : new Set(["*"]));
    }
  }
  return out;
}

/**
 * Where a step's outputs land under the build's target directory: `--target` (null when none: the build then writes `<profile>/<bin>`, with no triple
 * directory) and the profile directory (debug by default, `release` for --release, the name for --profile X with dev and test mapping to debug and bench
 * to release; a Tauri build is release unless --debug).
 */
function stepLayout(words, { command, tauri = false } = {}) {
  const end = words.indexOf("--");
  const own = end < 0 ? words : words.slice(0, end);
  const target = flagValues(words, "--target")[0] ?? (tauri ? flagValues(words, "-t")[0] : undefined) ?? null;
  let profile;
  if (tauri) profile = own.includes("--debug") ? "debug" : "release";
  else profile = flagValues(words, "--profile")[0] ?? (own.includes("--release") || own.includes("-r") ? "release" : command === "bench" ? "bench" : "dev");
  const dir = profile === "dev" || profile === "test" ? "debug" : profile === "bench" ? "release" : profile;
  return { target, dir };
}

/** True when a sign file path is exactly where a step with `layout` leaves `bin`: `<triple>/<profile>/<bin>[.exe]` for a --target build, `<profile>/<bin>[.exe]` otherwise. */
function signPathNames(file, layout, bin) {
  const segments = String(file).replaceAll("\\", "/").split("/");
  const expected = layout.target ? [layout.target, layout.dir] : [layout.dir];
  if (segments.length !== expected.length + 1) return false;
  return expected.every((segment, i) => segments[i] === segment) && segments[segments.length - 1].replace(/\.exe$/i, "") === bin;
}

/**
 * Launch binding (rightkit#1303): the earlier sign steps that satisfy a launching suite must name, in `sign.files`, each executable it launches. `launch` is
 * the launching step's classification. A sign file covers a bin when `classOf(from)` (the classification of the earlier suite it names) is a build of that
 * package (a Tauri build builds only the shell's package), that build's output layout equals the launch's (the same explicit --target, or neither; the same profile),
 * and the path is exactly where that layout leaves the bin. `signs` is `[{ files: [{ from, path }] }]`, the sign steps before the launch. A bin whose name is
 * not literal ("*") needs every bin the package declares covered (`declaredBins(pkg)`; null when they cannot be read, which is never covered).
 * Returns the launched bins no sign file covers (`"pkg:bin"`), empty when every one is covered.
 */
export function uncoveredLaunchBins(launch, signs, classOf, declaredBins = () => null) {
  const missing = [];
  // Exactly the same layout: cargo writes a --target build under <triple>/ and a build with none under <profile>/ alone, so a host triple never stands in for a missing --target.
  const same = (x, y) => x.target === y.target && x.dir === y.dir;
  const covers = (pkg, bin) => signs.some((sign) => sign.files.some((file) => {
    const built = classOf(file.from);
    return Boolean(built) && built.product && (built.buildsAny || built.packages.has(pkg)) && built.layout && launch.layout && same(built.layout, launch.layout) && signPathNames(file.path, built.layout, bin);
  }));
  for (const [pkg, bins] of launch.launchBins) {
    for (const bin of bins) {
      if (bin !== "*") { if (!covers(pkg, bin)) missing.push(`${pkg}:${bin}`); continue; }
      const declared = declaredBins(pkg);
      if (!declared || declared.size === 0) { missing.push(`${pkg}:<a bin of ${pkg}: its declared bins cannot be read>`); continue; }
      for (const each of declared) if (!covers(pkg, each)) missing.push(`${pkg}:${each} (the launched bin's name is not literal, so every bin of ${pkg} must be signed)`);
    }
  }
  return missing;
}

/**
 * Whether a step builds or launches a product, by artifact (see `classifyStepArtifacts`). A step whose artifacts cannot be classified counts as one
 * here (the policy refuses it separately, before it runs).
 */
export function isProductBuildStep(step, targets = { packages: new Set(), manifests: new Set() }) {
  const found = classifyStepArtifacts(step, targets);
  return found.product || found.launches || found.unknown.length > 0;
}

/** Why a step asks for ad hoc signing (the QA bundle signer, or the ad hoc opt-in), or null. */
export function adHocReason(step) {
  const words = stepWords(step);
  const bad = words.find((word) => /sign-qa-bundle/.test(word));
  if (bad !== undefined) return `${JSON.stringify(bad)} runs the ad hoc QA bundle signer (sign-qa-bundle)`;
  const named = words.find((word) => /RIGHTKIT_DEV_SIGNING/.test(word));
  if (named !== undefined) return `${JSON.stringify(named)} names RIGHTKIT_DEV_SIGNING`;
  if (isObject(step?.env) && Object.keys(step.env).some((key) => key === "RIGHTKIT_DEV_SIGNING")) return "its env sets RIGHTKIT_DEV_SIGNING (ad hoc signing)";
  return null;
}

/** The Tauri `--config`/`-c` values of a step (inline JSON text or a path). */
export function tauriConfigValues(step) {
  const words = stepWords(step);
  const values = [];
  words.forEach((word, i) => {
    if (word === "--config" || word === "-c") { if (words[i + 1] !== undefined) values.push(words[i + 1]); }
    else if (word.startsWith("--config=")) values.push(word.slice("--config=".length));
  });
  return values;
}

/** True when a parsed Tauri config (or merge patch) sets `identifier`. */
const setsIdentifier = (value) => isObject(value) && Object.hasOwn(value, "identifier");

/** Why a step's Tauri `--config` overrides the identifier, or null. `readFile(path)` reads an app-relative file's text (null when it cannot). */
export function identifierOverrideReason(step, readFile = () => null) {
  for (const value of tauriConfigValues(step)) {
    const text = String(value).trim();
    let parsed;
    if (text.startsWith("{")) {
      try { parsed = JSON.parse(text); } catch { return `a --config value is JSON that does not parse, so it cannot be shown not to set the identifier: ${text.slice(0, 60)}`; }
    } else {
      const body = readFile(text);
      if (typeof body !== "string") return `the --config file ${JSON.stringify(text)} cannot be read, so it cannot be shown not to set the identifier`;
      try { parsed = JSON.parse(body); } catch { return `the --config file ${JSON.stringify(text)} is not JSON, so it cannot be shown not to set the identifier`; }
    }
    if (setsIdentifier(parsed)) return `a --config sets the bundle identifier (${JSON.stringify(parsed.identifier)}); a dev or test build carries the product's one identifier`;
  }
  return null;
}

// ---- the product's one identifier -----------------------------------------------------------------------------------------------

/**
 * The product's bundle identifier: `identifier` of tauri.conf.json beside the Tauri manifest. A platform file (tauri.macos.conf.json,
 * tauri.windows.conf.json) that sets a different one is refused. `readFile(path)` reads an app-relative file's text (null when absent).
 */
export function productIdentifier(options) { return productIdentity(options).identifier; }

/** The identifier plus the product's declared main binary name (tauri.conf.json `mainBinaryName`, or null when it declares none). */
export function productIdentity({ readFile, manifestPath = "src-tauri/Cargo.toml" }) {
  const dir = path.posix.dirname(String(manifestPath).replaceAll("\\", "/"));
  const at = (name) => (dir === "." ? name : `${dir}/${name}`);
  const text = readFile(at("tauri.conf.json"));
  if (typeof text !== "string") refuse("SIGN_IDENTIFIER_UNREADABLE", `${at("tauri.conf.json")} cannot be read: the product's identifier is derived from it`);
  let identifier; let mainBinaryName = null;
  try { const parsed = JSON.parse(text); identifier = parsed.identifier; if (typeof parsed.mainBinaryName === "string" && parsed.mainBinaryName.trim()) mainBinaryName = parsed.mainBinaryName.trim(); } catch { refuse("SIGN_IDENTIFIER_UNREADABLE", `${at("tauri.conf.json")} is not JSON`); }
  if (typeof identifier !== "string" || !SIGNING_ID.test(identifier)) refuse("SIGN_IDENTIFIER_UNREADABLE", `${at("tauri.conf.json")} has no usable identifier (letters, digits, '.' and '-'): ${JSON.stringify(identifier)}`);
  for (const name of ["tauri.macos.conf.json", "tauri.windows.conf.json", "tauri.linux.conf.json"]) {
    const platform = readFile(at(name));
    if (typeof platform !== "string") continue;
    let other;
    try { other = JSON.parse(platform)?.identifier; } catch { refuse("SIGN_IDENTIFIER_UNREADABLE", `${at(name)} is not JSON`); }
    if (other !== undefined && other !== identifier) refuse("SIGN_IDENTIFIER_OVERRIDE", `${at(name)} sets identifier ${JSON.stringify(other)}, not the product's ${JSON.stringify(identifier)}`);
  }
  return { identifier, mainBinaryName };
}

/** The mac dev install must name the same id: `install.bundleId`, when set, equals the Tauri identifier. */
export function assertBundleIdMatches(config, identifier) {
  const bundleId = config?.development?.targets?.mac?.install?.bundleId;
  if (bundleId !== undefined && bundleId !== identifier) refuse("SIGN_IDENTIFIER_MISMATCH", `development.targets.mac.install.bundleId ${JSON.stringify(bundleId)} differs from the tauri.conf.json identifier ${JSON.stringify(identifier)}; one id per product, derived from tauri.conf.json`);
}

// ---- the gate's refusals --------------------------------------------------------------------------------------------------------

/** The codes of the gate's signing refusals (the route probe lists them beside its own). */
export const SIGNING_REFUSAL_CODES = Object.freeze(["RK_SUITE_UNSIGNED_BUILD", "RK_SUITE_ADHOC_SIGNING", "RK_SUITE_IDENTIFIER_OVERRIDE", "RK_SUITE_IDENTIFIER_MISMATCH", "RK_DEV_WIN_UNSIGNED_INSTALL", "RK_DEV_UNSIGNED_INSTALL", "RK_SUITE_UNCLASSIFIED_ARTIFACT", "RK_SUITE_LAUNCH_UNBOUND"]);

/** Hosts these refusals apply to: the Mac and the Dell. Linux and hosted CI are exempt. */
export const signingRefusalHost = (platform, env = {}) => (platform === "darwin" || platform === "win32") && !(env.GITHUB_ACTIONS === "true" && env.CI === "true" && env.RUNNER_ENVIRONMENT === "github-hosted");

/**
 * Every signing refusal for an app's config on this host, `[{ code, message }]` (empty when the host is exempt or the config is clean).
 * `to` is the stage (`dev`, `test`); `devPlatform` the `--platform` of a dev run (default: the host's); `readFile(rel)` reads an app-relative file of the
 * commit (text, or null).
 */
export function suiteSigningRefusals({ config, platform, env = {}, to, devPlatform = null, buildOnly = false, readFile = () => null, listFiles = null }) {
  if (!signingRefusalHost(platform, env) || !isObject(config)) return [];
  const out = [];
  const add = (code, message) => out.push({ code, message });
  const hostDev = platform === "win32" ? "win" : "mac";
  const dev = devPlatform ?? hostDev;
  const targets = productTargets({ config, readFile, devPlatform: null, listFiles });
  const runners = isObject(config.tests?.runners) ? config.tests.runners : {};
  const suites = Array.isArray(config.tests?.suites) ? config.tests.suites.filter(isObject) : [];
  // Suites run by `right-release test`; development builds run by `right-release dev`.
  const judged = to === "dev" ? [] : suites;
  const signs = [];
  const classified = new Map(); // suite name -> its artifacts
  judged.forEach((entry, index) => {
    if (entry.sign !== undefined) {
      const files = Array.isArray(entry.sign?.files) ? entry.sign.files.filter(isObject) : [];
      signs.push({ index, from: new Set(files.map((file) => file.from)), files });
    } else classified.set(entry.name, { index, found: classifyStepArtifacts(suiteStep(entry, runners), targets) });
  });
  judged.forEach((entry, index) => {
    if (entry.sign !== undefined) return;
    const step = suiteStep(entry, runners);
    const label = `tests.suites[${index}] (${entry.name ?? "?"})`;
    const adhoc = adHocReason(step);
    if (adhoc) add("RK_SUITE_ADHOC_SIGNING", `${label}: ${adhoc}; a ${platform === "darwin" ? "Mac" : "Dell"} build is signed with the product's one identity (Developer ID, team ${SIGN_TEAM}; Azure Artifact Signing ${SIGN_SUBJECT}), never ad hoc`);
    const artifacts = classifyStepArtifacts(step, targets);
    for (const why of artifacts.unknown) add("RK_SUITE_UNCLASSIFIED_ARTIFACT", `${label}: ${why}; an artifact that cannot be classified as product or harness is refused before it runs`);
    if (artifacts.launches) {
      const earlier = signs.filter((sign) => sign.index < index);
      if (!earlier.length) add("RK_SUITE_UNSIGNED_BUILD", `${label} launches a product bin (cargo run, or an integration test that uses CARGO_BIN_EXE_) and no earlier sign step signs one; products are signed before any launch`);
      else {
        const missing = uncoveredLaunchBins(artifacts, earlier, (from) => { const built = classified.get(from); return built && built.index < index ? built.found : null; }, (pkg) => targets.declaredBins.get(pkg) ?? null);
        if (missing.length) add("RK_SUITE_LAUNCH_UNBOUND", `${label} launches ${missing.join(", ")} and no earlier sign step names it: a sign.files entry { from: <the suite that builds it>, path: <triple>/<profile>/<bin>[.exe] } must cover the executable the suite launches`);
      }
    }
    if (artifacts.product || artifacts.launches) {
      const override = identifierOverrideReason(step, readFile);
      if (override) add("RK_SUITE_IDENTIFIER_OVERRIDE", `${label}: ${override}`);
    }
    if (artifacts.product) {
      if (!signs.some((sign) => sign.index > index && sign.from.has(entry.name))) add("RK_SUITE_UNSIGNED_BUILD", `${label} builds a product and no later { sign: { files: [{ from: ${JSON.stringify(entry.name)}, path }] } } step names it; a ${platform === "darwin" ? "Mac" : "Dell"} test build is signed with the product's one identity before anything runs it`);
    }
  });
  // Development targets (`right-release dev`).
  const target = config.development?.targets?.[dev];
  if (target && (to === "dev" || to === undefined || to === null)) {
    const install = isObject(target.install) ? target.install : {};
    (Array.isArray(target.build) ? target.build : []).forEach((step, index) => {
      if (!isObject(step)) return;
      const label = `development.targets.${dev}.build[${index}]`;
      const adhoc = adHocReason(step);
      if (adhoc) add("RK_SUITE_ADHOC_SIGNING", `${label}: ${adhoc}`);
      const override = isProductBuildStep(step, targets) ? identifierOverrideReason(step, readFile) : null;
      if (override) add("RK_SUITE_IDENTIFIER_OVERRIDE", `${label}: ${override}`);
    });
    // Only a signed install route leaves a signed binary: `command` and `portable` installs, no install at all, `--build-only` and `stage` entries
    // all leave (or hand QA) the raw, unsigned build (rightkit#1303 M2).
    if (dev === "mac" || dev === "win") {
      if (!install.kind || install.kind === "command" || install.kind === "portable") add("RK_DEV_UNSIGNED_INSTALL", `development.targets.${dev}.install ${install.kind ? `kind ${JSON.stringify(install.kind)}` : "is missing"}: it leaves the product binary unsigned; use a signed install kind (${dev === "win" ? `"${WINDOWS_SIGNED_KIND}"` : '"tauri-devid"'})`);
      if (buildOnly) add("RK_DEV_UNSIGNED_INSTALL", `--build-only on development.targets.${dev} builds the product and signs nothing; a ${platform === "darwin" ? "Mac" : "Dell"} dev build is signed with the product's one identity`);
      if (Array.isArray(target.stage) && target.stage.length) add("RK_DEV_UNSIGNED_INSTALL", `development.targets.${dev}.stage copies the raw, unsigned build to the staged path QA runs from; a ${platform === "darwin" ? "Mac" : "Dell"} dev build stages no product binary`);
    }
    if (install.signing !== undefined) add("RK_SUITE_ADHOC_SIGNING", `development.targets.${dev}.install.signing ${JSON.stringify(install.signing)} asks for ad hoc signing; a ${platform === "darwin" ? "Mac" : "Dell"} dev build is signed with the product's one identity`);
    if (dev === "win" && install.kind === WINDOWS_TAURI_KIND) add("RK_DEV_WIN_UNSIGNED_INSTALL", `development.targets.win.install.kind "tauri" installs an unsigned installer; use "${WINDOWS_SIGNED_KIND}" (build --no-bundle, sign the exe, bundle, sign the installer, install)`);
    if (dev === "mac" && install.bundleId !== undefined) {
      try { assertBundleIdMatches(config, productIdentifier({ readFile, manifestPath: install.manifestPath ?? "src-tauri/Cargo.toml" })); }
      catch (error) { if (!(error instanceof SuiteSigningError)) throw error; add("RK_SUITE_IDENTIFIER_MISMATCH", error.message); }
    }
  }
  return out;
}

/**
 * The policy applied to the RESOLVED plan of a `right-release test` run (rightkit#1303 H1), which is what actually executes: runner mode (`--runner`),
 * a selected subset of suites and the `-- <args>` passthrough are all judged, not only `config.tests.suites`. For every non-sign item, its merged
 * step plus `extraArgs` is checked for ad hoc signing and an identifier override; a step that builds a product needs a sign item LATER in the same
 * plan whose `from` names it (by name or group). Runner mode has no sign item, so a runner invocation that builds a product is refused.
 * `plan` is `planTests`' result: `[{ name, group, step, extraArgs, sign? }]`.
 */
export function planSigningRefusals({ config, plan, platform, env = {}, readFile = () => null, listFiles = null }) {
  if (!signingRefusalHost(platform, env) || !Array.isArray(plan)) return [];
  const out = [];
  const add = (code, message) => out.push({ code, message });
  const host = platform === "darwin" ? "Mac" : "Dell";
  const targets = productTargets({ config: isObject(config) ? config : {}, readFile, devPlatform: null, listFiles });
  plan.forEach((item, index) => {
    if (item.sign !== undefined && item.sign !== null) return;
    const step = { ...item.step, args: [...(Array.isArray(item.step?.args) ? item.step.args : []), ...(Array.isArray(item.extraArgs) ? item.extraArgs : [])] };
    const label = `the planned step ${index} (${item.name ?? "?"})`;
    const adhoc = adHocReason(step);
    if (adhoc) add("RK_SUITE_ADHOC_SIGNING", `${label}: ${adhoc}; a ${host} build is signed with the product's one identity (Developer ID, team ${SIGN_TEAM}; Azure Artifact Signing ${SIGN_SUBJECT}), never ad hoc`);
    const artifacts = classifyStepArtifacts(step, targets);
    for (const why of artifacts.unknown) add("RK_SUITE_UNCLASSIFIED_ARTIFACT", `${label}: ${why}; an artifact that cannot be classified as product or harness is refused before it runs`);
    if (!artifacts.product && !artifacts.launches) return;
    const override = identifierOverrideReason(step, readFile);
    if (override) add("RK_SUITE_IDENTIFIER_OVERRIDE", `${label}: ${override}`);
    if (artifacts.launches) {
      const earlier = plan.slice(0, index).filter((item2) => item2.sign && Array.isArray(item2.sign.files)).map((item2) => ({ files: item2.sign.files.filter(isObject) }));
      if (!plan.slice(0, index).some((item2) => item2.sign)) add("RK_SUITE_UNSIGNED_BUILD", `${label} launches a product bin (cargo run, or an integration test that uses CARGO_BIN_EXE_) and no earlier sign step of this plan signs one; products are signed before any launch`);
      else {
        const classOf = (from) => {
          const built = plan.slice(0, index).find((item2) => !item2.sign && (item2.name === from || item2.group === from));
          return built ? classifyStepArtifacts({ ...built.step, args: [...(Array.isArray(built.step?.args) ? built.step.args : []), ...(Array.isArray(built.extraArgs) ? built.extraArgs : [])] }, targets) : null;
        };
        const missing = uncoveredLaunchBins(artifacts, earlier, classOf, (pkg) => targets.declaredBins.get(pkg) ?? null);
        if (missing.length) add("RK_SUITE_LAUNCH_UNBOUND", `${label} launches ${missing.join(", ")} and no earlier sign step of this plan names it: a sign.files entry { from: <the suite that builds it>, path: <triple>/<profile>/<bin>[.exe] } must cover the executable the suite launches`);
      }
    }
    if (!artifacts.product) return;
    const signed = plan.slice(index + 1).some((later) => later.sign && Array.isArray(later.sign.files) && later.sign.files.some((file) => file.from === item.name || file.from === item.group));
    if (!signed) add("RK_SUITE_UNSIGNED_BUILD", `${label} builds a product and no later sign step of this plan names ${JSON.stringify(item.name)}${item.group && item.group !== item.name ? ` or ${JSON.stringify(item.group)}` : ""}; a ${host} test build is signed with the product's one identity before anything runs it (a runner or passthrough product build cannot be signed: run the planned build and sign suites)`);
  });
  return out;
}

/** Throws SuiteSigningError with the first refusal of `planSigningRefusals`; a no-op off the Mac and the Dell. */
export function assertPlanSigningPolicy({ config, plan, platform = process.platform, env = process.env, readFile = () => null, listFiles = null }) {
  const [first] = planSigningRefusals({ config, plan, platform, env, readFile, listFiles });
  if (first) refuse(first.code, first.message);
}

/**
 * The same policy at run time, on the config a run actually loaded, whatever its format (right-release.config.json or a legacy right-release.config.mjs).
 * The route probe (routePlan) reads only the commit's JSON config, so a .mjs config is enforced here: the suite runner before any suite runs and the
 * dev runner before a target builds. Throws SuiteSigningError with the first refusal's code (RK_SUITE_*); a no-op off the Mac and the Dell.
 */
export function assertSuiteSigningPolicy({ config, platform = process.platform, env = process.env, to, devPlatform = null, buildOnly = false, readFile = () => null, listFiles = null }) {
  const [first] = suiteSigningRefusals({ config, platform, env, to, devPlatform, buildOnly, readFile, listFiles });
  if (first) refuse(first.code, first.message);
}
