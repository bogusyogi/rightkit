import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { consumerStage, localRunPlan, patchedManifest } from "./rk-consumer-stage.mjs";

const sha256 = (text) => crypto.createHash("sha256").update(text).digest("hex");
const SHA = "a".repeat(40);

function fixture({ runs, unit = "rightkit-example", version = "1.2.3" } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-stage-"));
  fs.mkdirSync(path.join(root, "crates", unit), { recursive: true });
  fs.writeFileSync(path.join(root, "crates", unit, "Cargo.toml"), `[package]\nname = "${unit}"\nversion = "${version}"\n`);
  const dir = path.join(root, "ci", "consumers", unit);
  fs.mkdirSync(path.join(dir, "src"), { recursive: true });
  const manifest = `[package]\nname = "${unit}-registry-consumer"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n[workspace]\n[dependencies]\n${unit} = { version = "=${version}" }\n`;
  fs.writeFileSync(path.join(dir, "Cargo.toml"), manifest);
  fs.writeFileSync(path.join(dir, "src", "main.rs"), "fn main() {}\n");
  const files = { "Cargo.toml": sha256(manifest), "src/main.rs": sha256("fn main() {}\n") };
  const entry = { pkId: "PK90", id: `ci/consumers/${unit}/src/main.rs`, files, ...(runs ? { runs } : {}) };
  fs.writeFileSync(path.join(root, "ci", "consumer-recipes.json"), JSON.stringify({ schema: 1, cargo: { [unit]: entry } }));
  const logDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-stage-logs-"));
  return { root, unit, version, logDir };
}

/** A broker double: records every brokered command, builds a fake binary into a fake target dir. */
function brokerDouble({ buildExit = 0, runExit = 0, attested = true } = {}) {
  const calls = [];
  const target = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-target-"));
  const runCommand = async (command, args, { cwd }) => {
    calls.push({ command, args, cwd });
    if (args[1] === "metadata") return { code: 0, out: JSON.stringify({ target_directory: target }), timedOut: false, seconds: 0 };
    if (args[1] === "build") {
      const manifest = fs.readFileSync(path.join(cwd, "Cargo.toml"), "utf8");
      const name = manifest.match(/^name\s*=\s*"([^"]+)"/m)[1];
      fs.mkdirSync(path.join(target, "aarch64-apple-darwin", "debug"), { recursive: true });
      fs.writeFileSync(path.join(target, "aarch64-apple-darwin", "debug", name), "binary");
      return { code: buildExit, out: `rustright: request 11111111-1111-4111-8111-111111111111\n${buildExit ? "error[E0277]: trait bound not satisfied\n" : ""}`, timedOut: false, seconds: 1 };
    }
    return { code: runExit, out: "rustright: request 22222222-2222-4222-8222-222222222222\nproof ok\n", timedOut: false, seconds: 1 };
  };
  const attest = (_out, { argv, workspace }) => (attested ? { verified: true, signatureVerified: true, requestId: "11111111-1111-4111-8111-111111111111", nonce: "n".repeat(64), argv, workspace } : { verified: false, reason: "signature verification failed" });
  return { calls, runCommand, attest };
}

test("the check manifest is the committed recipe plus one patch pointing the unit at the checkout; a recipe with its own patch is refused", () => {
  const out = patchedManifest('[package]\nname = "x"\n', "rightkit-a", path.join("..", "..", "crates", "rightkit-a"));
  assert.match(out, /\[patch\.crates-io\]\nrightkit-a = \{ path = "\.\.\/\.\.\/crates\/rightkit-a" \}\n$/);
  assert.throws(() => patchedManifest('[package]\nname = "x"\n[patch.crates-io]\nfoo = { path = "x" }\n', "rightkit-a", "p"), /must not carry its own \[patch\]/);
});

test("runs needing real user stores or a signed helper are not run locally, with the reason; Chrome runs are", () => {
  const plan = localRunPlan([
    { id: "store", features: [], args: [], hosts: null, env: { RIGHTKIT_TEST_REAL_USER_STORES: "literal1" } },
    { id: "chrome", features: [], args: [], hosts: null, env: { RK_R2_CFT_ROOT: "cftRoot", RK_R2_CFT_SHA256: "cftSha256" } },
    { id: "windows-only", features: [], args: [], hosts: ["win32"], env: {} },
  ], "darwin");
  assert.deepEqual(plan.map((p) => p.local), [false, true, false]);
  assert.match(plan[0].reason, /literal1, which only the hosted runner provides/);
  assert.equal(plan[1].needsChrome, true);
  assert.match(plan[2].reason, /not applicable on darwin/);
});

test("a recipe without runs: brokered build in a fresh check dir under .consumer-checks, then a brokered `cargo run`", async () => {
  const f = fixture();
  const b = brokerDouble();
  const stage = await consumerStage({ root: f.root, unit: f.unit, version: f.version, sha: SHA, logDir: f.logDir, env: {}, runCommand: b.runCommand, attest: b.attest, provisionChrome: async () => { throw new Error("no chrome needed"); }, platform: "darwin" });
  assert.equal(stage.ok, true, stage.reason);
  assert.ok(stage.checkDir.startsWith(path.join(".consumer-checks", `${f.unit}-${SHA.slice(0, 12)}-`)));
  assert.deepEqual(b.calls.filter((c) => c.args[1] !== "metadata").map((c) => c.args.join(" ")), ["cargo build", "cargo run"]);
  assert.ok(b.calls.every((c) => c.command === "rustright" && c.cwd === path.join(f.root, stage.checkDir)), "every cargo command enters the broker from the check dir");
  assert.match(fs.readFileSync(path.join(f.root, stage.checkDir, "Cargo.toml"), "utf8"), /\[patch\.crates-io\]/);
  assert.deepEqual(stage.runs.map((r) => [r.id, r.via, r.status]), [["default", "broker", "passed"]]);
  assert.match(stage.proof, /local-source fixture proof/);
});

test("a fixture that does not compile fails the stage at build, before any run; an unattested build fails too", async () => {
  const f = fixture();
  const broken = brokerDouble({ buildExit: 101 });
  const stage = await consumerStage({ root: f.root, unit: f.unit, version: f.version, sha: SHA, logDir: f.logDir, env: {}, runCommand: broken.runCommand, attest: broken.attest, provisionChrome: async () => ({}), platform: "darwin" });
  assert.equal(stage.ok, false);
  assert.equal(stage.failedAt, "build");
  assert.match(stage.lines.join("\n"), /E0277/);
  assert.ok(!broken.calls.some((c) => c.args[1] === "run"), "no run after a failed build");
  const unattested = brokerDouble({ attested: false });
  const second = await consumerStage({ root: f.root, unit: f.unit, version: f.version, sha: SHA, logDir: f.logDir, env: {}, runCommand: unattested.runCommand, attest: unattested.attest, provisionChrome: async () => ({}), platform: "darwin" });
  assert.equal(second.failedAt, "build");
  assert.match(second.reason, /not attested/);
});

test("a Chrome run provisions the pinned browser, gets its reviewed env, executes the built binary, and a non-zero exit fails the stage", async () => {
  const f = fixture({ runs: [{ id: "browser-r2", env: { RK_R2_CFT_ROOT: "cftRoot", RK_R2_CFT_SHA256: "cftSha256", RK_R2_SCRATCH: "freshRunRoot" } }] });
  const b = brokerDouble();
  let provisioned = null;
  const provisionChrome = async ({ runnerTemp }) => {
    const cft = path.join(runnerTemp, "chrome-for-testing", "current");
    fs.mkdirSync(cft, { recursive: true });
    provisioned = runnerTemp;
    return { RK_R2_CFT_ROOT: cft, RK_R2_CFT_SHA256: "c".repeat(64) };
  };
  const seen = [];
  const passing = (binary, args, { env, logFile }) => { seen.push({ binary, env }); fs.writeFileSync(logFile, "proof browser_r2=pass\n"); return { code: 0, out: "proof browser_r2=pass\n", timedOut: false }; };
  const ok = await consumerStage({ root: f.root, unit: f.unit, version: f.version, sha: SHA, logDir: f.logDir, env: {}, runCommand: b.runCommand, attest: b.attest, provisionChrome, platform: "darwin", spawnRun: passing });
  assert.equal(ok.ok, true, ok.reason);
  assert.ok(provisioned, "Chrome is provisioned when a run needs it");
  assert.equal(seen.length, 1);
  assert.equal(seen[0].env.RK_R2_CFT_SHA256, "c".repeat(64));
  assert.ok(seen[0].env.RK_R2_SCRATCH && fs.existsSync(seen[0].env.RK_R2_SCRATCH), "a fresh run root per run");
  assert.match(ok.runs[0].via, /^direct/);
  const failing = (binary, args, { logFile }) => { fs.writeFileSync(logFile, "Error: browser did not retain imported cookies\n"); return { code: 1, out: "Error: browser did not retain imported cookies\n", timedOut: false }; };
  const bad = await consumerStage({ root: f.root, unit: f.unit, version: f.version, sha: SHA, logDir: f.logDir, env: {}, runCommand: brokerDouble().runCommand, attest: b.attest, provisionChrome, platform: "darwin", spawnRun: failing });
  assert.equal(bad.ok, false);
  assert.equal(bad.failedAt, "run browser-r2");
  assert.match(bad.lines.join("\n"), /did not retain imported cookies/);
});

test("a unit without a reviewed recipe fails at recipe, before any broker work", async () => {
  const f = fixture();
  fs.rmSync(path.join(f.root, "ci", "consumers", f.unit), { recursive: true, force: true });
  const b = brokerDouble();
  const stage = await consumerStage({ root: f.root, unit: f.unit, version: f.version, sha: SHA, logDir: f.logDir, env: {}, runCommand: b.runCommand, attest: b.attest, provisionChrome: async () => ({}), platform: "darwin" });
  assert.equal(stage.failedAt, "recipe");
  assert.equal(b.calls.length, 0);
});
