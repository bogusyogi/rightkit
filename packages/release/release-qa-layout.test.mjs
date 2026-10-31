import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { QA_CONTROL_MARKER } from "./qa-control-scan.mjs";
import { QA_COMPILED_DIRS_ENV } from "./qa-layout.mjs";
import { runQa } from "./release-qa-command.mjs";

const KEY = "unit-test-key";
const store = { read: () => KEY };
const BUNDLE = "b".repeat(64);
const LAYOUT = { binary: "shell", beside: [{ built: "engine" }, { from: "src-tauri/resources", to: "resources" }] };
const qaPlan = (extra = {}) => ({ version: "0.10.0", config: "rightkit-qa.toml", checks: { "smoke.launch": ["app.launch"], journey: ["journey.hub-nav"] }, layout: LAYOUT, ...extra });
const evidence = () => ({ schemaVersion: 4, runId: "r1", status: "passed", finishedAt: "2026-10-09T00:00:00Z", checks: [{ name: "app.launch", status: "passed" }, { name: "journey.hub-nav", status: "passed" }] });

function sh(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

// A committed app with a qa config and resources, a compile output directory holding a QA shell and an engine, a managed runner file, a runs root.
async function withApp(body, { qa = qaPlan(), tests = null } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-layout-run-"));
  try {
    const root = path.join(dir, "repo");
    mkdirSync(path.join(root, "src-tauri", "resources"), { recursive: true });
    sh(dir, ["init", "-q", "-b", "main", root]);
    sh(root, ["config", "user.email", "t@example.com"]); sh(root, ["config", "user.name", "t"]);
    sh(root, ["remote", "add", "origin", "https://github.com/bogusyogi/example-app.git"]);
    writeFileSync(path.join(root, "Cargo.lock"), "lock-1\n");
    writeFileSync(path.join(root, ".gitignore"), ".right-release/\n");
    writeFileSync(path.join(root, "rightkit-qa.toml"), "app = 'demo'\napp_root = '.'\n");
    writeFileSync(path.join(root, "src-tauri", "resources", "a.txt"), "resource-a");
    writeFileSync(path.join(root, "right-release.config.mjs"), `export default { app: "demo", version: "1.0.0", qa: ${JSON.stringify(qa)}${tests ? `, tests: ${JSON.stringify(tests)}` : ""} };\n`);
    sh(root, ["add", "-A"]); sh(root, ["commit", "-q", "-m", "init"]);
    const built = path.join(dir, "target");
    mkdirSync(path.join(built, "debug"), { recursive: true });
    writeFileSync(path.join(built, "debug", "shell"), `${QA_CONTROL_MARKER}`);
    writeFileSync(path.join(built, "debug", "engine"), "engine-bytes");
    const runner = path.join(dir, "managed-rightkit-qa");
    writeFileSync(runner, "#!/bin/sh\n"); chmodSync(runner, 0o755);
    const evidenceFile = path.join(dir, "evidence.json");
    writeFileSync(evidenceFile, JSON.stringify(evidence()));
    return await body({ dir, root, built, runner, evidenceFile, runsRoot: path.join(dir, "runs") });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

const run = (ctx, deps = {}) => runQa({ stage: "source", platform: "mac", configPath: path.join(ctx.root, "right-release.config.mjs"), deps: {
  skipPrimaryCheckout: true, quiet: true, credentialStore: store, policyBundleDigest: BUNDLE, runsRoot: ctx.runsRoot,
  resolveManagedRunner: ({ version, platform }) => ({ file: ctx.runner, sha256: "unused", version, platform, source: "managed" }),
  env: { ...process.env, [QA_COMPILED_DIRS_ENV]: JSON.stringify([ctx.built]) },
  spawnRunner: () => ({ status: 0, stdout: `[qa] 2 passed, 0 failed, 0 skipped; evidence ${ctx.evidenceFile}\n`, stderr: "" }),
  ...deps,
} });
const managed = (ctx) => ({ resolveManagedRunner: ({ version }) => ({ file: ctx.runner, sha256: createHashOf(ctx.runner), version, source: "managed" }) });
const createHashOf = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

test("source QA with qa.version 0.10.0 runs the managed runner against the staged QA shell in a run-scoped overlay, and seals the evidence", async () => withApp(async (ctx) => {
  const seen = [];
  const result = await run(ctx, { ...managed(ctx), spawnRunner: (file, args, options) => {
    // while the runner runs, the overlay exists and the binary it is pointed at is inside its stash
    const binary = options.env.RIGHTKIT_QA_APP_BINARY;
    seen.push({ file, args, cwd: options.cwd, binary, hasEngine: existsSync(path.join(path.dirname(binary), "engine")), hasResource: existsSync(path.join(path.dirname(binary), "resources", "a.txt")), configIsCopy: !lstatSync(args[2]).isSymbolicLink() });
    return { status: 0, stdout: `[qa] evidence ${ctx.evidenceFile}\n`, stderr: "", pid: 4242 };
  } });
  assert.equal(seen.length, 1);
  const [call] = seen;
  assert.equal(call.file, ctx.runner, "the managed runner runs, not a PATH binary");
  assert.deepEqual(call.args.slice(0, 2), ["run", "--config"]);
  assert.ok(call.args[2].startsWith(ctx.runsRoot), "the config is the run-scoped copy");
  assert.equal(call.cwd, path.dirname(call.args[2]));
  assert.ok(call.binary.includes(`${path.sep}.rightkit-qa-bin${path.sep}host${path.sep}shell`));
  assert.ok(call.hasEngine && call.hasResource && call.configIsCopy);
  assert.ok(!call.binary.startsWith(ctx.root), "nothing runs from the checkout or snapshot");
  assert.equal(readdirSync(ctx.runsRoot).length, 0, "the run-scoped directory is removed after the run");
  assert.deepEqual(readdirSync(ctx.root).sort(), [".git", ".gitignore", ".right-release", "Cargo.lock", "right-release.config.mjs", "rightkit-qa.toml", "src-tauri"].sort(), "no stash or pointer file was written into the source (only the receipt directory, as always)");
  assert.equal(result.receipt.status, "passed");
  assert.equal(result.receipt.runner.source, "managed");
  assert.equal(result.receipt.runner.version, "0.10.0");
  assert.equal(result.receipt.layout.beside.length, 2);
  assert.match(result.receipt.layout.binary.sha256, /^[0-9a-f]{64}$/);
  assert.equal(result.receipt.layout.source, path.join(ctx.built, "debug", "shell"));
  assert.match(result.receipt.digest, /^[0-9a-f]{64}$/, "the layout evidence is inside the sealed body");
}));

test("a managed runner is resolved for qa.version 0.10.0 only; a refusal names what to enrol and nothing runs", async () => withApp(async (ctx) => {
  let ran = false;
  await assert.rejects(run(ctx, { resolveManagedRunner: () => { throw Object.assign(new Error("rightkit-qa 0.10.0 is not enrolled for mac"), { code: "RK_QA_RUNNER_MISSING" }); }, spawnRunner: () => { ran = true; return { status: 0, stdout: "" }; } }), /not enrolled for mac/);
  assert.equal(ran, false);
}));

test("a digest drift between resolution and sealing is refused", async () => withApp(async (ctx) => {
  await assert.rejects(run(ctx, { resolveManagedRunner: ({ version }) => ({ file: ctx.runner, sha256: "0".repeat(64), version, source: "managed" }) }), /changed between its verification and its digest/);
}));

test("a declared layout without the gate's compile outputs, or with a plain app, is refused before the runner starts", async () => withApp(async (ctx) => {
  let ran = false;
  const spawnRunner = () => { ran = true; return { status: 0, stdout: "" }; };
  await assert.rejects(run(ctx, { ...managed(ctx), env: { ...process.env, [QA_COMPILED_DIRS_ENV]: "" }, spawnRunner }), /qa\.layout needs the gate's compile outputs/);
  writeFileSync(path.join(ctx.built, "debug", "shell"), "no marker");
  await assert.rejects(run(ctx, { ...managed(ctx), spawnRunner }), /RK_QA_LAYOUT_NOT_QA/);
  assert.equal(ran, false);
  assert.equal(existsSync(ctx.runsRoot) ? readdirSync(ctx.runsRoot).length : 0, 0);
}));

test("an invalid qa.layout is refused with the plan before anything runs", async () => withApp(async (ctx) => {
  await assert.rejects(run(ctx, managed(ctx)), /qa\.layout is invalid[\s\S]*binary must be a bare/);
}, { qa: qaPlan({ layout: { binary: "a/b" } }) }));

test("the runner's own failure still cleans the run-scoped directory", async () => withApp(async (ctx) => {
  await assert.rejects(run(ctx, { ...managed(ctx), spawnRunner: () => { throw new Error("spawn exploded"); } }), /spawn exploded/);
  assert.equal(readdirSync(ctx.runsRoot).length, 0);
}));

test("qa.layout.build: source QA runs the app's build and sign suites first and stages the signed shell and engine from their outputs, not the compile's", async () => {
  const tests = { suites: [{ name: "qa-build-shell", cmd: "node", args: ["build.mjs"] }, { name: "qa-sign", sign: { files: [{ from: "qa-build-shell", path: "debug/shell" }, { from: "qa-build-shell", path: "debug/engine" }] } }, { name: "unit", cmd: "node", args: ["unit.mjs"] }] };
  await withApp(async (ctx) => {
    const signedDir = path.join(ctx.dir, "signed");
    mkdirSync(signedDir);
    writeFileSync(path.join(signedDir, "shell"), `${QA_CONTROL_MARKER} signed`);
    writeFileSync(path.join(signedDir, "engine"), "engine signed");
    const ran = []; let staged = null;
    const runSuites = async (plan, { within }) => {
      ran.push(...plan.map((item) => item.name));
      const within_ = await within({ outputs: new Map([["qa-build-shell", [ctx.built]]]), env: { RIGHTKIT_QA_SIGNED_FILES: JSON.stringify({ "debug/shell": path.join(signedDir, "shell"), "debug/engine": path.join(signedDir, "engine") }) } });
      staged = within_;
      return { exitCode: 0, results: [], within: within_ };
    };
    const { receipt } = await run(ctx, { ...managed(ctx), runSuites, env: { ...process.env } });
    assert.deepEqual(ran, ["qa-build-shell", "qa-sign"], "only the declared build suites run, in declared order");
    assert.equal(receipt.layout.signed, true);
    assert.ok(staged.evidence.beside.some((e) => e.path === "engine"));
    const failing = async () => ({ exitCode: 1, results: [{ suite: "qa-build-shell", exitCode: 1 }] });
    await assert.rejects(() => run(ctx, { ...managed(ctx), runSuites: failing }), /qa.layout.build: the QA shell build failed \(qa-build-shell\); no QA ran/);
  }, { qa: qaPlan({ layout: { ...LAYOUT, build: ["qa-build-shell", "qa-sign"] } }), tests });
});
