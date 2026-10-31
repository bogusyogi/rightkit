// rightkit#1297, #1301: `rightkit-gate --to <stage> --route` answers which route a stage takes on this host and whether it refuses, and
// runs nothing. 0.6.12 shipped a production entry whose route refused every dev and test build on the Mac and the Dell: every door
// test injects its own carrier and the release canary only ran `doctor`, so nothing exercised the real entry. 0.6.14 wires the
// production carrier on the Mac and the Dell (right-release's own Cargo calls and the Tauri CLI's are carried), so every stage takes the
// snapshot route; `runStages` takes its route and refusal from `routePlan` itself, and the parity test below proves the probe and a real
// run cannot drift. docs/GATE-CONTRACT.md §1 "Route probe", docs/PIPELINE-ARCHITECTURE.md P10.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { REQUIRED_BROKER_CAPABILITIES, ROUTE_REFUSAL_CODES, ROUTE_SCHEMA, brokerCapabilityState, main, productionEntryDeps, queryBrokerCapabilities, quietLeaseLane, routePlan, snapshotRouteRefusal } from "./bin/rightkit-gate.mjs";
import net from "node:net";
import { resolveExecutor as realResolveExecutor } from "./gate-executor.mjs";
import { createHash } from "node:crypto";
import { LINUX_BROKER_CLIENT_ENV, LINUX_BROKER_CLIENT_SHA256_ENV, LINUX_BROKER_SOCKET_ENV } from "./managed-cargo.mjs";
import { compareCells } from "../../scripts/ci/rk-release-canary.mjs";
import { parseRoute, routeStagesFor, runRouteProbe } from "../../scripts/ci/rk-release-canary-leg.mjs";
import { PRODUCTION_SNAPSHOT_CARRIER } from "./snapshot-broker.mjs";
import { SIGNING_REFUSAL_CODES } from "./suite-signing-policy.mjs";

// The codes the route and carrier rules decide. The signing refusals (rightkit#1303) read the commit's config, so suite-signing-route.test.mjs derives and
// exercises those and proves the probe and a real run agree on them; the list a probe can answer with is these plus those.
const ROUTE_BASE_CODES = ROUTE_REFUSAL_CODES.filter((code) => !SIGNING_REFUSAL_CODES.includes(code));

const HERE = path.dirname(fileURLToPath(import.meta.url));
const GATE_FILE = path.join(HERE, "bin", "rightkit-gate.mjs");
const tmp = (t, prefix, base = os.tmpdir()) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(base, prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });
const brokerExecutor = () => ({ kind: "broker", cargo: ["rustright", "cargo"] });
const directExecutor = () => ({ kind: "direct", cargo: ["cargo"] });
const refuseExecutor = () => ({ kind: "refuse", cargo: null, problem: "the RightKit broker is not reachable" });
const OK_CAPS = () => ({ ok: true, capabilities: [...REQUIRED_BROKER_CAPABILITIES] });
const SIDECAR_CONFIG = '{"sidecars":[{"name":"a"}]}\n';
const TAURI_FILES = { "README.md": "r\n", "right-release.config.json": SIDECAR_CONFIG, "src-tauri/tauri.conf.json": "{}\n", "src-tauri/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n' };

/** A one-commit repository of `files`, and a runner of the real `main` that proves the probe touches nothing but git and `which`. */
function fixture(t, files = { "README.md": "r\n", "right-release.config.json": "{}\n" }) {
  const dir = tmp(t, "rk-route-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  for (const [rel, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c");
  const sha = git(repo, "rev-parse", "HEAD").stdout.trim();
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  for (const k of ["GITHUB_ACTIONS", "TAURI_CONFIG", "RIGHTKIT_BUILD_OWNERSHIP", "RIGHTKIT_BUILD_OWNERSHIP_FILE"]) delete env[k];
  const spawned = [];
  const spawn = (c, a, opts) => { spawned.push({ call: [c, ...a], env: opts?.env }); return c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" }; };
  const forbidden = (name) => () => { throw new Error(`the route probe called ${name}`); };
  const run = async (args, extra = {}) => {
    const out = []; const err = [];
    const code = await main(args, {
      cwd: repo, platform: "linux", env, spawn, snapshotRoot: path.join(dir, "snaps"), out: (m) => out.push(m), err: (m) => err.push(m), resolveExecutor: directExecutor,
      acquireOwnership: forbidden("acquireOwnership"), syncSnapshot: forbidden("syncSnapshot"), runCheck: forbidden("runCheck"), runCompile: forbidden("runCompile"), spawnInherit: forbidden("spawnInherit"),
      spawnCapture: forbidden("spawnCapture"), readReceipt: forbidden("readReceipt"), writeReceipt: forbidden("writeReceipt"), appCheck: forbidden("appCheck"), ...extra,
    });
    const line = out.find((l) => l.startsWith("{"));
    return { code, out, err: err.join("\n"), json: line ? JSON.parse(line) : null };
  };
  return { dir, repo, sha, env, spawned, run, write: (rel, body) => { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); } };
}

// ---- routePlan: the order the stages decide in ------------------------------------------------------------------------------

const plan = (o, over = {}) => routePlan({ resolveExecutor: over.executor ? () => over.executor : brokerExecutor, env: {}, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", snapshotBroker: { kind: "injected" }, ...o }, { to: "compile", stages: ["check", "compile"], devTest: false, executor: brokerExecutor(), ...over });

test("routePlan: hosted CI builds the runner's checkout; every other host takes the snapshot route (there is no checkout route)", () => {
  assert.deepEqual(plan({ platform: "linux", env: { GITHUB_ACTIONS: "true" } }, { executor: directExecutor() }), { route: "hosted", refusal: null });
  for (const platform of ["darwin", "win32", "linux"]) {
    assert.deepEqual(plan({ platform }), { route: "snapshot", refusal: null }, platform);
    assert.deepEqual(plan({ platform }, { to: "dev", devTest: true, stages: [] }), { route: "snapshot", refusal: null }, `${platform} dev`);
  }
  // Hosted wins over a snapshot-route host: the runner's checkout is the commit, so even TAURI_CONFIG is no refusal there.
  assert.equal(plan({ platform: "linux", env: { GITHUB_ACTIONS: "true", TAURI_CONFIG: "{}" } }, { executor: directExecutor() }).refusal, null);
});

test("routePlan: the snapshot route's refusals, in order: TAURI_CONFIG, then the carrier, then the broker's capabilities, then an unreachable broker", () => {
  assert.equal(plan({ platform: "linux", env: { TAURI_CONFIG: "{}" } }, { executor: directExecutor() }).refusal.code, "RK_TAURI_CONFIG_OVERRIDE");
  assert.equal(plan({ platform: "linux", env: { TAURI_CONFIG: "{}" } }, { to: "check", stages: ["check"], executor: directExecutor() }).refusal.code, "RK_TAURI_CONFIG_OVERRIDE", "even --to check refuses it");
  // A gate started without the carrier (never production's) refuses every stage but check.
  const none = { snapshotBroker: undefined };
  assert.equal(plan({ platform: "darwin", ...none }).refusal.code, "RK_SNAPSHOT_CARRIER_NOT_WIRED");
  assert.equal(plan({ platform: "darwin", ...none }, { to: "dev", devTest: true, stages: [] }).refusal.code, "RK_SNAPSHOT_CARRIER_NOT_WIRED");
  assert.equal(plan({ platform: "darwin", ...none }, { to: "check", stages: ["check"] }).refusal, null, "--to check compiles nothing");
  assert.equal(plan({ platform: "linux", ...none }, { executor: directExecutor() }).refusal, null, "a direct host needs no carrier");
  // With the carrier a Mac's dev, test, compile, package and qualify all take the snapshot route.
  for (const [to, stages] of [["dev", []], ["test", []], ["compile", ["check", "compile"]], ["package", ["check", "compile", "package"]], ["qualify", ["check", "compile", "package", "qualify"]]]) {
    assert.deepEqual(plan({ platform: "darwin" }, { to, devTest: stages.length === 0, stages }), { route: "snapshot", refusal: null }, to);
  }
  // The Dell carries the Tauri CLI's own cargo through a stage grant (rustright#188), so no stage of any commit is refused there for it.
  for (const [to, devTest, stages] of [["dev", true, []], ["test", true, []], ["compile", false, ["check", "compile"]], ["package", false, ["check", "compile", "package"]], ["qualify", false, ["check", "compile", "package", "qualify"]]]) {
    assert.deepEqual(plan({ platform: "win32" }, { to, devTest, stages }), { route: "snapshot", refusal: null }, `win32 ${to}`);
  }
  // An unreachable broker is named on the snapshot route by the carrier rule, elsewhere last.
  assert.equal(plan({ platform: "win32", snapshotBroker: undefined }, { to: "dev", devTest: true, stages: [] }).refusal.code, "RK_SNAPSHOT_CARRIER_NOT_WIRED");
  assert.equal(plan({ platform: "darwin", resolveExecutor: refuseExecutor }, { executor: refuseExecutor() }).refusal.code, "RK_SNAPSHOT_BROKER_UNAVAILABLE");
  assert.equal(plan({ platform: "darwin", resolveExecutor: refuseExecutor }, { to: "check", stages: ["check"], executor: refuseExecutor() }).refusal.code, "RK_BROKER_UNAVAILABLE");
  assert.equal(plan({ platform: "linux", env: { GITHUB_ACTIONS: "true" } }, { executor: refuseExecutor() }).refusal.code, "RK_BROKER_UNAVAILABLE");
});

// L2: the codes a probe can print are DERIVED from the code that decides (the bodies of the three deciding functions), never from the list itself.
const body = (source, signature) => { const at = source.indexOf(signature); assert.ok(at >= 0, signature); return source.slice(at, source.indexOf("\n}\n", at)); };

test("every refusal code a probe can print is the set of codes the deciding functions can return, and each is reachable (not read back from the list under test)", () => {
  const source = fs.readFileSync(GATE_FILE, "utf8");
  const wired = (fn) => [...fn.matchAll(/\b(RK_[A-Z0-9_]+)\b/g)].map((m) => m[1]);
  const fromSource = new Set([
    ...wired(body(source, "export function snapshotRouteRefusal")),
    ...wired(body(source, "export function routePlan")),
  ]);
  assert.deepEqual([...fromSource].sort(), [...ROUTE_BASE_CODES].sort(), "the list names exactly what the deciding code returns");
  assert.deepEqual([...ROUTE_REFUSAL_CODES].sort(), [...ROUTE_BASE_CODES, ...SIGNING_REFUSAL_CODES].sort(), "and the signing refusals are the rest of it");
  // Behaviour: drive routePlan over the host, executor, carrier, environment and commit combinations; the codes it answers with are the list.
  const seen = new Set();
  for (const platform of ["linux", "darwin", "win32"]) for (const executor of [directExecutor(), brokerExecutor(), refuseExecutor()]) for (const carrier of [undefined, { kind: "x" }])
    for (const env of [{}, { TAURI_CONFIG: "{}" }, { GITHUB_ACTIONS: "true" }]) for (const [to, devTest, stages] of [["check", false, ["check"]], ["compile", false, ["check", "compile"]], ["dev", true, []], ["package", false, ["check", "compile", "package"]]]) for (const capabilities of [null, { ok: false, problem: "p" }]) for (const stagePlatform of [null, "mac", "win", "android"]) {
      const r = plan({ platform, snapshotBroker: carrier, env, resolveExecutor: () => executor }, { to, devTest, stages, executor, capabilities, stagePlatform });
      if (r.refusal) { seen.add(r.refusal.code); assert.ok(ROUTE_REFUSAL_CODES.includes(r.refusal.code), r.refusal.code); }
    }
  assert.deepEqual([...seen].sort(), [...ROUTE_BASE_CODES].sort(), "every code in the list is reachable");
  // Every code is one the stages print: the BLOCKED line of runStages is built from routePlan's refusal.
  assert.match(body(source, "async function runStages"), /decided\.refusal\.code/);
});

// ---- the stages and the probe share one decision (L1) ------------------------------------------------------------------------------

/** The `BLOCKED: <code>:` code a real run printed, or null: a run that got past the route decision prints none. */
const blockedCode = (err) => /BLOCKED: (RK_[A-Z0-9_]+):/.exec(err)?.[1] ?? null;

test("parity: a real run of every stage (no --route) is refused with exactly the code the probe answers, over the carrier, executor, platform, environment and commit combinations", async (t) => {
  const plain = fixture(t);
  const withTauri = fixture(t, TAURI_FILES);
  const reached = [];
  // A run that clears the route decision reaches the build ownership (snapshot route) or its class A (hosted): both stop here, never build.
  const stops = { acquireOwnership: () => { reached.push("ownership"); throw Object.assign(new Error("stopped at the ownership"), { code: "busy" }); }, syncSnapshot: () => { throw new Error("must not sync"); }, runCheck: async () => { reached.push("check"); return 1; }, runReleaseCheck: async () => 1, runCompile: async () => 1,
    spawnInherit: () => ({ status: 1 }), spawnCapture: () => ({ status: 1, stdout: "", stderr: "" }), readReceipt: () => null, writeReceipt: () => {}, appCheck: async () => 1 };
  const stagesFor = [["check", []], ["compile", []], ["dev", []], ["test", []], ["package", ["--platform", "mac"]], ["qualify", ["--platform", "win"]]];
  let compared = 0; const refused = new Map();
  const executors = [["direct", directExecutor], ["broker", brokerExecutor], ["refuse", refuseExecutor]];
  // The production carrier asks the broker for its capabilities (default query: the stubbed spawn answers nothing, so it refuses), or is
  // given an answer; the stubs here reach no broker.
  const CARRIERS = [[undefined, undefined], [PRODUCTION_SNAPSHOT_CARRIER, undefined], [PRODUCTION_SNAPSHOT_CARRIER, OK_CAPS], [PRODUCTION_SNAPSHOT_CARRIER, () => ({ ok: true, capabilities: ["snapshot-readonly-v1"] })]];
  const cases = [];
  for (const fx of [plain, withTauri]) {
    for (const platform of ["linux", "darwin", "win32"]) for (const [kind, executor] of executors) for (const [carrier, caps] of CARRIERS) for (const stage of stagesFor) cases.push({ fx, platform, kind, executor, carrier, caps, env: fx.env, stage, mode: "clean" });
    // An inherited TAURI_CONFIG, and hosted CI (a Linux runner: its carrier and broker are moot, so one pass of each executor is enough).
    for (const platform of ["linux", "darwin"]) for (const [kind, executor] of executors) for (const [carrier, caps] of CARRIERS) for (const stage of stagesFor.filter(([to]) => ["check", "dev", "package"].includes(to))) cases.push({ fx, platform, kind, executor, carrier, caps, env: { ...fx.env, TAURI_CONFIG: "{}" }, stage, mode: "TAURI_CONFIG" });
    for (const [kind, executor] of executors.filter(([k]) => k !== "broker")) for (const stage of stagesFor) cases.push({ fx, platform: "linux", kind, executor, carrier: undefined, env: { ...fx.env, GITHUB_ACTIONS: "true" }, stage, mode: "hosted" });
  }
  for (const { fx, platform, kind, executor, carrier, caps, env, stage: [to, extra], mode } of cases) {
    const common = { platform, resolveExecutor: executor, env, ...(carrier ? { snapshotBroker: carrier } : {}), ...(caps ? { brokerCapabilities: caps } : {}) };
    const probe = await fx.run(["--to", to, "--route", ...extra], common);
    assert.equal(probe.code, 0, probe.err);
    const real = await fx.run(["--to", to, ...extra], { ...stops, ...common, spawn: (c, a, o) => (c === "git" ? spawnSync(c, a, { ...o, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" }) });
    const label = `${fx === plain ? "plain" : "tauri"} ${platform} ${kind} carrier=${carrier ? carrier.kind : "none"}${caps ? `+caps:${caps().capabilities.length}` : ""} ${mode} --to ${to}`;
    assert.equal(blockedCode(real.err), probe.json.refusal?.code ?? null, `${label}: the probe says ${JSON.stringify(probe.json.refusal)}, the run printed ${real.err.slice(0, 300)}`);
    if (probe.json.refusal) { refused.set(probe.json.refusal.code, (refused.get(probe.json.refusal.code) ?? 0) + 1); assert.match(real.err, new RegExp(`BLOCKED: ${probe.json.refusal.code}: `), label); assert.deepEqual(reached.splice(0), [], `${label}: a refused run reached nothing`); }
    else assert.ok(reached.splice(0).length > 0, `${label}: an admitted run goes on to its ownership or its class A`);
    compared += 1;
  }
  assert.ok(compared > 400, `compared ${compared} combinations`);
  assert.deepEqual([...refused.keys()].sort(), [...ROUTE_BASE_CODES].sort(), "the combinations exercise every refusal code");
});

// ---- the command: arguments, answer and effects ------------------------------------------------------------------------------

test("--route answers one rightkit-gate.route/1 line for every stage, exit 0, and touches nothing but git and `which`", async (t) => {
  const fx = fixture(t);
  for (const [to, platform] of [["check"], ["compile"], ["dev"], ["test"], ["package", "mac"], ["qualify", "win"], ["package", "ios"], ["package", "android"], ["compile", "ios"]]) {
    // A Mac or Windows artifact is the native host's (E2E L3-03): the stage is probed on the host that builds it.
    const host = { mac: "darwin", ios: "darwin", win: "win32" }[platform] ?? "linux";
    const r = await fx.run(["--to", to, "--route", ...(platform ? ["--platform", platform] : [])], { platform: host });
    assert.equal(r.code, 0, `${to}: ${r.err}`);
    assert.equal(r.out.length, 1, "one line and nothing else");
    assert.equal(r.json.schema, ROUTE_SCHEMA);
    assert.deepEqual([r.json.to, r.json.platform, r.json.host, r.json.app, r.json.commit, r.json.executor, r.json.carrier, r.json.route, r.json.refusal, r.json.checkoutClean], [to, platform ?? null, host, ".", fx.sha, "direct", null, "snapshot", null, true]);
    assert.match(r.json.gate, /^\d+\.\d+\.\d+$/);
  }
  assert.deepEqual((await fx.run(["--to", "dev", "--route"])).json.stages, ["check", "dev"]);
  assert.deepEqual((await fx.run(["--to", "package", "--platform", "mac", "--route"], { platform: "darwin" })).json.stages, ["check", "compile", "package"]);
  assert.deepEqual((await fx.run(["--to", "package", "--platform", "ios", "--route"], { platform: "darwin" })).json.stages, ["check", "compile", "package"]);
  assert.deepEqual((await fx.run(["--to", "qualify", "--platform", "mac", "--route"], { platform: "darwin" })).json.stages, ["check", "compile", "package", "qualify"]);
  for (const { call } of fx.spawned) assert.ok(call[0] === "git" || call[0] === "which", `unexpected launch: ${call.join(" ")}`);
  assert.ok(fx.spawned.every(({ call }) => call[0] !== "git" || !["add", "commit", "checkout", "stash", "write-tree", "update-index", "worktree"].includes(call[1])), "git was only read");
  assert.equal(fs.existsSync(path.join(fx.dir, "snaps")), false, "no snapshot root was created");
});

test("--route's git status takes no optional lock (L4): GIT_OPTIONAL_LOCKS=0, so a probe never contends with an index writer", async (t) => {
  const fx = fixture(t);
  await fx.run(["--to", "dev", "--route"]);
  const status = fx.spawned.filter(({ call }) => call[0] === "git" && call.includes("status"));
  assert.equal(status.length, 1);
  assert.equal(status[0].env.GIT_OPTIONAL_LOCKS, "0");
  assert.ok(fx.spawned.filter(({ call }) => call[0] === "git" && !call.includes("status")).every(({ env }) => env?.GIT_OPTIONAL_LOCKS !== "1"));
});

test("--route on a Mac or Dell with the production carrier reports the snapshot route; the commit's own files decide what Windows refuses", async (t) => {
  const fx = fixture(t, TAURI_FILES);
  const prod = async (platform) => ({ platform, resolveExecutor: brokerExecutor, brokerCapabilities: OK_CAPS, ...(await productionEntryDeps({ platform })) });
  const mac = await prod("darwin");
  for (const to of ["dev", "test", "compile", "check"]) { const r = await fx.run(["--to", to, "--route"], mac); assert.deepEqual([r.code, r.json.route, r.json.refusal, r.json.carrier, r.json.executor], [0, "snapshot", null, "production", "broker"], to); }
  const win = await prod("win32");
  const dev = await fx.run(["--to", "dev", "--route"], win);
  assert.deepEqual([dev.code, dev.json.route, dev.json.carrier, dev.json.refusal], [0, "snapshot", "production", null], "the Dell carries the Tauri CLI's cargo through a stage grant");
  assert.equal((await fx.run(["--to", "compile", "--route"], win)).json.refusal, null);
  assert.equal((await fx.run(["--to", "check", "--route"], { ...win, resolveExecutor: refuseExecutor })).json.refusal.code, "RK_BROKER_UNAVAILABLE");
  assert.equal((await fx.run(["--to", "compile", "--route"], { ...mac, resolveExecutor: refuseExecutor })).json.refusal.code, "RK_SNAPSHOT_BROKER_UNAVAILABLE");
  assert.equal((await fx.run(["--to", "dev", "--route"], { ...mac, env: { ...fx.env, TAURI_CONFIG: "{}" } })).json.refusal.code, "RK_TAURI_CONFIG_OVERRIDE");
  const plain = fixture(t);
  assert.equal((await plain.run(["--to", "dev", "--route"], win)).json.refusal, null, "a commit with no Tauri crate refuses nothing on Windows");
});

test("--route reads the commit it is asked for, not the checkout, and an untracked file is no change", async (t) => {
  const fx = fixture(t);
  const win = { platform: "win32", resolveExecutor: brokerExecutor, brokerCapabilities: OK_CAPS, snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER };
  fx.write("src-tauri/tauri.conf.json", "{}\n"); // working tree only
  const before = await fx.run(["--to", "dev", "--route"], win);
  assert.equal(before.json.refusal, null);
  assert.equal(before.json.checkoutClean, true, "an untracked file is no change (--untracked-files=no)");
  git(fx.repo, "add", "-A"); git(fx.repo, "commit", "-q", "-m", "tauri");
  const old = await fx.run(["--to", "dev", "--route", "--commit", fx.sha], win);
  assert.equal(old.json.refusal, null);
  assert.equal(old.json.commit, fx.sha);
  assert.notEqual((await fx.run(["--to", "dev", "--route"], win)).json.commit, fx.sha);
});

test("--route usage errors exit 2 and print no route: --unit, --receipt-in, --receipt-out, --to release, no --to, an unreadable commit, a missing platform", async (t) => {
  const fx = fixture(t);
  const bad = [
    ["--to", "compile", "--unit", "npm:@rightkit/qa", "--route"], ["--to", "compile", "--route", "--receipt-in", "x.json"], ["--to", "compile", "--route", "--receipt-out", "x.json"],
    ["--to", "release", "--route"], ["--route"], ["--route", "--platform", "mac"], ["--to", "compile", "--route", "--commit", "nope"],
    ["--to", "package", "--route"], ["--to", "qualify", "--platform", "ios", "--route"],
  ];
  for (const args of bad) { const r = await fx.run(args); assert.equal(r.code, 2, args.join(" ")); assert.equal(r.json, null, args.join(" ")); assert.ok(r.err.length > 0); }
});

test("--route after `--` belongs to right-release: it is no probe", async (t) => {
  const fx = fixture(t);
  const stopped = [];
  const r = await fx.run(["--to", "dev", "--platform", "mac", "--", "--route"], { platform: "darwin", resolveExecutor: brokerExecutor, brokerCapabilities: OK_CAPS, snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER, acquireOwnership: () => { stopped.push("ownership"); throw Object.assign(new Error("stop"), { code: "busy" }); } });
  assert.equal(r.json, null);
  assert.deepEqual(stopped, ["ownership"], "the stage ran as an ordinary --to dev and took its ownership");
});

test("the usage text names --route", async () => {
  const out = []; await main(["--help"], { out: (m) => out.push(m), err: () => {} });
  assert.match(out.join("\n"), /--to check\|compile\|package\|qualify\|dev\|test --route/);
  assert.match(fs.readFileSync(GATE_FILE, "utf8").split("\nimport ")[0], /--route/, "the header comment names it too");
});

// ---- the real entry, and the contract test the canary's route probe makes of it ------------------------------------------------------

test("productionEntryDeps is what the real entry hands main: the production snapshot carrier on Mac and Dell, nothing on Linux", async () => {
  assert.deepEqual(await productionEntryDeps({ platform: "darwin" }), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER });
  assert.deepEqual(await productionEntryDeps({ platform: "win32" }), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER });
  assert.deepEqual(await productionEntryDeps({ platform: "linux" }), {});
  assert.deepEqual(await productionEntryDeps({ platform: "freebsd" }), {});
  assert.deepEqual(await productionEntryDeps(), await productionEntryDeps({ platform: process.platform }));
  const source = fs.readFileSync(GATE_FILE, "utf8");
  assert.match(source, /productionEntryDeps\(\)\.then\(\(deps\) => main\(process\.argv\.slice\(2\), deps\)\)/, "the real entry runs main with exactly these deps");
});

/** The production entry under production's own deps, as `--route` reports it per stage: Mac and Dell, a Tauri commit and a plain one. */
test("the production entry reports route=snapshot, the production carrier and refusal=null for dev, test, compile, package and qualify on darwin; the same on win32 (the Tauri CLI's cargo runs under a stage grant)", async (t) => {
  const tauri = fixture(t, TAURI_FILES);
  const plain = fixture(t);
  const stages = [["dev", []], ["test", []], ["compile", []], ["package", ["--platform", "mac"]], ["qualify", ["--platform", "mac"]]];
  const answer = async (fx, platform, [to, extra]) => {
    const deps = { platform, resolveExecutor: brokerExecutor, brokerCapabilities: OK_CAPS, ...(await productionEntryDeps({ platform })) };
    const r = await fx.run(["--to", to, "--route", ...extra.map((x) => (x === "mac" && platform === "win32" ? "win" : x))], deps);
    assert.equal(r.code, 0, r.err);
    const { gate, commit, ...rest } = r.json; void gate; void commit;
    return rest;
  };
  for (const fx of [tauri, plain]) for (const stage of stages) {
    const r = await answer(fx, "darwin", stage);
    assert.deepEqual([r.route, r.carrier, r.refusal, r.executor, r.host], ["snapshot", "production", null, "broker", "darwin"], `darwin ${stage[0]}`);
  }
  for (const stage of stages) {
    const r = await answer(plain, "win32", stage);
    assert.deepEqual([r.route, r.carrier, r.refusal, r.host], ["snapshot", "production", null, "win32"], `win32 plain ${stage[0]}`);
  }
  for (const stage of stages) {
    const r = await answer(tauri, "win32", stage);
    assert.deepEqual([r.route, r.carrier, r.refusal], ["snapshot", "production", null], `win32 tauri ${stage[0]}: the stage grant carries the Tauri CLI's cargo`);
  }
});

/**
 * A fixture `rustright` first on PATH, so a real-entry run never reaches the host's client or broker: it answers `doctor --json` with
 * `{ broker: { capabilities } }` (no `.broker.capabilities` field when `capabilities` is undefined: today's broker) and fails everything else. POSIX only (a Windows client is an .exe).
 */
function fixtureClient(t, capabilities) {
  const dir = path.join(tmp(t, "rk-fixture-client-"), ".rightkit-managed", "agent-bin");
  fs.mkdirSync(dir, { recursive: true });
  // An unhealthy host's doctor exits 1 but still prints its JSON: the field decides. `null`: the client cannot run at all.
  const doc = capabilities === undefined ? { healthy: false, broker: { reachable: true } } : { healthy: true, broker: { reachable: true, capabilities } };
  const body = capabilities === null ? "exit 1" : `if [ "$1 $2" = "doctor --json" ]; then echo '${JSON.stringify(doc)}'; exit ${capabilities === undefined ? 1 : 0}; fi\nexit 1`;
  fs.writeFileSync(path.join(dir, "rustright"), `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return dir;
}
const POSIX_CLIENT = process.platform === "win32" && "the fixture client is a POSIX script";

test("the real entry (node rightkit-gate.mjs --to <stage> --route) answers with the production deps: the route this host's stages take, asking a fixture client for its capabilities (never the host's broker)", { skip: POSIX_CLIENT }, (t) => {
  const fx = fixture(t);
  const bin = fixtureClient(t, [...REQUIRED_BROKER_CAPABILITIES]);
  const env = { ...fx.env, PATH: `${bin}${path.delimiter}${fx.env.PATH}` };
  for (const to of ["dev", "test", "compile", "check"]) {
    const r = spawnSync(process.execPath, [GATE_FILE, "--to", to, "--route"], { cwd: fx.repo, env, encoding: "utf8" });
    assert.equal(r.status, 0, r.stderr);
    const line = JSON.parse(r.stdout.trim().split("\n").pop());
    assert.equal(line.schema, ROUTE_SCHEMA);
    const brokerHost = process.platform === "darwin" || process.platform === "win32";
    assert.equal(line.carrier, brokerHost ? "production" : null, "production carries the snapshot carrier on Mac and Dell only");
    assert.equal(line.route, "snapshot", "there is no checkout route on any host");
    if (line.executor !== "refuse") assert.equal(line.refusal, null, `${to} on this host: ${JSON.stringify(line.refusal)}`);
    if (brokerHost) assert.deepEqual(line.brokerCapabilities, to === "check" ? null : [...REQUIRED_BROKER_CAPABILITIES], `${to}: the capabilities the fixture client advertised`);
  }
  // A client that advertises too little, or cannot answer, is refused on every stage that builds; --to check builds nothing.
  if (process.platform === "darwin" || process.platform === "win32") for (const [caps, why] of [[["snapshot-readonly-v1", "stage-grant-v1"], "snapshot-adapter-v1"], [["snapshot-readonly-v1"], "snapshot-adapter-v1"], [["snapshot-adapter-v1", "stage-grant-v1"], "snapshot-readonly-v1"], [[], "snapshot-readonly-v1"], [null, "cannot be asked"], [undefined, "no `.broker.capabilities`"]]) {
    const bad = { ...fx.env, PATH: `${fixtureClient(t, caps)}${path.delimiter}${fx.env.PATH}` };
    for (const to of ["dev", "test", "compile", "package", "qualify"]) {
      const out = spawnSync(process.execPath, [GATE_FILE, "--to", to, "--route", ...(to === "package" || to === "qualify" ? ["--platform", "mac"] : [])], { cwd: fx.repo, env: bad, encoding: "utf8" });
      const answer = JSON.parse(out.stdout.trim().split("\n").pop());
      assert.equal(answer.refusal?.code, "RK_BROKER_CAPABILITY_MISSING", `${to} with ${JSON.stringify(caps)}`);
      assert.match(answer.refusal.message, new RegExp(why));
      assert.match(answer.refusal.message, /rustright#190/);
    }
    assert.equal(JSON.parse(spawnSync(process.execPath, [GATE_FILE, "--to", "check", "--route"], { cwd: fx.repo, env: bad, encoding: "utf8" }).stdout.trim().split("\n").pop()).refusal, null);
  }
});

test("rightkit#1301 B2: the broker's capabilities are read from `rustright doctor --json` `.broker.capabilities` without a build, and a missing field, a failing, hung or garbled answer is a refusal on the probe and on the real run", async (t) => {
  assert.deepEqual([...REQUIRED_BROKER_CAPABILITIES], ["snapshot-readonly-v1", "snapshot-adapter-v1"], "no stage grant is minted any more: stage-grant-v1 is not required");
  const o = (spawn, over = {}) => ({ platform: "darwin", env: {}, spawn, exists: () => true, ...over });
  const answer = (stdout, status = 0) => (c, a) => (c === "which" ? { status: 0, stdout: "/managed/agent-bin/rustright\n" } : { status, stdout, stderr: "e" });
  const calls = [];
  const ok = queryBrokerCapabilities(o((c, a, opts) => { calls.push([c, ...a, opts?.timeout]); return answer('{"healthy":true,"broker":{"reachable":true,"capabilities":["journal-v1","snapshot-readonly-v1","stage-grant-v1","snapshot-adapter-v1"]}}')(c, a); }));
  assert.deepEqual(ok, { ok: true, capabilities: ["journal-v1", "snapshot-readonly-v1", "stage-grant-v1", "snapshot-adapter-v1"] });
  assert.deepEqual(calls.at(-1), ["/managed/agent-bin/rustright", "doctor", "--json", 20000], "a bounded, build-free query through the resolved client");
  for (const [spawn, why] of [[answer("", 2), /no JSON \(exit 2\)/], [answer("not json"), /no JSON/], [answer('{"broker":{}}'), /no `\.broker\.capabilities`/], [answer('{"capabilities":["snapshot-readonly-v1"]}'), /no `\.broker\.capabilities`/], [answer('{"broker":{"capabilities":"x"}}'), /no `\.broker\.capabilities`/], [answer('{"broker":{"capabilities":[1]}}'), /no `\.broker\.capabilities`/], [(c) => (c === "which" ? { status: 1, stdout: "" } : { status: 0, stdout: "{}" }), /not on PATH/], [(c) => (c === "which" ? { status: 0, stdout: "/m/rustright\n" } : { status: null, error: new Error("ETIMEDOUT"), stdout: "" }), /did not finish/]]) {
    const r = queryBrokerCapabilities(o(spawn)); assert.equal(r.ok, false); assert.match(r.problem, why);
  }
  // Who asks: only a broker executor on the snapshot route that builds, with the production carrier (or a declared query).
  const broker = { kind: "broker" };
  assert.equal(brokerCapabilityState(o(answer("{}"), { snapshotBroker: { kind: "injected" } }), { to: "dev", executor: broker }), null, "an injected fixture broker declares its own");
  assert.equal(brokerCapabilityState(o(answer("{}"), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER }), { to: "check", executor: broker }), null);
  assert.equal(brokerCapabilityState(o(answer("{}"), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER }), { to: "dev", executor: { kind: "direct" } }), null);
  assert.equal(brokerCapabilityState(o(answer("{}"), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER, env: { GITHUB_ACTIONS: "true" } }), { to: "dev", executor: broker }), null);
  assert.equal(brokerCapabilityState(o(answer("{}"), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER }), { to: "dev", executor: broker }).ok, false);
  // The plan: every capability is owed, a failed query refuses, check and the direct host are not asked.
  const planWith = (capabilities, over = {}) => routePlan({ platform: "darwin", env: {}, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", snapshotBroker: { kind: "x" }, resolveExecutor: brokerExecutor, ...over }, { to: "package", stages: ["check", "compile", "package"], devTest: false, executor: brokerExecutor(), capabilities });
  assert.equal(planWith(OK_CAPS()).refusal, null);
  for (const caps of [{ ok: false, problem: "p" }, { ok: true, capabilities: [] }, { ok: true, capabilities: ["snapshot-readonly-v1"] }, { ok: true, capabilities: ["stage-grant-v1"] }, { ok: true, capabilities: ["snapshot-adapter-v1"] }, { ok: true, capabilities: ["snapshot-readonly-v1", "stage-grant-v1"] }]) {
    const r = planWith(caps); assert.equal(r.refusal.code, "RK_BROKER_CAPABILITY_MISSING"); assert.equal(r.refusal.stage, "package"); assert.match(r.refusal.message, /rustright doctor --json/);
  }
  assert.equal(routePlan({ platform: "darwin", env: {}, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", snapshotBroker: { kind: "x" }, resolveExecutor: brokerExecutor }, { to: "check", stages: ["check"], devTest: false, executor: brokerExecutor(), capabilities: { ok: false, problem: "p" } }).refusal, null, "--to check builds nothing");
});

/** A copy of packages/release whose production entry hands main no carrier (0.6.12's failure from the other side: the entry and the route disagree), node_modules linked. */
function releaseCopy(t, { carrier }) {
  // The host's scratch root when it exists and is writable (the Mac's and the Dell's), else the real temp directory (a hosted runner has no such root).
  const preferred = process.platform === "darwin" ? "/Volumes/D/scratch" : process.platform === "win32" ? "D:\\scratch" : null;
  const usable = (dir) => { try { return fs.statSync(dir).isDirectory() && (fs.accessSync(dir, fs.constants.W_OK), true); } catch { return false; } };
  const base = fs.realpathSync(preferred && usable(preferred) ? preferred : os.tmpdir());
  const dir = tmp(t, "rk-route-contract-", base);
  const dest = path.join(dir, "release");
  fs.cpSync(HERE, dest, { recursive: true, filter: (src) => path.basename(src) !== "node_modules" });
  fs.symlinkSync(path.join(HERE, "node_modules"), path.join(dest, "node_modules"), "dir");
  if (!carrier) {
    const file = path.join(dest, "bin", "rightkit-gate.mjs");
    const source = fs.readFileSync(file, "utf8");
    const stripped = source.replace('  return { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER };', "  void PRODUCTION_SNAPSHOT_CARRIER; return {};");
    assert.notEqual(stripped, source, "the production entry's carrier is removed");
    fs.writeFileSync(file, stripped);
  }
  return { gateBin: path.join(dest, "bin", "rightkit-gate.mjs"), dir };
}

test("contract: a candidate whose production entry does not wire the carrier (the 0.6.12 failure's mirror) is a canary regression: route-refusal <stage>:RK_SNAPSHOT_CARRIER_NOT_WIRED for every stage the shipped entry admits", { skip: process.platform !== "darwin" && "this contract runs on the Mac with a POSIX fixture client (Linux takes the direct snapshot route; the Dell's client is an .exe)" }, async (t) => {
  const fx = fixture(t);
  const clientDir = fixtureClient(t, [...REQUIRED_BROKER_CAPABILITIES]); // the host's own client and broker are never reached
  const probeAll = async (gateBin) => {
    const routes = [];
    for (const stage of routeStagesFor("mac")) {
      const run = await runRouteProbe({ gateBin, appRoot: fx.repo, platform: "mac", stage, env: { ...fx.env, PATH: `${clientDir}${path.delimiter}${fx.env.PATH}` } });
      routes.push({ stage, ...parseRoute({ ...run, stage }) });
    }
    return routes;
  };
  const baseline = releaseCopy(t, { carrier: true });
  const candidate = releaseCopy(t, { carrier: false });
  const baseRoutes = await probeAll(baseline.gateBin);
  if (!baseRoutes.every((r) => r.status === "ok" && r.route.executor === "broker")) return t.skip("this host has no reachable RustRight broker; the contract needs the broker executor");
  assert.ok(baseRoutes.every((r) => r.route.refusal === null && r.route.carrier === "production"), "the shipped entry admits every stage: " + JSON.stringify(baseRoutes.map((r) => r.route.refusal)));
  const candRoutes = await probeAll(candidate.gateBin);
  assert.ok(candRoutes.every((r) => r.status === "ok"), JSON.stringify(candRoutes));
  const cell = (side, routes) => ({ id: "app", app: "app", platform: "mac", side, host: process.platform, status: "pass", failNames: [], gateFailNames: [], fatal: null, compilerCache: true, treeChanged: false, treeDelta: [], gatesRun: false, head: "h", routeSupported: true, routes });
  const result = compareCells({ candidate: cell("candidate", candRoutes), baseline: cell("baseline", baseRoutes), version: "0.6.14", app: "app", platform: "mac" });
  assert.equal(result.verdict, "regression");
  const blocks = result.findings.filter((f) => f.severity === "block").map((f) => `${f.kind} ${f.match}`);
  for (const stage of ["dev", "test", "compile", "package", "qualify"]) assert.ok(blocks.includes(`route-refusal ${stage}:RK_SNAPSHOT_CARRIER_NOT_WIRED`), blocks.join("; "));
  // The shipped entry is clean against itself.
  const same = compareCells({ candidate: cell("candidate", baseRoutes), baseline: cell("baseline", baseRoutes), version: "0.6.14", app: "app", platform: "mac" });
  assert.equal(same.verdict, "ok");
});

test("[GB-appscope] the app scope is the SELECTED COMMIT's committed config inventory plus the cwd: a newer checkout's deleted, added, untracked or dirty config never moves an older commit's app", async (t) => {
  const fx = fixture(t, { "README.md": "r\n", "apps/one/right-release.config.json": "{}\n", "apps/one/src/a.txt": "a\n", "apps/two/src/b.txt": "b\n" });
  const c1 = fx.sha;
  // The newer commit drops apps/one's config and adds apps/two's; the working tree then also holds an untracked config for apps/one.
  git(fx.repo, "rm", "-q", "apps/one/right-release.config.json");
  fx.write("apps/two/right-release.config.json", "{}\n");
  git(fx.repo, "add", "-A"); git(fx.repo, "commit", "-q", "-m", "c2");
  const c2 = git(fx.repo, "rev-parse", "HEAD").stdout.trim();
  const at = (rel) => path.join(fx.repo, ...rel.split("/"));
  const appOf = async (sha, rel) => { const r = await fx.run(["--to", "package", "--platform", "mac", "--route", "--commit", sha], { cwd: at(rel) }); assert.equal(r.code, 0, r.err); return r.json.app; };
  // Checkout state A: HEAD = c2 (apps/one has no config, apps/two has one).
  assert.equal(await appOf(c1, "apps/one/src"), "apps/one", "c1 committed apps/one's config: it is the app although the checkout no longer has it");
  assert.equal(await appOf(c1, "apps/two/src"), ".", "c1 has no config for apps/two: the root is the app although the checkout now has one");
  assert.equal(await appOf(c2, "apps/two/src"), "apps/two");
  assert.equal(await appOf(c2, "apps/one/src"), ".");
  // Checkout state B: dirty and untracked configs appear; the selected commit's answer is unchanged.
  fx.write("apps/one/right-release.config.mjs", "export default {};\n");
  fx.write("apps/two/right-release.config.json", "{ dirty\n");
  assert.equal(await appOf(c2, "apps/one/src"), ".", "an untracked config in the checkout does not make apps/one an app of c2");
  assert.equal(await appOf(c1, "apps/one"), "apps/one"); assert.equal(await appOf(c1, "apps/two"), ".");
  // And the checkout's config files are never read to decide it.
  const { appRootOf, committedConfigDirs } = await import("./bin/rightkit-gate.mjs");
  const root = fx.repo;
  const dirs = committedConfigDirs({ git: (...a) => git(root, ...a), commit: c1 }).dirs;
  assert.deepEqual([...dirs], ["apps/one"]);
  assert.deepEqual(appRootOf({ root, cwd: at("apps/one/src"), exists: () => { throw new Error("the checkout is not asked"); }, configDirs: dirs }), { appRoot: at("apps/one"), appDir: "apps/one" });
  assert.match(committedConfigDirs({ git: (...a) => git(root, ...a), commit: "f".repeat(40) }).refusal, /cannot list the files/);
});


// ---- the optional Linux broker route (rightkit#1317: Linux needs no broker, but a broker is allowed on Linux) ---------------------------
// The real executor and the real production entry decide; the client file and the listening unix socket are real files on disk, and no test here
// starts a compile. Whether a real installed broker then compiles the real HeardRight tree is the box's acceptance, not this file's.

async function declaredLinuxBroker(t) {
  const dir = tmp(t, "rk-linux-route-");
  const socket = path.join(dir, "broker.sock");
  const server = net.createServer();
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  t.after(() => server.close());
  const client = path.join(dir, "rustright");
  fs.writeFileSync(client, "#!/bin/sh\nexit 1\n"); fs.chmodSync(client, 0o755);
  const pin = createHash("sha256").update("#!/bin/sh\nexit 1\n").digest("hex");
  return { dir, client, socket, env: { [LINUX_BROKER_CLIENT_ENV]: client, [LINUX_BROKER_SOCKET_ENV]: socket, [LINUX_BROKER_CLIENT_SHA256_ENV]: pin } };
}

test("productionEntryDeps hands a declared Linux broker the production carrier (valid or not, so nothing is left to compile direct); an undeclared Linux gets none even with RustRight on PATH", async (t) => {
  const host = await declaredLinuxBroker(t);
  assert.deepEqual(await productionEntryDeps({ platform: "linux", env: host.env }), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER });
  assert.deepEqual(await productionEntryDeps({ platform: "linux", env: { [LINUX_BROKER_CLIENT_ENV]: "/not/installed/rustright" } }), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER }, "an invalid declaration still has the carrier; the executor refuses it");
  assert.deepEqual(await productionEntryDeps({ platform: "linux", env: { PATH: `${path.dirname(host.client)}:/usr/bin`, RUSTRIGHT_BUILD_BROKER_SOCKET: host.socket } }), {}, "PATH and the older socket variable declare nothing");
  assert.deepEqual(await productionEntryDeps({ platform: "linux", env: {} }), {});
});

test("routePlan on Linux with the real executor: undeclared is direct; a valid declaration takes the broker with the carrier; a stopped or invalid one refuses and never compiles direct", async (t) => {
  const host = await declaredLinuxBroker(t);
  const inputs = (env, over = {}) => ({ platform: "linux", env, spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", resolveExecutor: realResolveExecutor, snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER, ...over });
  const decide = (env, to = "compile", capabilities = OK_CAPS()) => {
    const o = inputs(env);
    const executor = realResolveExecutor({ env, platform: "linux", exists: o.exists, which: () => [], readFile: o.readFile });
    return { executor, plan: routePlan(o, { to, stages: to === "check" ? ["check"] : ["check", to], devTest: false, executor, capabilities }) };
  };
  const direct = decide({ PATH: "/usr/bin" });
  assert.deepEqual([direct.executor.kind, direct.plan.refusal], ["direct", null]);
  const ok = decide(host.env);
  assert.deepEqual([ok.executor.kind, ok.executor.route, ok.plan.refusal], ["broker", "linux-broker", null]);
  // A declared broker that does not advertise what the carried stages need is refused, never routed direct.
  assert.equal(decide(host.env, "compile", { ok: true, capabilities: [] }).plan.refusal.code, "RK_BROKER_CAPABILITY_MISSING");
  assert.equal(decide(host.env, "compile", { ok: false, problem: "unreachable" }).plan.refusal.code, "RK_BROKER_CAPABILITY_MISSING");
  // The broker is stopped (its socket is gone) or the declaration is wrong: refused on every stage, by the executor's own problem.
  const { [LINUX_BROKER_CLIENT_SHA256_ENV]: _pin, ...unpinned } = host.env; void _pin;
  for (const env of [{ ...host.env, [LINUX_BROKER_SOCKET_ENV]: path.join(host.dir, "stopped.sock") }, { [LINUX_BROKER_CLIENT_ENV]: host.client }, unpinned, { ...host.env, [LINUX_BROKER_CLIENT_SHA256_ENV]: "0".repeat(64) }]) {
    assert.equal(decide(env).plan.refusal.code, "RK_SNAPSHOT_BROKER_UNAVAILABLE");
    assert.match(decide(env).plan.refusal.message, /RK_LINUX_BROKER_UNAVAILABLE/);
    assert.equal(decide(env, "check").plan.refusal.code, "RK_BROKER_UNAVAILABLE");
    assert.match(snapshotRouteRefusal(inputs(env)).message, /never runs direct around a declared broker/);
  }
  // A declared broker with a gate started without its carrier is refused as on the Mac and Dell, never compiled direct.
  assert.equal(routePlan(inputs(host.env, { snapshotBroker: undefined }), { to: "compile", stages: ["check", "compile"], devTest: false, executor: realResolveExecutor({ env: host.env, platform: "linux", exists: () => false, which: () => [], readFile: () => "" }) }).refusal.code, "RK_SNAPSHOT_CARRIER_NOT_WIRED");
});

test("--route on Linux through the real entry's deps and the real executor reports the broker executor and carrier for a declaration, and direct without one", async (t) => {
  const host = await declaredLinuxBroker(t);
  const fx = fixture(t);
  const probe = async (env) => fx.run(["--to", "compile", "--route"], { platform: "linux", env: { ...fx.env, ...env }, resolveExecutor: realResolveExecutor, brokerCapabilities: OK_CAPS, ...(await productionEntryDeps({ platform: "linux", env })) });
  const declared = await probe(host.env);
  assert.deepEqual([declared.code, declared.json.executor, declared.json.carrier, declared.json.route, declared.json.refusal], [0, "broker", "production", "snapshot", null]);
  const undeclared = await probe({});
  assert.deepEqual([undeclared.code, undeclared.json.executor, undeclared.json.carrier, undeclared.json.refusal], [0, "direct", null, null]);
  const stopped = await probe({ ...host.env, [LINUX_BROKER_SOCKET_ENV]: path.join(host.dir, "stopped.sock") });
  assert.equal(stopped.json.refusal.code, "RK_SNAPSHOT_BROKER_UNAVAILABLE");
});

test("the quiet-lease lane takes its lease from a declared Linux broker's own client and endpoint, and takes none on direct Linux", async (t) => {
  const host = await declaredLinuxBroker(t);
  const executor = realResolveExecutor({ env: host.env, platform: "linux", exists: () => false, which: () => [], readFile: () => "" });
  const calls = [];
  const spawn = (c, a, opts) => { calls.push({ command: c, args: a, socket: opts?.env?.RUSTRIGHT_BUILD_BROKER_SOCKET }); return { status: 1, stdout: "", stderr: "refused" }; };
  const base = { platform: "linux", env: { ...host.env }, spawn, out: () => {}, readFile: () => "" };
  assert.equal(await quietLeaseLane(base, { executor }).acquire(), null, "a refused lease is no lease");
  const lease = calls.find((c) => c.args?.includes("quiet"));
  assert.ok(lease, "the lease was asked of the broker");
  assert.equal(lease.command, host.client, "the declared client, not a PATH `rustright`");
  assert.equal(lease.socket, host.socket);
  calls.length = 0;
  assert.equal(await quietLeaseLane({ ...base, env: {} }, { executor: { kind: "direct", cargo: ["cargo"] } }).acquire(), null);
  assert.deepEqual(calls, [], "direct Linux asks no broker for anything");
});
