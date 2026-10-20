import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { archiveTool, assertRegistryManifest, isWithin, registryLockEvidence, runRegistryConsumer } from "./registry-consumer.mjs";
import { sha } from "./upload.mjs";

const unit = { name: "rightkit-logs", version: "0.3.4", sha256: "a".repeat(64) };
const manifest = `[package]\nname="consumer"\nversion="0.0.0"\n[workspace]\n[dependencies]\nrightkit-logs = { version = "=0.3.4", default-features = false, features = [] }\n`;
const lock = (u = unit, extra = {}) => `version = 4\n\n[[package]]\nname = "consumer"\nversion = "0.0.0"\n\n[[package]]\nname = "${u.name}"\nversion = "${extra.version ?? u.version}"\nsource = "${extra.source ?? "registry+https://github.com/rust-lang/crates.io-index"}"\nchecksum = "${extra.checksum ?? u.sha256}"\n`;

test("archive tool bypasses Git Bash GNU tar on Windows", () => {
  assert.equal(archiveTool("win32", { SystemRoot: "C:\\Windows" }), "C:\\Windows\\System32\\tar.exe");
  assert.equal(archiveTool("win32", { SYSTEMROOT: "E:\\Windows" }), "E:\\Windows\\System32\\tar.exe");
  assert.equal(archiveTool("darwin", {}), "tar");
});

test("native archive tool lists & extracts absolute runner paths", () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-tar-path-"));
  try {
    const source = path.join(scratch, "source");
    const destination = path.join(scratch, "destination");
    fs.mkdirSync(source);
    fs.mkdirSync(destination);
    fs.writeFileSync(path.join(source, "payload.txt"), "sealed bytes\n");
    const archive = path.join(scratch, "sealed.crate");
    const run = (args) => {
      const result = spawnSync(archiveTool(), args, { encoding: "utf8", timeout: 30_000 });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr);
      return result.stdout;
    };
    run(["-czf", archive, "-C", source, "payload.txt"]);
    assert.match(run(["-tzf", archive]), /payload\.txt/);
    assert.match(run(["-tvzf", archive]), /payload\.txt/);
    run(["-xzf", archive, "-C", destination.replaceAll("\\", "/")]);
    assert.equal(fs.readFileSync(path.join(destination, "payload.txt"), "utf8"), "sealed bytes\n");
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
});

test("consumer requires exact version & refuses path/git/patch escape hatches", () => {
  assert.doesNotThrow(() => assertRegistryManifest(manifest, unit));
  for (const altered of [
    manifest.replace("=0.3.4", "0.3.4"),
    manifest.replace("=0.3.4", "=0.3.5"),
    manifest.replace('version = "=0.3.4"', 'path = "../rightkit-logs"'),
    `${manifest}\n[patch.crates-io]\nrightkit-logs={path="../logs"}\n`,
    `${manifest}\n[replace]\n"rightkit-logs:0.3.4"={git="https://example.invalid"}\n`,
    manifest.replace('version = "=0.3.4"', 'version = "=0.3.4", git="https://example.invalid"'),
  ]) assert.throws(() => assertRegistryManifest(altered, unit));
});

test("lock proof binds registry identity, exact version & archive checksum", () => {
  assert.deepEqual(registryLockEvidence(lock(), [unit]), [{ ...unit, source: "registry+https://github.com/rust-lang/crates.io-index", checksum: unit.sha256, archiveSha256: unit.sha256 }].map(({ sha256, ...row }) => row));
  for (const extra of [{ version: "0.3.5" }, { checksum: "b".repeat(64) }, { source: "path+file:///tmp/logs" }, { source: "registry+https://example.invalid/index" }]) assert.throws(() => registryLockEvidence(lock(unit, extra), [unit]), /registry lock mismatch/);
  assert.throws(() => registryLockEvidence(lock().replace(/^checksum = .*$/m, ""), [unit]), /registry lock mismatch/);
  // Every same-name resolution is checked; a correct row cannot hide another version.
  assert.throws(() => registryLockEvidence(lock() + lock(unit, { version: "0.3.3" }), [unit]), /registry lock mismatch/);
  assert.deepEqual(registryLockEvidence(lock(), [{ ...unit, name: "unused-sealed-sibling" }]), []);
});

test("scratch containment rejects checkout & descendants, accepts sibling prefix", () => {
  const root = path.resolve(os.tmpdir(), "workspace");
  assert.equal(isWithin(root, root), true);
  assert.equal(isWithin(root, path.join(root, "scratch")), true);
  assert.equal(isWithin(root, `${root}-scratch`), false);
});

test("runner refuses local execution without launching any process", () => {
  assert.throws(() => runRegistryConsumer({ env: {}, spawn: () => assert.fail("must not execute") }), /requires GitHub CI/);
});

test("archive tampering refuses before extraction or Cargo", () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-registry-negative-"));
  try {
    const root = path.join(scratch, "checkout");
    const packs = path.join(scratch, "packed");
    fs.mkdirSync(root);
    fs.mkdirSync(packs);
    const archive = Buffer.from("sealed crate");
    const sealedUnit = { ...unit, file: "rightkit-logs-0.3.4.crate", size: archive.length, sha256: sha("sha256", archive) };
    fs.writeFileSync(path.join(packs, sealedUnit.file), "tampered");
    fs.writeFileSync(path.join(packs, "manifest.json"), JSON.stringify({ schema: 1, registry: "crates", commit: "b".repeat(40), runId: "42", runAttempt: "1", units: [sealedUnit] }));
    assert.throws(() => runRegistryConsumer({ packDir: packs, root, scratchRoot: scratch, env: { GITHUB_ACTIONS: "true", GITHUB_SHA: "b".repeat(40), GITHUB_RUN_ID: "42", GITHUB_RUN_ATTEMPT: "1" }, spawn: () => assert.fail("must not execute") }), /sha256 does not match/);
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
});

test("R2a runs exact committed PK example with registry lock, feature resolution & command receipts", t => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-r2a-mocked-")); t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  const root = path.join(scratch, "checkout"); const packs = path.join(scratch, "packed"); const template = path.join(root, "ci/consumers/rightkit-logs");
  fs.mkdirSync(path.dirname(template), { recursive: true }); fs.mkdirSync(packs);
  const committedRoot = fileURLToPath(new URL("../../", import.meta.url));
  fs.cpSync(path.join(committedRoot, "ci/consumers/rightkit-logs"), template, { recursive: true });
  fs.copyFileSync(path.join(committedRoot, "ci/consumer-recipes.json"), path.join(root, "ci/consumer-recipes.json"));
  const bytes = Buffer.from("mock archive bytes"); const selected = { ...unit, file: "rightkit-logs-0.3.4.crate", size: bytes.length, sha256: sha("sha256", bytes) };
  fs.writeFileSync(path.join(packs, selected.file), bytes);
  const sealed = { schema: 1, registry: "crates", commit: "b".repeat(40), runId: "42", runAttempt: "1", units: [selected] };
  fs.writeFileSync(path.join(packs, "manifest.json"), JSON.stringify(sealed));
  const env = { GITHUB_ACTIONS: "true", GITHUB_SHA: sealed.commit, GITHUB_RUN_ID: "42", GITHUB_RUN_ATTEMPT: "1", RUNNER_TEMP: scratch, PATH: process.env.PATH, NODE_AUTH_TOKEN: "must-not-reach-child", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "must-not-reach-child" };
  let mode = "pass";
  const spawn = (command, args, options) => {
    assert.equal(options.env.NODE_AUTH_TOKEN, undefined); assert.equal(options.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN, undefined);
    if (command === "rustc" || args[0] === "--version") return { status: 0, stdout: `${command} mocked-version\n` };
    if (args[0] === "-tzf") return { status: 0, stdout: "rightkit-logs-0.3.4/Cargo.toml\nrightkit-logs-0.3.4/src/lib.rs\n" };
    if (args[0] === "-tvzf") return { status: 0, stdout: "-rw-r--r-- archive files\n" };
    if (args[0] === "-xzf") { const dest = path.join(args[args.indexOf("-C") + 1], "rightkit-logs-0.3.4"); fs.mkdirSync(dest); fs.writeFileSync(path.join(dest, "Cargo.toml"), "sealed manifest"); return { status: 0, stdout: "" }; }
    if (args[0] === "generate-lockfile") { fs.writeFileSync(path.join(options.cwd, "Cargo.lock"), lock(selected)); return { status: 0, stdout: "" }; }
    if (args[0] === "metadata") {
      const vendor = path.join(path.dirname(options.cwd), "registry", "rightkit-logs-0.3.4", "Cargo.toml");
      return { status: 0, stdout: JSON.stringify({ packages: [{ name: "consumer", manifest_path: path.join(options.cwd, "Cargo.toml"), source: null }, { id: "selected", name: selected.name, version: selected.version, source: mode === "path" ? null : "registry+https://github.com/rust-lang/crates.io-index", manifest_path: vendor }], resolve: { nodes: [{ id: "selected", features: [] }] } }) };
    }
    if (args[0] === "build" || args[0] === "run") {
      assert.deepEqual(args.slice(1), ["--locked", "--offline"]);
      assert.doesNotMatch(fs.readFileSync(path.join(options.cwd, "Cargo.toml"), "utf8"), /path\s*=|\[patch/);
      if (args[0] === "run" && mode === "runtime") return { status: 101, stderr: "PK example assertion failed" };
      if (args[0] === "run" && mode === "recipe") fs.appendFileSync(path.join(options.cwd, "src/main.rs"), "// tampered\n");
    }
    return { status: 0, stdout: "PK example passed\n" };
  };
  const invoke = () => runRegistryConsumer({ packDir: packs, root, scratchRoot: scratch, env, spawn });
  const report = invoke(); assert.equal(report.qualification, "R2a"); assert.equal(report.verified, true);
  assert.equal(report.archiveSha256, selected.sha256); assert.equal(report.runId, "42"); assert.equal(report.recipe.sourceCommit, sealed.commit);
  assert.equal(report.recipe.id, "ci/consumers/rightkit-logs/src/main.rs"); assert.deepEqual(report.resolvedFeatures, []);
  assert.equal(report.finalManifestSha256, report.recipe.manifestSha256); assert.equal(report.runtime.status, 0); assert.equal(report.build.status, 0);
  assert.ok(report.commands.some(c => c.args[0] === "run" && c.status === 0));
  assert.deepEqual(report.toolchain, { rustc: "rustc mocked-version", cargo: "cargo mocked-version" });
  for (mode of ["path", "runtime", "recipe"]) assert.throws(invoke);
});
