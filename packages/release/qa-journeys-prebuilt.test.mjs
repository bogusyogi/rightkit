// GB-79 (GATE test stage E1/E6/E11; capability `journeys-prebuilt-input`): `rightkit-qa journeys run` on a snapshot stage is the gate's build-once route. The gate asks QA what it
// would build, carries that build (one signed, ledgered broker request), signs the products, writes an attestation and runs QA directly on the verified input; QA never compiles.
// The broker is the REAL installed RustRight broker (an isolated instance, test-support/real-broker.mjs) over the gate's real snapshot of a real git checkout, and the carried build
// is a real `cargo test --no-run` of a real crate. `rightkit-qa` is a recording stand-in for the QA CLI (not the broker) that speaks the CLI's real `--print-build-plan` / `--from-cargo-json` /
// `--prebuilt-attestation` contract (the Rust side is tested in crates/rightkit-qa/src/journeys/prebuilt.rs).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch, qaBuildReason } from "./build-launch.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, brokerLeaseState, markCarriedSend } from "./build-ownership.mjs";
import { enrollBrokerPins } from "./broker-pins.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { ATTESTATION_KEYS, BUILD_PLAN_SCHEMA, JOURNEYS_PREBUILT_CAPABILITY, PREBUILT_SCHEMA, isJourneysRun, journeysRunRefusal, qaPrebuiltCapability, runJourneysPrebuilt } from "./qa-journeys-prebuilt.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { brokerSection, createSnapshotBroker } from "./snapshot-broker.mjs";
import { spawnSync } from "./background-process.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the carried adapter PATH and QA stand-in are POSIX only" : false;
const sha = (data) => createHash("sha256").update(data).digest("hex");
const JOURNEYS = ["journeys", "run", "--root", ".", "--id", "J-001"];

// ---- the real broker host: the installed RustRight broker (an isolated instance), a real git checkout, the gate's real snapshot of it, the enrolled pins of THAT broker, and the production snapshot broker over them ----
const HANDLE = "own-7";
const PLATFORM_KEY = process.platform === "win32" ? "win32" : process.platform === "darwin" ? "darwin" : "linux";
const LOCK = (name) => `version = 4\n\n[[package]]\nname = "${name}"\nversion = "0.1.0"\n`;
const BASE_FILES = {
  ".gitignore": "target/\nnode_modules/\n",
  "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\nresolver = "2"\n', "Cargo.lock": LOCK("a"), "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\nedition = "2021"\n', "crates/a/src/lib.rs": "\n",
  "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\nedition = "2021"\n', "native/Cargo.lock": LOCK("b"), "native/src/lib.rs": "\n",
  "packages/release/package.json": '{"name":"@x/release","version":"0.0.0"}\n',
};
const gitIn = (cwd, ...args) => { const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd, encoding: "utf8" }); if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`); return String(r.stdout).trim(); };
/** The enrolled pins of the broker instance `rb`: its own attestation key (public fields only), host and broker digest as its own `doctor --json` reports them. */
function instancePins(rb) {
  const key = JSON.parse(fs.readFileSync(path.join(rb.controlRoot, "attestation", "key.json"), "utf8"));
  const run = rb.run(["doctor", "--json", "--no-scan"], {});
  const doctor = JSON.parse(run.stdout);
  const find = (value, name) => { if (value && typeof value === "object") { if (typeof value[name] === "string") return value[name]; for (const inner of Object.values(value)) { const hit = find(inner, name); if (hit) return hit; } } return null; };
  const host = find(doctor, "hostName") ?? find(doctor, "host"); const brokerDigest = find(doctor, "brokerDigest");
  if (!host || !brokerDigest) throw new Error(`the isolated broker's doctor report names no host or brokerDigest to enroll: ${run.stdout.slice(0, 300)}`);
  return { publicKey: key.publicKey, keyId: key.keyId, host, brokerDigest };
}
/** `files` are committed over BASE_FILES; the snapshot is the gate's own (syncSnapshot) inside the broker's data root; `sb` is createSnapshotBroker's. */
async function realBrokerHost(t, name, files = {}) {
  const rb = await startRealBroker({ name });
  t.after(() => rb.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `rk-${name}-`)));
  t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const checkout = path.join(root, "checkout"); fs.mkdirSync(checkout);
  for (const [rel, body] of Object.entries({ ...BASE_FILES, ...files })) { const f = path.join(checkout, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); }
  gitIn(checkout, "init", "-q", "-b", "main"); gitIn(checkout, "add", "-A"); gitIn(checkout, "commit", "-q", "-m", "c");
  const snap = syncSnapshot({ checkout, rev: "HEAD", root: path.join(rb.dataRoot, "snapshots"), managedRoot: rb.dataRoot });
  enrollBrokerPins({ dataRoot: rb.dataRoot, platform: PLATFORM_KEY, ...instancePins(rb) });
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle: HANDLE, broker: { submitted: false } }));
  const submits = [];
  const sb = createSnapshotBroker({
    env: rb.env, platform: process.platform, checkout, commit: { sha: snap.commit, tree: snap.tree }, snapshot: { path: snap.path, expectedTree: snap.tree },
    ownership: { handle: HANDLE, markBrokerSubmit: (callHandle) => { submits.push(callHandle); return markCarriedSend({ file: ownership, handle: HANDLE, callHandle }); } },
    client: rb.clientPath, gateFile: path.join(checkout, "x", "rightkit-gate.mjs"), dataRoot: rb.dataRoot, lease: (q) => brokerLeaseState(q),
  });
  return { rb, root, checkout, snapshot: snap.path, commit: { sha: snap.commit, tree: snap.tree }, sb, submits, ownershipEnv: { [OWNERSHIP_ENV]: HANDLE, [OWNERSHIP_FILE_ENV]: ownership } };
}

const QA_FILES = (broken) => ({
  "Cargo.toml": '[workspace]\nmembers = ["crates/a", "crates/fx"]\nresolver = "2"\n',
  "Cargo.lock": `${LOCK("a")}\n[[package]]\nname = "fx"\nversion = "0.1.0"\n`,
  "crates/fx/Cargo.toml": '[package]\nname = "fx"\nversion = "0.1.0"\nedition = "2021"\n[features]\nqa = []\n[[bin]]\nname = "fxapp"\npath = "src/main.rs"\n',
  "crates/fx/src/lib.rs": broken ? 'compile_error!("the carried journeys build fails");\n' : "\n",
  "crates/fx/src/main.rs": "fn main() {}\n",
  "crates/fx/tests/journeys_adapter.rs": 'const _APP: &str = env!("CARGO_BIN_EXE_fxapp");\n#[test]\nfn adapter() {}\n',
});

/** A snapshot stage on the real broker with a carrier and a stand-in `rightkit-qa`; `artifacts: false` declares no artifact, `broken` makes the carried build fail for real. */
async function host(t, { artifacts = true, broken = false } = {}) {
  const fx = await realBrokerHost(t, "qa-journeys", QA_FILES(broken));
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const log = path.join(fx.root, "qa-log.jsonl");
  const planFile = path.join(fx.root, "plan.json");
  const plan = { schema: BUILD_PLAN_SCHEMA, name: "fx", command: ["rustright", "cargo", "test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"], cwd: fx.snapshot, artifacts: artifacts ? [{ role: "app", marker: "required", cargo_target: "fxapp" }, { role: "adapter", marker: "not_applicable", cargo_target: "journeys_adapter" }] : [] };
  fs.writeFileSync(planFile, JSON.stringify(plan));
  const qa = path.join(fx.root, "rightkit-qa");
  fs.writeFileSync(qa, [
    `#!${process.execPath}`,
    'const fs = require("node:fs");',
    "const a = process.argv.slice(2);",
    `const log = (o) => fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify(o) + "\\n");`,
    'const value = (flag) => { const i = a.indexOf(flag); return i < 0 ? null : a[i + 1]; };',
    'if (a.includes("--help")) { process.stdout.write(process.env.FAKE_QA_HELP ?? "usage: rightkit-qa journeys run (capability journeys-prebuilt-input)\\n"); process.exit(0); }',
    `const plan = JSON.parse(fs.readFileSync(${JSON.stringify(planFile)}, "utf8"));`,
    'if (a.includes("--print-build-plan")) {',
    '  log({ mode: "plan", args: a });',
    '  const from = value("--from-cargo-json");',
    '  if (from) {',
    '    const lines = fs.readFileSync(from, "utf8").split("\\n").filter((l) => l.startsWith("{")).map((l) => JSON.parse(l));',
    '    for (const art of plan.artifacts) { const hit = lines.find((l) => l.reason === "compiler-artifact" && l.executable && l.target.name === art.cargo_target); art.executable = hit ? hit.executable : null; }',
    '    if (process.env.FAKE_QA_ELSEWHERE && plan.artifacts[0]) plan.artifacts[0].executable = process.env.FAKE_QA_ELSEWHERE;',
    "  }",
    "  process.stdout.write(JSON.stringify(plan) + \"\\n\"); process.exit(Number(process.env.FAKE_QA_PLAN_EXIT ?? 0));",
    "}",
    'const att = value("--prebuilt-attestation");',
    'log({ mode: "run", args: a, cwd: process.cwd(), listingText: fs.readFileSync(value("--from-cargo-json"), "utf8"), attestation: fs.readFileSync(att, "utf8"), env: { evidence: process.env.RIGHTKIT_QA_SIGNING_EVIDENCE ?? null, app: process.env.RIGHTKIT_QA_APP_BINARY ?? null, signed: process.env.RIGHTKIT_QA_SIGNED_FILES ?? null } });',
    "process.exit(Number(process.env.FAKE_QA_EXIT ?? 0));",
    "",
  ].join("\n"), { mode: 0o755 });
  const env = { ...fx.rb.env, PATH: `${fx.root}:${process.env.PATH}`, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", RK_GATE_CHECKOUT: fx.checkout, ...fx.ownershipEnv, ...installed.env };
  const map = JSON.parse(fs.readFileSync(installed.env[CARRIED_CARGO_MAP_ENV], "utf8"));
  /** A signing stand-in with the real sign step's shape: copies each named file from the receipt's output directory, "signs" the copy, writes the evidence. */
  const signs = [];
  const signStep = (spec) => {
    signs.push(spec);
    const signedDir = fs.mkdtempSync(path.join(fx.root, "signed-"));
    const files = spec.sign.files.map((file) => {
      const dir = spec.outputDirsOf(file.from)?.[0];
      assert.ok(dir, "the sign step reads the receipt's output directory");
      const source = path.join(dir, ...file.path.split("/"));
      const copy = path.join(signedDir, ...file.path.split("/"));
      fs.mkdirSync(path.dirname(copy), { recursive: true });
      const bytes = Buffer.concat([fs.readFileSync(source), Buffer.from("SIGNED")]);
      fs.writeFileSync(copy, bytes);
      return { file: file.path, signed: copy, sha256Before: sha(fs.readFileSync(source)), sha256After: sha(bytes) };
    });
    const evidenceFile = path.join(signedDir, "signing-evidence.json");
    fs.writeFileSync(evidenceFile, JSON.stringify({ schema: 1, step: spec.name, files }));
    return { appBinary: files[0].signed, signedFiles: Object.fromEntries(files.map((f) => [f.file, f.signed])), evidence: files, evidenceFile, signedDir };
  };
  const run = (over = {}) => runJourneysPrebuilt({ qa, args: JOURNEYS, cwd: fx.snapshot, env, map, signStep, loadConfig: () => ({ app: "fx" }), log: () => {}, ...over });
  const records = () => (fs.existsSync(log) ? fs.readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  const CLIENT = fx.rb.clientPath;
  return { fx, installed, qa, env, map, run, records, signs, signStep, entries, doorHost: { platform: "darwin", cwd: fx.snapshot, exists: (p) => p === CLIENT || p === qa, which: (c) => (c === "rightkit-qa" ? [qa] : c === "rustright" ? [CLIENT] : []), env: { ...env }, stepEnv: {} } };
}

test("[GB-79] the gate carries the declared build once on the real broker, signs the products and hands QA the verified listing and attestation; QA is never asked to build", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const { status, attestation } = h.run();
  assert.equal(status, 0);
  // QA was asked twice what it would build (nothing built), then run once, directly, on the verified input.
  const records = h.records();
  assert.deepEqual(records.map((r) => r.mode), ["plan", "plan", "run"]);
  assert.deepEqual(records[0].args, [...JOURNEYS, "--print-build-plan"]);
  assert.equal(records[1].args.at(-2), "--from-cargo-json");
  const run = records[2];
  assert.deepEqual(run.args.slice(0, JOURNEYS.length), JOURNEYS);
  assert.deepEqual(run.args.slice(JOURNEYS.length).filter((w) => !w.startsWith("/")), ["--from-cargo-json", "--prebuilt-attestation"]);
  assert.ok(!run.args.some((w) => /^--build/.test(w) || w === "cargo" || w === "rustright"), "no hidden QA rebuild: QA is started with no build flag and no build tool");
  // Exactly one carried broker call: the declared build (without rustright), receipt verified, ledgered; no adapter job.
  const { entries, problems } = h.entries();
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.filter((e) => e.mode === "broker").map((e) => e.status), ["verified"]);
  assert.equal(entries.filter((e) => e.adapter).length, 0);
  const receipt = await h.fx.sb.receiptOf(entries[0].requestId);
  assert.deepEqual(receipt.provenance.payload.request.requestedArgv.filter((w) => w !== "--locked"), ["test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"]);
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate verifies the build's receipt again itself");
  // The attestation binds the request, the listing, the command and every output QA will launch.
  const written = JSON.parse(run.attestation);
  assert.deepEqual(Object.keys(written), [...ATTESTATION_KEYS]);
  assert.deepEqual(written, attestation);
  assert.equal(written.schema, PREBUILT_SCHEMA);
  assert.equal(written.request_id, entries[0].requestId, "the attested request is the broker request the receipt was verified for");
  assert.match(written.request_sha256, /^[0-9a-f]{64}$/);
  assert.equal(written.listing_sha256, sha(run.listingText));
  assert.deepEqual(written.command, ["cargo", "test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"], "the carried command, broker program removed (QA compares it with its manifest's declared build)");
  const outputDir = h.installed.outputDirs()[0];
  assert.equal(written.outputs.length, 2);
  assert.deepEqual(written.outputs.map((o) => path.basename(o.path).split("-")[0]), ["fxapp", "journeys_adapter"]);
  for (const o of written.outputs) { assert.ok(o.path.startsWith(`${fs.realpathSync(outputDir)}${path.sep}`) || o.path.startsWith(`${outputDir}${path.sep}`), "an output of the receipt's output directory"); assert.equal(o.sha256, sha(fs.readFileSync(o.path)), "the digest of the broker's own bytes"); }
  // Only the product was signed, from the receipt's output directory; the signing evidence reaches QA's environment.
  assert.equal(h.signs.length, 1);
  assert.deepEqual(h.signs[0].sign.files, [{ from: "journeys-build", path: "fxapp" }]);
  assert.match(run.env.evidence, /signing-evidence\.json$/);
  assert.match(run.env.app, /fxapp$/);
  // Nothing outlives the run: the listing, the attestation and the signed copies are removed.
  const listingFile = run.args[run.args.indexOf("--from-cargo-json") + 1];
  assert.equal(fs.existsSync(listingFile), false);
  assert.equal(fs.existsSync(run.args.at(-1)), false);
  assert.equal(fs.existsSync(path.dirname(run.env.evidence)), false);
  // QA's own exit status is the step's.
  assert.equal(h.run({ env: { ...h.env, FAKE_QA_EXIT: "7" } }).status, 7);
});

test("[GB-79] negative: a product that cannot be signed refuses, and QA never runs on unsigned bytes (the real sign step refuses off macOS and Windows)", { skip: POSIX }, async (t) => {
  const h = await host(t);
  assert.throws(() => h.run({ signStep: () => { throw Object.assign(new Error("SIGN_FAILED: codesign failed"), { code: "SIGN_FAILED" }); } }), (e) => e.code === "SIGN_FAILED" && /unsigned product is never launched/.test(e.message));
  assert.deepEqual(h.records().map((r) => r.mode), ["plan", "plan"], "QA was never started");
  const real = await host(t);
  assert.throws(() => real.run({ signStep: undefined, platform: "linux" }), (e) => /unsigned product is never launched/.test(e.message));
  assert.equal(real.records().some((r) => r.mode === "run"), false);
});

test("[GB-79] negative: a carried build the real broker signs as FAILED, an unreadable plan and a listing outside the output directory never reach QA", { skip: POSIX }, async (t) => {
  const failed = await host(t, { broken: true });
  assert.throws(() => failed.run(), (e) => /RK_QA_JOURNEYS_BUILD/.test(e.message) && e.status !== 0);
  assert.equal(failed.records().some((r) => r.mode === "run"), false, "a build with no verified success never feeds QA");
  const noPlan = await host(t);
  assert.throws(() => noPlan.run({ env: { ...noPlan.env, FAKE_QA_PLAN_EXIT: "2" } }), /RK_QA_JOURNEYS_PLAN/);
  assert.deepEqual(noPlan.fx.submits, [], "nothing is carried when QA cannot say what to build");
  const outside = await host(t);
  const elsewhere = path.join(outside.fx.root, "elsewhere-bin");
  fs.writeFileSync(elsewhere, "x");
  assert.throws(() => outside.run({ env: { ...outside.env, FAKE_QA_ELSEWHERE: elsewhere } }), (e) => e.code === "RK_QA_JOURNEYS_PLACE" && /not inside the receipt's output directory/.test(e.message));
  assert.equal(outside.records().some((r) => r.mode === "run"), false);
});

test("[GB-79] negative: an empty journey set is never green: no declared artifact refuses before anything is carried; the step may not name its own listing, attestation or plan flag", { skip: POSIX }, async (t) => {
  const empty = await host(t, { artifacts: false });
  assert.throws(() => empty.run(), (e) => e.code === "RK_QA_JOURNEYS_EMPTY" && /never green/.test(e.message));
  assert.deepEqual(empty.fx.submits, [], "nothing was built for an empty set");
  const h = await host(t);
  for (const extra of [["--from-cargo-json", "/x/l.jsonl"], ["--from-cargo-json=/x/l.jsonl"], ["--prebuilt-attestation", "/x/a.json"], ["--print-build-plan"]]) {
    assert.throws(() => h.run({ args: [...JOURNEYS, ...extra] }), (e) => e.code === "RK_QA_JOURNEYS_FLAG", extra.join(" "));
    assert.ok(qaBuildReason(["journeys", "run", ...extra], { carried: true }), `the door refuses ${extra.join(" ")}`);
  }
  assert.deepEqual(h.fx.submits, []);
  assert.equal(journeysRunRefusal(JOURNEYS), null);
  assert.ok(qaBuildReason(["journeys", "run"]), "outside a gate-carried stage it is still a build");
  assert.equal(qaBuildReason(["journeys", "run", "--root", "."], { carried: true }), null, "on a gate-carried stage the gate carries the build");
  assert.ok(qaBuildReason(["journeys", "run", "--build"], { carried: true }), "a build flag is still a build");
  assert.equal(isJourneysRun(["journeys", "receipt"]), false);
});

test("[GB-79] the door hands a journeys step to the build-once entry when QA advertises the capability, and refuses naming it when it does not", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const launch = buildLaunch("rightkit-qa", JOURNEYS, { ...h.doorHost, qaPrebuiltCapability: () => true });
  assert.equal(launch.command, process.execPath);
  assert.match(launch.args[0], /qa-journeys-prebuilt-run\.mjs$/);
  assert.deepEqual(launch.args.slice(1), [h.qa, "--", ...JOURNEYS]);
  assert.throws(() => buildLaunch("rightkit-qa", JOURNEYS, { ...h.doorHost, qaPrebuiltCapability: () => false }), (e) => e.code === BUILD_LAUNCH_REFUSED && new RegExp(`capability "${JOURNEYS_PREBUILT_CAPABILITY}"`).test(e.message));
  // The default probe asks the installed binary.
  assert.equal(buildLaunch("rightkit-qa", JOURNEYS, h.doorHost).command, process.execPath);
  assert.equal(qaPrebuiltCapability(h.qa), true);
  assert.equal(qaPrebuiltCapability(h.qa, { env: { ...process.env, FAKE_QA_HELP: "usage: rightkit-qa journeys run\n" } }), false, "a QA that does not name the capability has no such mode");
  assert.equal(qaPrebuiltCapability(path.join(h.fx.root, "missing-qa")), false);
  assert.throws(() => buildLaunch("rightkit-qa", [...JOURNEYS, "--from-cargo-json", "/x"], { ...h.doorHost, qaPrebuiltCapability: () => true }), (e) => e.code === BUILD_LAUNCH_REFUSED);
});
