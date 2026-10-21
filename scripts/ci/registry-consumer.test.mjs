import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { archiveTool, assertRegistryManifest, isWithin, registryLockEvidence, runRegistryConsumer } from "./registry-consumer.mjs";
import { sha } from "./upload.mjs";

const unit = { name: "rightkit-logs", version: "0.3.5", sha256: "a".repeat(64) };
const manifest = `[package]\nname="consumer"\nversion="0.0.0"\n[workspace]\n[dependencies]\nrightkit-logs = { version = "=0.3.5", default-features = false, features = [] }\n`;
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
    manifest.replace("=0.3.5", "0.3.5"),
    manifest.replace("=0.3.5", "=0.3.6"),
    manifest.replace('version = "=0.3.5"', 'path = "../rightkit-logs"'),
    `${manifest}\n[patch.crates-io]\nrightkit-logs={path="../logs"}\n`,
    `${manifest}\n[replace]\n"rightkit-logs:0.3.5"={git="https://example.invalid"}\n`,
    manifest.replace('version = "=0.3.5"', 'version = "=0.3.5", git="https://example.invalid"'),
  ]) assert.throws(() => assertRegistryManifest(altered, unit));
});

test("lock proof binds registry identity, exact version & archive checksum", () => {
  assert.deepEqual(registryLockEvidence(lock(), [unit]), [{ ...unit, source: "registry+https://github.com/rust-lang/crates.io-index", checksum: unit.sha256, archiveSha256: unit.sha256 }].map(({ sha256, ...row }) => row));
  for (const extra of [{ version: "0.3.6" }, { checksum: "b".repeat(64) }, { source: "path+file:///tmp/logs" }, { source: "registry+https://example.invalid/index" }]) assert.throws(() => registryLockEvidence(lock(unit, extra), [unit]), /registry lock mismatch/);
  assert.throws(() => registryLockEvidence(lock().replace(/^checksum = .*$/m, ""), [unit]), /registry lock mismatch/);
  // Every same-name resolution is checked; a correct row cannot hide another version.
  assert.throws(() => registryLockEvidence(lock() + lock(unit, { version: "0.3.6" }), [unit]), /registry lock mismatch/);
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
    const sealedUnit = { ...unit, file: "rightkit-logs-0.3.5.crate", size: archive.length, sha256: sha("sha256", archive) };
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
  const bytes = Buffer.from("mock archive bytes"); const selected = { ...unit, file: "rightkit-logs-0.3.5.crate", size: bytes.length, sha256: sha("sha256", bytes) };
  fs.writeFileSync(path.join(packs, selected.file), bytes);
  const sealed = { schema: 1, registry: "crates", commit: "b".repeat(40), runId: "42", runAttempt: "1", units: [selected] };
  fs.writeFileSync(path.join(packs, "manifest.json"), JSON.stringify(sealed));
  const env = { GITHUB_ACTIONS: "true", GITHUB_SHA: sealed.commit, GITHUB_RUN_ID: "42", GITHUB_RUN_ATTEMPT: "1", RUNNER_TEMP: scratch, PATH: process.env.PATH, NODE_AUTH_TOKEN: "must-not-reach-child", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "must-not-reach-child" };
  let mode = "pass";
  const spawn = (command, args, options) => {
    assert.equal(options.env.NODE_AUTH_TOKEN, undefined); assert.equal(options.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN, undefined);
    if (command === "rustc" || args[0] === "--version") return { status: 0, stdout: `${command} mocked-version\n` };
    if (args[0] === "-tzf") return { status: 0, stdout: "rightkit-logs-0.3.5/Cargo.toml\nrightkit-logs-0.3.5/src/lib.rs\n" };
    if (args[0] === "-tvzf") return { status: 0, stdout: "-rw-r--r-- archive files\n" };
    if (args[0] === "-xzf") { const dest = path.join(args[args.indexOf("-C") + 1], "rightkit-logs-0.3.5"); fs.mkdirSync(dest); fs.writeFileSync(path.join(dest, "Cargo.toml"), "sealed manifest"); return { status: 0, stdout: "" }; }
    if (args[0] === "generate-lockfile") { fs.writeFileSync(path.join(options.cwd, "Cargo.lock"), lock(selected)); return { status: 0, stdout: "" }; }
    if (args[0] === "metadata") {
      const vendor = path.join(path.dirname(options.cwd), "registry", "rightkit-logs-0.3.5", "Cargo.toml");
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

function runsHarness(t, runs, platform = "win32", { name = "rightkit-logs", version = "0.3.5", features = [], env: extraEnv = {}, hook } = {}) {
  const scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-r2a-runs-"))); t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  const root = path.join(scratch, "checkout"); const packs = path.join(scratch, "packed"); const template = path.join(root, "ci/consumers", name);
  fs.mkdirSync(path.dirname(template), { recursive: true }); fs.mkdirSync(packs);
  const committedRoot = fileURLToPath(new URL("../../", import.meta.url));
  fs.cpSync(path.join(committedRoot, "ci/consumers", name), template, { recursive: true });
  const inventory = JSON.parse(fs.readFileSync(path.join(committedRoot, "ci/consumer-recipes.json"), "utf8")); if (runs) inventory.cargo[name].runs = runs;
  fs.writeFileSync(path.join(root, "ci/consumer-recipes.json"), JSON.stringify(inventory));
  const bytes = Buffer.from("mock archive bytes"); const selected = { name, version, file: `${name}-${version}.crate`, size: bytes.length, sha256: sha("sha256", bytes) };
  fs.writeFileSync(path.join(packs, selected.file), bytes);
  const sealed = { schema: 1, registry: "crates", commit: "b".repeat(40), runId: "42", runAttempt: "1", units: [selected] };
  fs.writeFileSync(path.join(packs, "manifest.json"), JSON.stringify(sealed));
  const env = { GITHUB_ACTIONS: "true", GITHUB_SHA: sealed.commit, GITHUB_RUN_ID: "42", GITHUB_RUN_ATTEMPT: "1", RUNNER_TEMP: scratch, PATH: process.env.PATH, NODE_AUTH_TOKEN: "secret", RIGHTKIT_TEST_REAL_USER_STORES: "0", RK_CONSUMER_RUN_ROOT: "/candidate-chosen", ...extraEnv };
  const calls = [];
  const spawn = (command, args, options) => {
    calls.push({ command, args, cwd: options.cwd, env: options.env });
    const hooked = hook?.(command, args, options); if (hooked) return hooked;
    if (command === "rustc" || args[0] === "--version") return { status: 0, stdout: `${command} mocked-version\n` };
    if (args[0] === "-tzf") return { status: 0, stdout: `${name}-${version}/Cargo.toml\n${name}-${version}/src/lib.rs\n` };
    if (args[0] === "-tvzf") return { status: 0, stdout: "-rw-r--r-- archive files\n" };
    if (args[0] === "-xzf") { const dest = path.join(args[args.indexOf("-C") + 1], `${name}-${version}`); fs.mkdirSync(dest); fs.writeFileSync(path.join(dest, "Cargo.toml"), "sealed manifest"); return { status: 0, stdout: "" }; }
    if (args[0] === "generate-lockfile") { fs.writeFileSync(path.join(options.cwd, "Cargo.lock"), lock(selected)); return { status: 0, stdout: "" }; }
    if (args[0] === "metadata") {
      const vendor = path.join(path.dirname(options.cwd), "registry", `${name}-${version}`, "Cargo.toml");
      return { status: 0, stdout: JSON.stringify({ packages: [{ name: "consumer", manifest_path: path.join(options.cwd, "Cargo.toml"), source: null }, { id: "selected", name: selected.name, version: selected.version, source: "registry+https://github.com/rust-lang/crates.io-index", manifest_path: vendor }], resolve: { nodes: [{ id: "selected", features }] } }) };
    }
    if (args[0] === "run" && args.includes("beta")) return { status: 101, stderr: "beta assertion failed" };
    return { status: 0, stdout: `ok ${args.join(" ")}\n` };
  };
  return { scratch, env, calls, invoke: (extra = {}) => runRegistryConsumer({ packDir: packs, name, root, scratchRoot: scratch, env, spawn, platform, ...extra }) };
}
test("declared crate runs execute with exactly their declared harness-owned env & are recorded", t => {
  const h = runsHarness(t, [{ id: "alpha", features: ["sqlcipher"], args: ["native-db"], env: { RIGHTKIT_TEST_REAL_USER_STORES: "literal1", RK_CONSUMER_RUN_ROOT: "freshRunRoot" } }, { id: "mac-only", args: ["native-helper"], hosts: ["darwin"], env: { RK_CONSUMER_HELPER_NAME: "signedHelper", RK_CONSUMER_HELPER_TEAM_ID: "signedHelper" } }, { id: "plain", args: ["core"] }]);
  const report = h.invoke(); assert.equal(report.verified, true);
  const runs = h.calls.filter(c => c.args[0] === "run");
  assert.deepEqual(runs.map(c => c.args.slice(1)), [["--locked", "--offline", "--features", "sqlcipher", "--", "native-db"], ["--locked", "--offline", "--", "core"]]);
  const base = h.calls.find(c => c.args[0] === "build").env;
  assert.deepEqual(Object.keys(runs[0].env).filter(k => !Object.hasOwn(base, k)).sort(), ["RIGHTKIT_TEST_REAL_USER_STORES", "RK_CONSUMER_RUN_ROOT"]);
  assert.equal(runs[0].env.RIGHTKIT_TEST_REAL_USER_STORES, "1"); assert.notEqual(runs[0].env.RK_CONSUMER_RUN_ROOT, "/candidate-chosen");
  assert.equal(path.relative(h.scratch, runs[0].env.RK_CONSUMER_RUN_ROOT).startsWith(".."), false);
  assert.equal(runs[1].env.RIGHTKIT_TEST_REAL_USER_STORES, undefined); // neither the workflow's value nor the candidate's value leaks into an undeclared run
  assert.equal(runs[1].env.RK_CONSUMER_RUN_ROOT, undefined); assert.equal(runs[0].env.NODE_AUTH_TOKEN, undefined);
  assert.deepEqual(report.runs, [{ id: "alpha", args: ["native-db"], features: ["sqlcipher"], envNames: ["RIGHTKIT_TEST_REAL_USER_STORES", "RK_CONSUMER_RUN_ROOT"], status: "passed" }, { id: "mac-only", args: ["native-helper"], features: [], envNames: [], status: "not-applicable", hosts: ["darwin"] }, { id: "plain", args: ["core"], features: [], envNames: [], status: "passed" }]);
  assert.match(report.runtimeOutput, /\[run mac-only\] not-applicable/);
});
test("default consumer without declared runs still records one default run", t => {
  const h = runsHarness(t, null); const report = h.invoke();
  assert.deepEqual(h.calls.filter(c => c.args[0] === "run").map(c => c.args.slice(1)), [["--locked", "--offline"]]);
  assert.deepEqual(report.runs, [{ id: "default", args: [], features: [], envNames: [], status: "passed" }]);
});
test("a failing run is recorded failed; a missing prerequisite refuses before any Cargo work", t => {
  const failing = runsHarness(t, [{ id: "alpha", args: ["alpha"] }, { id: "beta", args: ["beta"] }]);
  assert.throws(() => failing.invoke(), error => { assert.deepEqual(error.report.runs.map(r => r.status), ["passed", "failed"]); return /beta assertion failed/.test(error.message); });
  const missing = runsHarness(t, [{ id: "browser", env: { RK_R2_CFT_ROOT: "cftRoot" } }]);
  assert.throws(() => missing.invoke(), error => { assert.match(error.message, /prerequisite missing: cftRoot/); assert.equal(error.report.verified, false); return true; });
  assert.deepEqual(missing.calls, []);
  const reportFile = path.join(missing.scratch, "out", "report.json"); assert.throws(() => missing.invoke({ reportFile }));
  assert.match(JSON.parse(fs.readFileSync(reportFile, "utf8")).error, /prerequisite missing/);
});

// ---- rightkit-credentials signed nested helper: stage default consumer + unchanged signed helper, launch staged bundle ----
const HELPER_APP = "/Users/runner/approved/RightKitCredentialsQualification.app";
const helperEnv = { RK_CONSUMER_HELPER_NAME: "RightKitCredentialsQualification", RK_CONSUMER_HELPER_TEAM_ID: "6KLGD3LLKF", RK_SOURCE_HELPER_APP: HELPER_APP };
const STAGED_LAUNCH = path.join("Contents", "MacOS", "consumer");
function credentialsHarness(t, platform, { env = helperEnv, mutateReceipt = receipt => receipt, helperOutput = "native bundled helper consumer assertions passed\n" } = {}) {
  const hook = (command, args, options) => {
    if (args[0] === "build") {
      const exe = path.join(options.env.CARGO_TARGET_DIR, "debug", "rightkit-credentials-r2-consumer"); fs.mkdirSync(path.dirname(exe), { recursive: true });
      fs.writeFileSync(exe, "default consumer bytes"); return { status: 0, stdout: "" };
    }
    if (command === "python3") { // stands in for the reviewed preparer: same flags, same bundle layout & receipt shape
      const flag = name => args[args.indexOf(name) + 1];
      const launch = path.join(flag("--bundle"), STAGED_LAUNCH); fs.mkdirSync(path.dirname(launch), { recursive: true }); fs.copyFileSync(flag("--consumer-executable"), launch);
      const receipt = { schema: 1, kind: "credentials-native-helper-input", name: "RightKitCredentialsQualification", teamId: "6KLGD3LLKF", bundleIdentifier: "org.rightkit.qualification.credentials", accessGroup: "6KLGD3LLKF.org.rightkit.qualification.credentials", helperTree: { sha256: "d".repeat(64) }, consumerSha256: sha("sha256", fs.readFileSync(launch)), signatureVerified: true, nativeConsumerVerified: false };
      fs.writeFileSync(flag("--receipt"), `${JSON.stringify(mutateReceipt(receipt))}\n`); return { status: 0, stdout: `${flag("--receipt")}\n` };
    }
    if (command.endsWith(STAGED_LAUNCH)) return { status: 0, stdout: helperOutput };
    return undefined;
  };
  return runsHarness(t, null, platform, { name: "rightkit-credentials", version: "0.4.1", features: ["db-adapter"], env, hook });
}
test("credentials macOS stages the default consumer + signed helper before SQLCipher, then launches the staged bundle instead of Cargo", t => {
  const h = credentialsHarness(t, "darwin"); const report = h.invoke(); assert.equal(report.verified, true);
  const index = predicate => h.calls.findIndex(predicate);
  const prepare = index(c => c.command === "python3"); const build = index(c => c.args[0] === "build"); const nativeDb = index(c => c.args[0] === "run" && c.args.includes("native-db")); const staged = index(c => c.command.endsWith(STAGED_LAUNCH));
  assert.ok(build >= 0 && build < prepare && prepare < nativeDb && nativeDb < staged, "default build, then staging, then SQLCipher run, then staged launch");
  assert.equal(h.calls.some(c => c.command === "cargo" && c.args[0] === "run" && c.args.includes("native-helper")), false);
  const call = h.calls[prepare]; const flag = name => call.args[call.args.indexOf(name) + 1];
  assert.equal(path.basename(call.args[0]), "prepare_signed_helper.py"); assert.ok(call.args[0].startsWith(path.join(h.scratch, "checkout", "ci/consumers/rightkit-credentials")), "reviewed preparer comes from the committed checkout");
  assert.deepEqual(call.args.filter(a => a.startsWith("--")), ["--scratch-root", "--consumer-executable", "--bundle", "--receipt"]);
  assert.equal(path.basename(flag("--consumer-executable")), "rightkit-credentials-r2-consumer"); assert.equal(path.basename(flag("--bundle")), "CredentialsConsumer.app"); assert.equal(path.basename(flag("--receipt")), "credentials-qualification-helper-input.json");
  for (const name of ["--bundle", "--receipt", "--consumer-executable"]) assert.equal(path.dirname(flag(name)).startsWith(flag("--scratch-root")), true);
  assert.equal(call.env.RK_SOURCE_HELPER_APP, HELPER_APP);
  for (const other of h.calls.filter((c, i) => i !== prepare)) assert.equal(other.env.RK_SOURCE_HELPER_APP, undefined, `source path must reach only the preparer: ${other.command} ${other.args[0]}`);
  const launch = h.calls[staged]; assert.deepEqual(launch.args, ["native-helper"]); assert.equal(launch.env.RK_CONSUMER_HELPER_NAME, "RightKitCredentialsQualification"); assert.equal(launch.env.RK_CONSUMER_HELPER_TEAM_ID, "6KLGD3LLKF");
  assert.equal(report.helperInput.nativeConsumerVerified, false); assert.equal(report.helperInput.signatureVerified, true);
  assert.equal(report.helperInputSha256, sha("sha256", Buffer.from(`${JSON.stringify(report.helperInput)}\n`)));
  assert.equal(report.helperInput.consumerSha256, sha("sha256", Buffer.from("default consumer bytes")));
  assert.deepEqual(report.runs.map(r => [r.id, r.status]), [["core", "passed"], ["native-db", "passed"], ["native-helper", "passed"]]);
  assert.match(report.runtimeOutput, /native bundled helper consumer assertions passed/);
});
test("credentials macOS refuses before any Cargo work without the supplied signed helper, with a misnamed one, or without the exact pinned identity", t => {
  const bad = { ...helperEnv };
  for (const [change, why] of [[{ RK_SOURCE_HELPER_APP: undefined }, /prerequisite missing: signedHelperSource/], [{ RK_SOURCE_HELPER_APP: "relative/RightKitCredentialsQualification.app" }, /signedHelperSource/], [{ RK_SOURCE_HELPER_APP: "/x/../RightKitCredentialsQualification.app" }, /signedHelperSource/], [{ RK_SOURCE_HELPER_APP: "/x/OtherHelper.app" }, /signedHelperSource/], [{ RK_CONSUMER_HELPER_TEAM_ID: "ABCDE12345" }, /pinned signed helper name & team id/], [{ RK_CONSUMER_HELPER_NAME: "OtherHelper" }, /pinned signed helper name & team id/], [{ RK_CONSUMER_HELPER_NAME: undefined }, /prerequisite missing: signedHelper/]]) {
    const h = credentialsHarness(t, "darwin", { env: { ...bad, ...change } });
    assert.throws(() => h.invoke(), error => { assert.match(error.message, why); assert.equal(error.report.verified, false); return true; });
    assert.deepEqual(h.calls, []);
  }
});
test("credentials macOS refuses when the recipe row is not the exact reviewed native-helper argv, instead of falling back to Cargo", t => {
  for (const runs of [[{ id: "core", args: ["core"] }], [{ id: "native-helper", args: ["native-helper"], hosts: ["win32"] }, { id: "core", args: ["core"] }], [{ id: "native-helper", args: ["native-helper"], features: ["sqlcipher"], hosts: ["darwin"], env: { RK_CONSUMER_HELPER_NAME: "signedHelper", RK_CONSUMER_HELPER_TEAM_ID: "signedHelper" } }]]) {
    const h = runsHarness(t, runs, "darwin", { name: "rightkit-credentials", version: "0.4.1", features: ["db-adapter"], env: helperEnv });
    assert.throws(() => h.invoke(), /requires the reviewed native-helper run/); assert.deepEqual(h.calls, []);
  }
});
test("credentials staging refuses an input receipt that is not the reviewed signed identity bound to the default consumer, and a launch that lacks its assertion line", t => {
  for (const [mutate, why] of [[r => ({ ...r, nativeConsumerVerified: true }), /not the reviewed signed-helper identity/], [r => ({ ...r, signatureVerified: false }), /not the reviewed signed-helper identity/], [r => ({ ...r, teamId: "ABCDE12345" }), /not the reviewed signed-helper identity/], [r => ({ ...r, bundleIdentifier: "org.rightkit.credentials" }), /not the reviewed signed-helper identity/], [r => ({ ...r, accessGroup: "6KLGD3LLKF.org.rightkit.credentials" }), /not the reviewed signed-helper identity/], [r => ({ ...r, consumerSha256: "e".repeat(64) }), /does not bind the staged default consumer/], [r => ({ ...r, helperTree: {} }), /does not bind the staged default consumer/]]) {
    const h = credentialsHarness(t, "darwin", { mutateReceipt: mutate });
    assert.throws(() => h.invoke(), error => { assert.match(error.message, why); return true; });
    assert.equal(h.calls.some(c => c.args[0] === "run"), false, "no consumer run after a refused staging");
  }
  const silent = credentialsHarness(t, "darwin", { helperOutput: "ok\n" });
  assert.throws(() => silent.invoke(), error => { assert.match(error.message, /did not report its consumer assertions/); assert.equal(error.report.runs[2].status, "failed"); return true; });
});
test("credentials on Windows never stages: native-helper is not-applicable & the helper input is not required", t => {
  const h = credentialsHarness(t, "win32", { env: {} }); const report = h.invoke(); assert.equal(report.verified, true);
  assert.equal(h.calls.some(c => c.command === "python3"), false); assert.equal(report.helperInput, undefined);
  assert.deepEqual(report.runs.map(r => [r.id, r.status]), [["core", "passed"], ["native-db", "passed"], ["native-helper", "not-applicable"]]);
});
