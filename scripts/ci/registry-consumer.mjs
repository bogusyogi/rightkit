// Exact registry-consumer qualification of sealed crates. Only GitHub CI executes Cargo.
// Bootstrap acquires upstream crates; it is explicitly NOT qualification. Final consumer
// uses a fresh manifest, no path dependency or patch, & Cargo's directory registry source.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { childEnv, loadSealed, sha } from "./upload.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const slash = (s) => s.replaceAll("\\", "/");
const quote = (s) => JSON.stringify(slash(s));
export function isWithin(parent, candidate) {
  const rel = path.relative(path.resolve(parent), path.resolve(candidate));
  return rel === "" || (!rel.startsWith(`..${path.sep}`) && rel !== ".." && !path.isAbsolute(rel));
}

export function assertRegistryManifest(text, unit) {
  if (/^\s*\[(?:patch|replace)(?:[.\]])/m.test(text) || /\b(?:path|git|workspace)\s*=/.test(text)) throw new Error("registry consumer must not contain path/git/workspace dependencies or patches");
  if (!/^[a-zA-Z0-9_-]+$/.test(unit.name)) throw new Error("invalid consumer crate name");
  const escaped = unit.name;
  const pin = new RegExp(`^${escaped}\\s*=\\s*\\{[^\\n]*version\\s*=\\s*"=${unit.version.replaceAll(".", "\\.")}"[^\\n]*\\}`, "m");
  if (!pin.test(text)) throw new Error(`consumer must pin ${unit.name} =${unit.version}`);
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
export function runRegistryConsumer({ packDir, name = "rightkit-logs", root = ROOT, scratchRoot = process.env.RUNNER_TEMP ?? os.tmpdir(), reportFile, env = process.env, spawn = spawnSync } = {}) {
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
  const template = path.join(root, "ci", "consumers", name);
  const manifest = fs.readFileSync(path.join(template, "Cargo.toml"), "utf8");
  assertRegistryManifest(manifest, unit);
  const scratch = fs.mkdtempSync(path.join(scratchRoot, "rk-registry-consumer-"));
  const consumer = path.join(scratch, "consumer");
  const vendor = path.join(scratch, "registry");
  const bootstrap = path.join(scratch, "bootstrap");
  const commandLog = [];
  const cargoEnv = childEnv(env, { CARGO_HOME: path.join(scratch, "cargo-home"), CARGO_TARGET_DIR: path.join(scratch, "target"), CARGO_TERM_COLOR: "never" });
  const run = (command, args, cwd = scratch) => {
    const result = spawn(command, args, { cwd, env: cargoEnv, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, timeout: 60 * 60_000 });
    commandLog.push({ command, args, status: result.status, stdout: String(result.stdout ?? ""), stderr: String(result.stderr ?? "") });
    if (result.error || result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed: ${String(result.stderr || result.error?.message).slice(-3000)}`);
    return result.stdout;
  };
  let report = { schema: 1, kind: "registry-consumer", name, version: unit.version, commit: sealed.manifest.commit, manifestSha256: sealed.manifestSha256, archiveSha256: unit.sha256, verified: false, bootstrap: "path seed only acquires upstream vendor closure; never qualification", commands: commandLog };
  try {
    fs.mkdirSync(vendor);
    fs.mkdirSync(path.join(bootstrap, "src"), { recursive: true });
    const extracted = new Map();
    for (const u of units) {
      const prefix = `${u.name}-${u.version}`;
      const entries = String(run("tar", ["-tzf", path.resolve(packDir, u.file)]))
        .split(/\r?\n/).filter(Boolean);
      if (!entries.length || entries.some((p) => p.includes("\\") || !p.startsWith(`${prefix}/`) || p.split("/").some((s) => s === ".." || s === "."))) throw new Error(`unsafe archive entries: ${u.file}`);
      const listing = run("tar", ["-tvzf", path.resolve(packDir, u.file)]);
      if (String(listing).split(/\r?\n/).filter(Boolean).some((line) => !["-", "d"].includes(line[0]))) throw new Error(`links/special entries in sealed archive: ${u.file}`);
      run("tar", ["-xzf", path.resolve(packDir, u.file), "-C", slash(scratch)]);
      const dir = path.join(scratch, prefix);
      checksumDirectory(dir, u.sha256); // Reject links before bootstrap Cargo reads manifests.
      extracted.set(u.name, dir);
    }
    // Path references exist only in this disposable dependency-fetch seed. cargo vendor excludes
    // those path packages. Final registry directory receives original sealed bytes below.
    const dependency = manifest.split(/\r?\n/).find((line) => line.startsWith(`${name} `));
    const seedDependency = dependency.replace(/version\s*=\s*"[^"]+"/, `path=${quote(extracted.get(name))}`);
    const seed = `[package]\nname="registry-bootstrap"\nversion="0.0.0"\nedition="2021"\n[workspace]\n[dependencies]\n${seedDependency}\n[patch.crates-io]\n${units.map((u) => `${u.name}={path=${quote(extracted.get(u.name))}}`).join("\n")}\n`;
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
    run("cargo", ["build", "--locked", "--offline"], consumer);
    const runtimeOutput = run("cargo", ["run", "--locked", "--offline"], consumer);
    if (fs.readFileSync(lockFile, "utf8") !== lock) throw new Error("lock changed during qualification");
    // Re-read sealed bytes too: upload & qualification remain bound to identical archives.
    for (const u of units) if (sha("sha256", fs.readFileSync(path.join(packDir, u.file))) !== u.sha256) throw new Error(`archive changed during qualification: ${u.name}`);
    report = { ...report, verified: true, consumerDirectory: consumer, lockEvidence, runtimeOutput };
    return report;
  } catch (error) {
    report.error = error.message;
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
