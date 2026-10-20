// Exact registry-consumer qualification of sealed crates. Only GitHub CI executes Cargo.
// Bootstrap acquires upstream crates; it is explicitly NOT qualification. Final consumer
// uses a fresh manifest, no path dependency or patch, & Cargo's directory registry source.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { childEnv, loadSealed, sha } from "./upload.mjs";
import { cargoRecipe, nativeFixture, planRuns, executeRuns } from "./consumer-recipes.mjs";
export { assertRegistryManifest } from "./consumer-recipes.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const slash = (s) => s.replaceAll("\\", "/");
const quote = (s) => JSON.stringify(slash(s));
export function archiveTool(platform = process.platform, env = process.env) {
  // Git Bash puts GNU tar first on PATH; it interprets D: as a remote host.
  return platform === "win32" ? path.win32.join(env.SystemRoot || env.SYSTEMROOT || "C:\\Windows", "System32", "tar.exe") : "tar";
}
export function isWithin(parent, candidate) {
  const rel = path.relative(path.resolve(parent), path.resolve(candidate));
  return rel === "" || (!rel.startsWith(`..${path.sep}`) && rel !== ".." && !path.isAbsolute(rel));
}

export function registryLockEvidence(lock, units) {
  const rows = [];
  const packages = lock.split(/^\[\[package\]\]\s*$/m).slice(1);
  const field = (block, key) => block.match(new RegExp(`^${key} = "([^"]+)"$`, "m"))?.[1];
  for (const unit of units) {
    const matches = packages.filter((b) => field(b, "name") === unit.name);
    if (!matches.length) continue; // Sealed batch may include units outside this consumer's closure.
    for (const block of matches) {
      const version = field(block, "version");
      const source = field(block, "source");
      const checksum = field(block, "checksum");
      if (version !== unit.version || source !== "registry+https://github.com/rust-lang/crates.io-index" || checksum !== unit.sha256) throw new Error(`registry lock mismatch for ${unit.name}: version/source/checksum must equal sealed archive`);
      rows.push({ name: unit.name, version, source, checksum, archiveSha256: unit.sha256 });
    }
  }
  return rows;
}

function checksumDirectory(dir, packageSha256) {
  const files = {};
  function walk(current) {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`symlink in registry package: ${full}`);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile()) {
        const rel = slash(path.relative(dir, full));
        if (rel !== ".cargo-checksum.json") files[rel] = sha("sha256", fs.readFileSync(full));
      } else throw new Error(`special file in registry package: ${full}`);
    }
  }
  walk(dir);
  fs.writeFileSync(path.join(dir, ".cargo-checksum.json"), `${JSON.stringify({ files, package: packageSha256 })}\n`);
}

/** Returns runtime evidence; throws before reporting pass on any provenance/build/run failure.
 * packDir holds publish.mjs manifest.json + sealed files. root owns source templates only.
 * reportFile must be outside root. scratchRoot defaults RUNNER_TEMP, never checkout.
 */
export function runRegistryConsumer({ packDir, name = "rightkit-logs", root = ROOT, scratchRoot = process.env.RUNNER_TEMP ?? os.tmpdir(), reportFile, env = process.env, spawn = spawnSync, uploadMetadata, platform = process.platform } = {}) {
  if (env.GITHUB_ACTIONS !== "true") throw new Error("registry consumer execution requires GitHub CI");
  root = fs.realpathSync(root);
  scratchRoot = fs.realpathSync(scratchRoot);
  if (isWithin(root, scratchRoot)) throw new Error("registry scratch must be outside checkout");
  if (reportFile && isWithin(root, path.resolve(reportFile))) throw new Error("registry report must be outside checkout");
  const raw = JSON.parse(fs.readFileSync(path.join(packDir, "manifest.json"), "utf8"));
  const sealed = loadSealed({ packDir, registry: "crates", unitsText: (raw.units ?? []).map((u) => u.name).join(" "), run: { runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT } });
  if (sealed.problems.length) throw new Error(sealed.problems.join("\n"));
  if (sealed.manifest.commit !== env.GITHUB_SHA) throw new Error("sealed commit differs from GitHub checkout commit");
  const units = sealed.manifest.units;
  if (new Set(units.map((u) => u.name)).size !== units.length) throw new Error("duplicate sealed crate names");
  const unit = units.find((u) => u.name === name);
  if (!unit || !/^[a-zA-Z0-9_-]+$/.test(name)) throw new Error(`missing/unsafe sealed crate ${name}`);
  for (const u of units) if (!/^[a-zA-Z0-9_-]+$/.test(u.name) || !/^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$/.test(u.version)) throw new Error("invalid sealed crate identity");
  const recipe = cargoRecipe({ root, unit });
  const template = recipe.directory; const manifest = recipe.manifest;
  const fixture = nativeFixture({ unit, env });
  const scratch = fs.mkdtempSync(path.join(scratchRoot, "rk-registry-consumer-"));
  const consumer = path.join(scratch, "consumer");
  const vendor = path.join(scratch, "registry");
  const bootstrap = path.join(scratch, "bootstrap");
  const commandLog = [];
  const cargoEnv = Object.fromEntries(Object.entries(childEnv(env)).filter(([key]) => !/^(?:GITHUB_|ACTIONS_|RUNNER_)/.test(key)));
  Object.assign(cargoEnv, { GITHUB_ACTIONS: "true", RUNNER_TEMP: env.RUNNER_TEMP ?? scratchRoot, HOME: path.join(scratch, "home"), RUSTUP_HOME: env.RUSTUP_HOME || path.join(env.HOME || env.USERPROFILE || os.homedir(), ".rustup"), CARGO_HOME: path.join(scratch, "cargo-home"), CARGO_TARGET_DIR: path.join(scratch, "target"), CARGO_TERM_COLOR: "never" });
  if (env.RUSTUP_TOOLCHAIN) cargoEnv.RUSTUP_TOOLCHAIN = env.RUSTUP_TOOLCHAIN;
  fs.mkdirSync(cargoEnv.HOME); fs.mkdirSync(cargoEnv.CARGO_HOME);
  const tar = archiveTool(process.platform, env);
  const run = (command, args, cwd = scratch, extraEnv = {}) => {
    const result = spawn(command, args, { cwd, env: { ...cargoEnv, ...extraEnv }, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, timeout: 60 * 60_000 });
    commandLog.push({ command, args, cwd, status: result.status, stdout: String(result.stdout ?? ""), stderr: String(result.stderr ?? "") });
    if (result.error || result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed: ${String(result.stderr || result.error?.message).slice(-3000)}`);
    return result.stdout;
  };
  let report = { schema: 1, kind: "registry-consumer", qualification: "R2a", name, version: unit.version, commit: sealed.manifest.commit, manifestSha256: sealed.manifestSha256, archiveSha256: unit.sha256, runId: String(env.GITHUB_RUN_ID), runAttempt: String(env.GITHUB_RUN_ATTEMPT), manifestRunId: String(sealed.manifest.runId), manifestRunAttempt: String(sealed.manifest.runAttempt), host: platform === "win32" ? "windows" : platform === "darwin" ? "macos" : platform, runnerOS: env.RUNNER_OS ?? null, arch: process.arch, recipe: { ...recipe.evidence, sourceCommit: env.GITHUB_WORKFLOW_SHA || env.GITHUB_SHA }, fixture, verified: false, bootstrap: "path seed only acquires upstream vendor closure; never qualification", commands: commandLog };
  try {
    // Resolve every run's harness-owned env before any Cargo work: a missing prerequisite refuses early.
    const plan = planRuns({ runs: recipe.runs, env, scratchRoot, platform });
    report.runs = plan.map(({ values, implicit, ...entry }) => entry);
    fs.mkdirSync(vendor);
    fs.mkdirSync(path.join(bootstrap, "src"), { recursive: true });
    report.toolchain = { rustc: String(run("rustc", ["--version"])).trim(), cargo: String(run("cargo", ["--version"])).trim() };
    const extracted = new Map();
    for (const u of units) {
      const prefix = `${u.name}-${u.version}`;
      const entries = String(run(tar, ["-tzf", path.resolve(packDir, u.file)]))
        .split(/\r?\n/).filter(Boolean);
      if (!entries.length || entries.some((p) => p.includes("\\") || p.includes(":") || !p.startsWith(`${prefix}/`) || p.split("/").some((s) => s === ".." || s === "."))) throw new Error(`unsafe archive entries: ${u.file}`);
      const listing = run(tar, ["-tvzf", path.resolve(packDir, u.file)]);
      if (String(listing).split(/\r?\n/).filter(Boolean).some((line) => !["-", "d"].includes(line[0]))) throw new Error(`links/special entries in sealed archive: ${u.file}`);
      run(tar, ["-xzf", path.resolve(packDir, u.file), "-C", slash(scratch)]);
      const dir = path.join(scratch, prefix);
      checksumDirectory(dir, u.sha256); // Reject links before bootstrap Cargo reads manifests.
      extracted.set(u.name, dir);
      if (uploadMetadata) {
        // Metadata may create Cargo.lock. Its disposable copy never enters final registry.
        const metadataDir = path.join(scratch, `metadata-${u.name}`); fs.cpSync(dir, metadataDir, { recursive: true });
        const doc = JSON.parse(run("cargo", ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", path.join(metadataDir, "Cargo.toml")], metadataDir));
        const pkg = doc.packages?.find(p => p.name === u.name && p.version === u.version);
        if (!pkg) throw new Error(`${u.name}: packaged Cargo.toml identity mismatch`);
        const readme = pkg.readme ? path.resolve(metadataDir, pkg.readme) : null;
        if (readme && (!isWithin(metadataDir, readme) || !fs.statSync(readme).isFile())) throw new Error("packaged README escapes archive");
        const derived = `${JSON.stringify(uploadMetadata(pkg, readme ? fs.readFileSync(readme, "utf8") : null))}\n`;
        if (derived !== fs.readFileSync(path.join(packDir, u.metaFile), "utf8")) throw new Error(`${u.name}: sealed upload metadata does not match packaged Cargo.toml & README`);
      }
    }
    // Path references exist only in this disposable dependency-fetch seed. cargo vendor excludes
    // those path packages. Final registry directory receives original sealed bytes below.
    const seedManifest = manifest.replace(/^name\s*=\s*"[^"]+"/m, 'name="registry-bootstrap"').replace(recipe.dependency, recipe.dependency.replace(/version\s*=\s*"[^"]+"/, `path=${quote(extracted.get(name))}`));
    const seed = `${seedManifest}\n[patch.crates-io]\n${units.map((u) => `${u.name}={path=${quote(extracted.get(u.name))}}`).join("\n")}\n`;
    fs.writeFileSync(path.join(bootstrap, "Cargo.toml"), seed);
    fs.writeFileSync(path.join(bootstrap, "src", "main.rs"), "fn main() {}\n");
    run("cargo", ["vendor", "--versioned-dirs", "--manifest-path", path.join(bootstrap, "Cargo.toml"), vendor], bootstrap);
    for (const u of units) {
      const destination = path.join(vendor, `${u.name}-${u.version}`);
      fs.rmSync(destination, { recursive: true, force: true });
      fs.cpSync(extracted.get(u.name), destination, { recursive: true });
    }
    fs.cpSync(template, consumer, { recursive: true });
    fs.mkdirSync(path.join(consumer, ".cargo"));
    fs.writeFileSync(path.join(consumer, ".cargo", "config.toml"), `[source.crates-io]\nreplace-with="sealed-directory"\n[source.sealed-directory]\ndirectory=${quote(vendor)}\n[net]\noffline=true\n`);
    run("cargo", ["generate-lockfile", "--offline"], consumer);
    const lockFile = path.join(consumer, "Cargo.lock");
    const lock = fs.readFileSync(lockFile, "utf8");
    const lockEvidence = registryLockEvidence(lock, units);
    if (!lockEvidence.some((r) => r.name === name)) throw new Error("qualified crate absent from registry lock");
    const metadata = JSON.parse(run("cargo", ["metadata", "--format-version", "1", "--locked", "--offline"], consumer));
    const selected = metadata.packages?.filter(p => p.name === name && p.version === unit.version);
    if (selected?.length !== 1 || selected[0].source !== "registry+https://github.com/rust-lang/crates.io-index" || !isWithin(vendor, path.resolve(selected[0].manifest_path))) throw new Error("final consumer did not resolve selected crate from sealed directory registry");
    for (const pkg of metadata.packages ?? []) {
      if (path.resolve(pkg.manifest_path) === path.join(consumer, "Cargo.toml")) { if (pkg.source) throw new Error("final consumer root has unexpected source"); }
      else if (pkg.source !== "registry+https://github.com/rust-lang/crates.io-index" || !isWithin(vendor, path.resolve(pkg.manifest_path))) throw new Error("final consumer has path/git/foreign registry closure");
    }
    const resolved = metadata.resolve?.nodes?.filter(node => node.id === selected[0].id);
    if (resolved?.length !== 1 || !Array.isArray(resolved[0].features)) throw new Error("missing selected crate feature resolution");
    if (recipe.evidence.dependency.features.some(feature => !resolved[0].features.includes(feature))) throw new Error("selected PK feature prescription absent from resolved crate");
    run("cargo", ["build", "--locked", "--offline"], consumer);
    const outputs = executeRuns({ plan, evidence: report.runs, execute: (cargoArgs, values) => run("cargo", cargoArgs, consumer, values) });
    const runtimeOutput = recipe.runs ? plan.map((entry, i) => `[run ${entry.id}] ${report.runs[i].status}`).join("\n") + "\n" + outputs.join("") : outputs[0];
    if (fs.readFileSync(lockFile, "utf8") !== lock) throw new Error("lock changed during qualification");
    for (const [file, digest] of Object.entries(recipe.evidence.files)) if (sha("sha256", fs.readFileSync(path.join(consumer, file))) !== digest) throw new Error(`PK example changed during qualification: ${file}`);
    if (cargoRecipe({ root, unit }).evidence.sha256 !== recipe.evidence.sha256) throw new Error("committed PK example changed during qualification");
    // Re-read sealed bytes too: upload & qualification remain bound to identical archives.
    for (const u of units) if (sha("sha256", fs.readFileSync(path.join(packDir, u.file))) !== u.sha256) throw new Error(`archive changed during qualification: ${u.name}`);
    if (sha("sha256", fs.readFileSync(path.join(packDir, "manifest.json"))) !== sealed.manifestSha256) throw new Error("sealed manifest changed during qualification");
    report = { ...report, verified: true, consumerDirectory: consumer, lockSha256: sha("sha256", lock), lockEvidence, resolvedFeatures: resolved[0].features, finalManifestSha256: sha("sha256", fs.readFileSync(path.join(consumer, "Cargo.toml"))), finalSource: "sealed-directory-registry", build: { locked: true, offline: true, status: 0 }, runtime: { locked: true, offline: true, status: 0 }, runtimeOutput };
    return report;
  } catch (error) {
    report.error = error.message;
    error.report = report;
    throw error;
  } finally {
    if (reportFile) {
      fs.mkdirSync(path.dirname(path.resolve(reportFile)), { recursive: true });
      fs.writeFileSync(reportFile, `${JSON.stringify(report, null, 2)}\n`);
    }
    fs.rmSync(scratch, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [packDir, name, reportFile] = process.argv.slice(2);
  if (!packDir || !name || !reportFile) throw new Error("usage: node scripts/ci/registry-consumer.mjs <sealed-pack-dir> <crate-name> <report-file>");
  runRegistryConsumer({ packDir, name, reportFile });
}
