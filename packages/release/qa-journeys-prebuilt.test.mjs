// GB-79 (GATE test stage E1/E6/E11; capability `journeys-prebuilt-input`): `rightkit-qa journeys run` on a snapshot stage is the gate's build-once route. The gate asks QA what it
// would build, carries that build (one signed, ledgered broker request), signs the products, writes an attestation and runs QA directly on the verified input; QA never compiles.
// The broker is the fixture (test-support/fake-broker.mjs); `rightkit-qa` is a recording stand-in that speaks the CLI's real `--print-build-plan` / `--from-cargo-json` /
// `--prebuilt-attestation` contract (the Rust side is tested in crates/rightkit-qa/src/journeys/prebuilt.rs).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch, qaBuildReason } from "./build-launch.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { ATTESTATION_KEYS, BUILD_PLAN_SCHEMA, JOURNEYS_PREBUILT_CAPABILITY, PREBUILT_SCHEMA, isJourneysRun, journeysRunRefusal, qaPrebuiltCapability, runJourneysPrebuilt } from "./qa-journeys-prebuilt.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client and QA stand-in are POSIX only" : false;
const sha = (data) => createHash("sha256").update(data).digest("hex");
const CLIENT = "/managed/agent-bin/rustright";
const JOURNEYS = ["journeys", "run", "--root", ".", "--id", "J-001"];

/** The listing the carried build prints: the app (a product) and the journeys adapter (a test binary), both inside the receipt's output directory. */
const listingOf = (dir) => [
  JSON.stringify({ reason: "compiler-artifact", package_id: "path+file:///x/crates/fxapp#0.1.0", target: { name: "fxapp", kind: ["bin"] }, profile: { test: false }, features: ["qa"], executable: path.join(dir, "fxapp"), fresh: false }),
  JSON.stringify({ reason: "compiler-artifact", package_id: "path+file:///x/fx#fx@0.1.0", target: { name: "journeys_adapter", kind: ["test"] }, profile: { test: true }, features: ["qa"], executable: path.join(dir, "deps", "journeys_adapter-1234"), fresh: false }),
  JSON.stringify({ reason: "build-finished", success: true }),
].join("\n");

/** A snapshot stage with a fake broker, a carrier, built files in the receipt's output directory and a stand-in `rightkit-qa`. */
function host(t, { artifacts = true } = {}) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  fs.mkdirSync(path.join(fx.outputDir, "deps"), { recursive: true });
  fs.writeFileSync(path.join(fx.outputDir, "fxapp"), "app-bytes");
  fs.writeFileSync(path.join(fx.outputDir, "deps", "journeys_adapter-1234"), "adapter-bytes");
  const listing = listingOf(fx.outputDir);
  fx.setBehavior({ stdout: `${listing}\n` });
  const log = path.join(fx.root, "qa-log.jsonl");
  const planFile = path.join(fx.root, "plan.json");
  const plan = { schema: BUILD_PLAN_SCHEMA, name: "fx", command: ["rustright", "cargo", "test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"], cwd: fx.snapshot, artifacts: artifacts ? [{ role: "app", marker: "required", cargo_package: "fxapp" }, { role: "adapter", marker: "not_applicable", cargo_target: "journeys_adapter" }] : [] };
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
    '    for (const art of plan.artifacts) { const hit = lines.find((l) => l.reason === "compiler-artifact" && (art.cargo_target ? l.target.name === art.cargo_target : l.package_id.includes(art.cargo_package))); art.executable = hit ? hit.executable : null; }',
    "  }",
    "  process.stdout.write(JSON.stringify(plan) + \"\\n\"); process.exit(Number(process.env.FAKE_QA_PLAN_EXIT ?? 0));",
    "}",
    'const att = value("--prebuilt-attestation");',
    'log({ mode: "run", args: a, cwd: process.cwd(), listingText: fs.readFileSync(value("--from-cargo-json"), "utf8"), attestation: fs.readFileSync(att, "utf8"), env: { evidence: process.env.RIGHTKIT_QA_SIGNING_EVIDENCE ?? null, app: process.env.RIGHTKIT_QA_APP_BINARY ?? null, signed: process.env.RIGHTKIT_QA_SIGNED_FILES ?? null } });',
    "process.exit(Number(process.env.FAKE_QA_EXIT ?? 0));",
    "",
  ].join("\n"), { mode: 0o755 });
  const env = { PATH: `${fx.root}:${process.env.PATH}`, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", RK_GATE_CHECKOUT: fx.checkout, ...fx.ownershipEnv, ...installed.env };
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
  return { fx, installed, qa, env, map, run, records, signs, signStep, listing, doorHost: { platform: "darwin", cwd: fx.snapshot, exists: (p) => p === CLIENT || p === qa, which: (c) => (c === "rightkit-qa" ? [qa] : c === "rustright" ? [CLIENT] : []), env: { ...env }, stepEnv: {} } };
}

test("[GB-79] the gate carries the declared build once, signs the products and hands QA the verified listing and attestation; QA is never asked to build", { skip: POSIX }, async (t) => {
  const h = host(t);
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
  const calls = h.fx.calls();
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].argv.filter((w) => w !== "--locked"), ["test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"]);
  assert.equal(h.fx.adapterCalls().length, 0);
  const { entries, problems } = readLedger(h.installed.ledgerDir, UNSCOPED);
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.filter((e) => e.mode === "broker").map((e) => e.status), ["verified"]);
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate verifies the build's receipt again itself");
  // The attestation binds the request, the listing, the command and every output QA will launch.
  const written = JSON.parse(run.attestation);
  assert.deepEqual(Object.keys(written), [...ATTESTATION_KEYS]);
  assert.deepEqual(written, attestation);
  assert.equal(written.schema, PREBUILT_SCHEMA);
  assert.equal(written.request_id, calls[0].id, "the attested request is the broker request the receipt was verified for");
  assert.match(written.request_sha256, /^[0-9a-f]{64}$/);
  assert.equal(written.listing_sha256, sha(run.listingText));
  assert.deepEqual(written.command, ["cargo", "test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"], "the carried command, broker program removed (QA compares it with its manifest's declared build)");
  assert.deepEqual(written.outputs, [
    { path: path.join(h.fx.outputDir, "fxapp"), sha256: sha("app-bytes") },
    { path: path.join(h.fx.outputDir, "deps", "journeys_adapter-1234"), sha256: sha("adapter-bytes") },
  ]);
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

test("[GB-79] negative: a product that cannot be signed refuses, and QA never runs on unsigned bytes (the real sign step refuses off macOS and Windows)", { skip: POSIX }, (t) => {
  const h = host(t);
  assert.throws(() => h.run({ signStep: () => { throw Object.assign(new Error("SIGN_FAILED: codesign failed"), { code: "SIGN_FAILED" }); } }), (e) => e.code === "SIGN_FAILED" && /unsigned product is never launched/.test(e.message));
  assert.deepEqual(h.records().map((r) => r.mode), ["plan", "plan"], "QA was never started");
  const real = host(t);
  assert.throws(() => real.run({ signStep: undefined, platform: "linux" }), (e) => /unsigned product is never launched/.test(e.message));
  assert.equal(real.records().some((r) => r.mode === "run"), false);
});

test("[GB-79] negative: a failed carried build, an unverified receipt, an unreadable plan and a listing outside the output directory never reach QA", { skip: POSIX }, (t) => {
  const failed = host(t);
  failed.fx.setBehavior({ exit: 101, stdout: "" });
  assert.throws(() => failed.run(), (e) => /RK_QA_JOURNEYS_BUILD/.test(e.message) && e.status !== 0);
  assert.equal(failed.records().some((r) => r.mode === "run"), false);
  const unverified = host(t);
  unverified.fx.setBehavior({ tamper: "failed", stdout: `${unverified.listing}\n` });
  assert.throws(() => unverified.run(), /RK_QA_JOURNEYS_BUILD/);
  assert.equal(unverified.records().some((r) => r.mode === "run"), false, "a build with no verified receipt never feeds QA");
  const noPlan = host(t);
  assert.throws(() => noPlan.run({ env: { ...noPlan.env, FAKE_QA_PLAN_EXIT: "2" } }), /RK_QA_JOURNEYS_PLAN/);
  assert.equal(noPlan.fx.calls().length, 0, "nothing is carried when QA cannot say what to build");
  const outside = host(t);
  const elsewhere = path.join(outside.fx.root, "elsewhere-bin");
  fs.writeFileSync(elsewhere, "x");
  outside.fx.setBehavior({ stdout: `${listingOf(path.dirname(elsewhere)).replaceAll(path.join(path.dirname(elsewhere), "deps"), path.join(outside.fx.outputDir, "deps"))}\n` });
  assert.throws(() => outside.run(), (e) => e.code === "RK_QA_JOURNEYS_PLACE" && /not inside the receipt's output directory/.test(e.message));
  assert.equal(outside.records().some((r) => r.mode === "run"), false);
});

test("[GB-79] negative: an empty journey set is never green: no declared artifact refuses before anything is carried; the step may not name its own listing, attestation or plan flag", { skip: POSIX }, (t) => {
  const empty = host(t, { artifacts: false });
  assert.throws(() => empty.run(), (e) => e.code === "RK_QA_JOURNEYS_EMPTY" && /never green/.test(e.message));
  assert.equal(empty.fx.calls().length, 0, "nothing was built for an empty set");
  const h = host(t);
  for (const extra of [["--from-cargo-json", "/x/l.jsonl"], ["--from-cargo-json=/x/l.jsonl"], ["--prebuilt-attestation", "/x/a.json"], ["--print-build-plan"]]) {
    assert.throws(() => h.run({ args: [...JOURNEYS, ...extra] }), (e) => e.code === "RK_QA_JOURNEYS_FLAG", extra.join(" "));
    assert.ok(qaBuildReason(["journeys", "run", ...extra], { carried: true }), `the door refuses ${extra.join(" ")}`);
  }
  assert.equal(h.fx.calls().length, 0);
  assert.equal(journeysRunRefusal(JOURNEYS), null);
  assert.ok(qaBuildReason(["journeys", "run"]), "outside a gate-carried stage it is still a build");
  assert.equal(qaBuildReason(["journeys", "run", "--root", "."], { carried: true }), null, "on a gate-carried stage the gate carries the build");
  assert.ok(qaBuildReason(["journeys", "run", "--build"], { carried: true }), "a build flag is still a build");
  assert.equal(isJourneysRun(["journeys", "receipt"]), false);
});

test("[GB-79] the door hands a journeys step to the build-once entry when QA advertises the capability, and refuses naming it when it does not", { skip: POSIX }, (t) => {
  const h = host(t);
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
