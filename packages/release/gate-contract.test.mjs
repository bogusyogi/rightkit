// Acceptance tests for docs/GATE-CONTRACT.md (joint plan step 3). Every test names its kind:
//   [behavior] runs a public seam of the gate (resolveExecutor, planScans, runScans, main) and observes routing, ordering or refusals.
//              Real subprocesses with sentinel-writing fakes where launches matter; never a real compile.
//   [drift]    binds docs/GATE-CLASS-A-INVENTORY.md to the canonical enumerations in code (scan prefixes, test classes, rule ids, stages,
//              executor kinds, entry bins). A source check only: it proves no runtime cell.
//   [pending]  a contract expectation the current code does not meet, or has no seam for yet. Written as the contract states it and
//              registered with `todo` naming the worklist step that owns it; never weakened to pass today.
// The nine origin × product cells and rows E1-E18 are enumerated once below (CELLS, ROWS) with what covers each.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { CLASS_A_SCAN_PREFIXES, CLASS_B_SCAN_PREFIXES, TEST_SCAN_CLASS, LIVE_SCAN_IDS, REGISTRY_SCAN_IDS, cargoReachOf, planScans } from "./deterministic-scans.mjs";
import { EXECUTOR_KINDS, resolveExecutor } from "./gate-executor.mjs";
import { STAGES } from "./gate-stages.mjs";
import { RULES as APP_RULES, COMPILE_TIER_IDS } from "./app-check.mjs";
import { RULES as PREFLIGHT_RULES } from "../../scripts/preflight.mjs";
import { main } from "./bin/rightkit-gate.mjs";
import { directPlatform, managedRuntimeClient, runCargoSync } from "./managed-cargo.mjs";
import { brokerManagedHost, managedCargoShimOnPath } from "./cargo-contract.mjs";
import { developmentEnvironment } from "./development.mjs";
import { GITHUB_ACTIONS_ONLY, GITHUB_ACTIONS_ONLY_ERROR, LOCAL_BUILD_FILE, assertReleaseExecutionAllowed } from "./execution-policy.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..");
const INVENTORY = path.join(REPO, "docs", "GATE-CLASS-A-INVENTORY.md");
const read = (rel) => fs.readFileSync(path.join(REPO, rel), "utf8");
function tmp(t, prefix) {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(d, { recursive: true, force: true }));
  return d;
}
function write(root, files) {
  for (const [name, body] of Object.entries(files)) { const p = path.join(root, ...name.split("/")); fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, body); }
}

// ---- the nine cells and E1-E18: what covers each today ------------------------------------------------------------------------------
// covered: [file, test title] pairs whose existence a [drift] test checks; pending: [worklist step, what is missing].
const CELLS = {
  "R-M": { host: "mac", product: "rightkit" }, "R-D": { host: "dell", product: "rightkit" }, "R-L": { host: "linux", product: "rightkit" },
  "P-M": { host: "mac", product: "private" }, "P-D": { host: "dell", product: "private" }, "P-L": { host: "linux", product: "private" },
  "A-M": { host: "mac", product: "public" }, "A-D": { host: "dell", product: "public" }, "A-L": { host: "linux", product: "public" },
};
const ROWS = {
  E1: { covered: [["gate-contract.test.mjs", "[behavior] E1: a red class A starts no compile and no later-stage scan (real subprocesses, sentinel fakes)"], ["gate-contract.test.mjs", "[behavior] E1: the door's dev and test stages never launch right-release after a red check"], ["rightkit-gate.test.mjs", "--to runs every stage up to the target in order, class A first, and stops at the first failure with nothing after it"], ["snapshot-check.test.mjs", "E17: a dirty checkout whose HEAD is ahead still checks the selected commit's snapshot, never reading HEAD or status"]], pending: [["decisions.md (recorded placement decisions)", "inventory placements marked PROPOSED or PENDING are not applied in the planner until a decision is recorded"], ["step 11", "the complete inventory reconciled to the checks the installed gate executes on Mac, Dell and Linux"]] },
  E2: { covered: [["gate-contract.test.mjs", "[behavior] executor: a Mac or Dell managed host whose broker is unreachable is refused, never routed direct"], ["rightkit-gate.test.mjs", "executor: a broker-managed machine without a reachable broker is refused; any other host runs cargo directly through the same plan"]], pending: [["step 11", "installed shared entry on Mac and Dell refuses before compile (installed-host run)"]] },
  E3: { covered: [["gate-contract.test.mjs", "[behavior] executor: a Linux host with no broker runs direct"], ["gate-contract.test.mjs", "[behavior] executor: a Linux host always runs direct, even with RustRight on PATH, a broker socket variable or a managed cargo shim (contract §1 'One entry')"]], pending: [["step 10", "actual Linux/WSL2 local compile through the installed entry, zero broker or browser launches"]] },
  E4: { covered: [], pending: [["step 10", "private Linux hold/resume with and without Linux deliverables"], ["step 11", "hosted Linux qualification of exact sealed bytes on Mac/Dell"]] },
  E5: { covered: [], pending: [["step 9", "nonpublishing fixtures for each npm/Cargo route; failed qualification never promotes"], ["step 12", "six real R-M/R-D/R-L x npm/Cargo publications with readback"]] },
  E6: { covered: [["snapshot.test.mjs", "E6: a changed byte, a deleted file, a planted visible file refuse verification; the next sync repairs them"], ["snapshot-check.test.mjs", "E17: a snapshot changed while class A runs refuses with RK_CHECK_ERROR, exit 2"], ["gate-contract.test.mjs", "[behavior] E6/§1: the door builds the commit, not the checkout: a dirty checkout neither blocks nor leaks, an edit mid-stage never reaches the snapshot, a tampered snapshot voids the stage (exit 2), a second request waits for the held ownership (exit 1)"]], pending: [["step 9", "stale or altered output, helper, native output, final artifact, app SHA, run or attempt reject"]] },
  E7: { covered: [], pending: [["step 9", "candidate-to-final evidence binding (transformations, final digest, unchanged promotion)"]] },
  E8: { covered: [], pending: [["step 11", "canary legs per origin with required app SHAs"]] },
  E9: { covered: [], pending: [["step 9", "durable retry/restart reconciliation at dispatch and publication boundaries"]] },
  E10: { covered: [["gate-contract.test.mjs", "[behavior] E10: the executor has no caller input, so a human and a managed agent route the same way"]], pending: [["step 11", "installed entry on Mac/Dell for both callers; a personal terminal outside the entry is not intercepted"]] },
  E11: { covered: [], pending: [["step 11", "pinned qualification policy, native installers and targets on their native hosts"]] },
  E12: { covered: [], pending: [["pulse#492 (Pulse owner)", "headless Linux guest join, messaging, reconnect, revoked/expired denial"]] },
  E13: { covered: [], pending: [["step 7", "workspace Clippy route on Mac, Dell (broker) and Linux (direct)"]] },
  E14: { covered: [], pending: [["step 7", "batched Clippy equivalence before the per-crate path retires"]] },
  E15: { covered: [], pending: [["step 7", "failure collection with attribution across independent failures"]] },
  E16: { covered: [["gate-contract.test.mjs", "[behavior] E16/§1: on Linux the door compiles the selected commit\'s snapshot; every cargo scan writes to the checkout\'s resolved target directory, never into the snapshot (real subprocesses, fake cargo)"]], pending: [["step 6", "a second build of the same commit reuses the checkout's outputs (measured cache reuse)"]] },
  E17: { covered: [["snapshot-check.test.mjs", "E17: a dirty checkout whose HEAD is ahead still checks the selected commit's snapshot, never reading HEAD or status"], ["snapshot-check.test.mjs", "E17 real subprocesses: npm-only dirty checkout with HEAD ahead passes on the selected commit's snapshot"]], pending: [["step 4", "the broker admits the snapshot-aware check itself (snapshot cwd for cargo scans; no primary-checkout fallback)"]] },
  E18: { covered: [["snapshot-check.test.mjs", "E18 live-scans: one JSON line the broker's parser accepts; exit 0 only when every live scan passed; only live scans run"], ["snapshot-check.test.mjs", "rightkit#1257: a red of deterministic failures prints the marker with env=; a live-scan or undecided failure prints none"]], pending: [["step 5", "reuse keyed on the commit tree under snapshot mode, with the broker's refresh (B19)"]] },
};

test("[drift] every row E1-E18 is enumerated, and every covering test named here exists in its file", () => {
  assert.deepEqual(Object.keys(ROWS), Array.from({ length: 18 }, (_, i) => `E${i + 1}`));
  const contract = read("docs/GATE-CONTRACT.md");
  for (const id of Object.keys(ROWS)) assert.match(contract, new RegExp(`^\\| ${id} `, "m"), `${id} is a row of the contract`);
  // Titles are read from actual `test("...")` calls, never from this file's ROWS literal: a deleted test is missed coverage.
  const titlesOf = (file) => new Set([...fs.readFileSync(path.join(HERE, file), "utf8").matchAll(/^test\("((?:[^"\\]|\\.)*)"/gm)].map((m) => JSON.parse(`"${m[1]}"`)));
  for (const [id, row] of Object.entries(ROWS)) {
    for (const [file, title] of row.covered) assert.ok(titlesOf(file).has(title), `${id}: ${file} has no test "${title}"`);
  }
  for (const id of Object.keys(CELLS)) assert.match(contract, new RegExp(`^\\| ${id} \\|`, "m"), `${id} is a cell of the contract matrix`);
});

for (const [id, row] of Object.entries(ROWS)) {
  for (const [owner, what] of row.pending) test(`[pending] ${id}: ${what}`, { todo: `owned by ${owner}` }, () => { assert.fail(`${id} not yet provable: ${what}`); });
}
for (const [id, cell] of Object.entries(CELLS)) {
  test(`[pending] ${id}: positive local compile and final-target qualification (${cell.host} x ${cell.product})`, { todo: "owned by steps 4, 9, 10, 11 and 12 (installed entry, snapshot compile, qualification, publication)" }, () => { assert.fail(`${id} NOT RUN`); });
}

// ---- executor routing per origin (the route half of each cell) ------------------------------------------------------------------------
const HOSTS = {
  mac: { platform: "darwin", env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/Volumes/D/.rightkit-managed/broker.sock", PATH: "/Volumes/D/.rightkit-managed/agent-bin:/usr/bin" }, rustright: "/Volumes/D/.rightkit-managed/agent-bin/rustright" },
  dell: { platform: "win32", env: { RIGHTKIT_BUILD_BROKER_SOCKET: "\\\\.\\pipe\\rustright-broker", PATH: "D:\\.rightkit-managed\\agent-bin;C:\\Windows" }, rustright: "D:\\.rightkit-managed\\agent-bin\\rustright.cmd" },
  linux: { platform: "linux", env: { PATH: "/usr/local/bin:/usr/bin" }, rustright: null },
};
const route = (host, { rustrightOnPath = true, env = {} } = {}) => {
  const h = HOSTS[host];
  return resolveExecutor({ env: { ...h.env, ...env }, platform: h.platform, exists: () => false, readFile: () => "", which: (cmd) => (cmd === "rustright" && rustrightOnPath && (h.rustright || env.__rustright) ? [h.rustright ?? env.__rustright] : []) });
};

test("[behavior] executor: Mac and Dell route through the broker (the route half of the six M/D cells; the product does not enter executor selection)", () => {
  for (const [id, cell] of Object.entries(CELLS)) if (cell.host !== "linux") assert.deepEqual(route(cell.host), { kind: "broker", cargo: ["rustright", "cargo"] }, id);
  assert.deepEqual([...EXECUTOR_KINDS].sort(), ["broker", "direct", "refuse"]);
});
test("[behavior] executor: a Mac or Dell managed host whose broker is unreachable is refused, never routed direct", () => {
  for (const host of ["mac", "dell"]) { const r = route(host, { rustrightOnPath: false }); assert.equal(r.kind, "refuse", host); assert.equal(r.cargo, null); assert.match(r.problem, /never compiles around the broker/); }
});
test("[behavior] executor: a Linux host with no broker runs direct", () => {
  const kindOf = (r) => ({ kind: r.kind, cargo: r.cargo });
  for (const [id, cell] of Object.entries(CELLS)) if (cell.host === "linux") assert.deepEqual(kindOf(route("linux")), { kind: "direct", cargo: ["cargo"] }, id);
});
test("[behavior] executor: a Linux host always runs direct, even with RustRight on PATH, a broker socket variable or a managed cargo shim (contract §1 'One entry')", () => {
  const kindOf = (r) => ({ kind: r.kind, cargo: r.cargo });
  assert.deepEqual(kindOf(route("linux", { env: { __rustright: "/usr/local/bin/rustright" } })), { kind: "direct", cargo: ["cargo"] });
  assert.deepEqual(kindOf(route("linux", { env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/run/rustright.sock" } })), { kind: "direct", cargo: ["cargo"] });
  const shim = resolveExecutor({ env: { PATH: "/home/a/.rightkit-managed/agent-bin:/usr/bin", RIGHTKIT_DATA_ROOT: "/home/a/.rightkit-managed" }, platform: "linux", exists: () => true, readFile: () => "cargo-guard.mjs", which: (c) => (c === "cargo" ? ["/home/a/.rightkit-managed/agent-bin/cargo"] : []) });
  assert.equal(shim.kind, "direct");
  assert.deepEqual(shim.cargo, ["cargo"]);
  assert.equal(shim.env.PATH, "/usr/bin", "the managed shim's directory is taken off PATH, so `cargo` is the real toolchain");
});

test("[behavior] executor: on Linux the gate launches the real cargo, never a RustRight shim first on PATH (real subprocesses, sentinel fakes)", { skip: process.platform === "win32" ? "POSIX shims" : false }, async (t) => {
  // A Cargo repository's snapshot check, run in a child (no harness broker guard) as a Linux host: unit discovery runs `cargo metadata`.
  // PATH holds a fake RustRight shim (in a .rightkit-managed/agent-bin directory) first and a fake direct cargo second; each writes a sentinel.
  const dir = tmp(t, "rk-gc-linux-");
  const shimDir = path.join(dir, ".rightkit-managed", "agent-bin"); const directDir = path.join(dir, "toolchain", "bin");
  const meta = (root) => JSON.stringify({ packages: [{ name: "core", manifest_path: path.join(root, "crates", "core", "Cargo.toml"), publish: null, id: "core", targets: [] }], workspace_members: ["core"], workspace_root: root, target_directory: path.join(root, "target"), version: 1 });
  const fake = (name) => `#!/bin/sh\necho "$*" >> '${path.join(dir, `${name}-ran`)}'\nif [ "$1" = "metadata" ]; then node -e 'const r=process.cwd();console.log(JSON.stringify({packages:[{name:"core",manifest_path:r+"/crates/core/Cargo.toml",publish:null,id:"core",targets:[]}],workspace_members:["core"],workspace_root:r,target_directory:r+"/target",version:1}))'; fi\n`;
  write(shimDir, { cargo: fake("shim") }); fs.chmodSync(path.join(shimDir, "cargo"), 0o755);
  write(directDir, { cargo: fake("direct") }); fs.chmodSync(path.join(directDir, "cargo"), 0o755);
  void meta;
  const repo = path.join(dir, "repo");
  fs.mkdirSync(repo);
  for (const a of [["init", "-q", "-b", "main"]]) spawnSync("git", a, { cwd: repo });
  write(repo, { "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n', "Cargo.lock": "version = 4\n", "crates/core/Cargo.toml": '[package]\nname = "core"\nversion = "0.1.0"\nedition = "2021"\n', "crates/core/src/lib.rs": "\n" });
  spawnSync("git", ["add", "-A"], { cwd: repo }); spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c"], { cwd: repo });
  const sha = String(spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout).trim();
  const snapRoot = path.join(dir, "snaps");
  const runAs = (label, executorStub) => {
    for (const n of ["shim", "direct"]) fs.rmSync(path.join(dir, `${n}-ran`), { force: true });
    const driver = path.join(dir, `driver-${label}.mjs`);
    fs.writeFileSync(driver, `const { main } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "bin", "rightkit-gate.mjs")).href)});
const { syncSnapshot } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "snapshot.mjs")).href)});
const snap = syncSnapshot({ checkout: ${JSON.stringify(repo)}, rev: ${JSON.stringify(sha)}, root: ${JSON.stringify(snapRoot)} });
const out = [];
const code = await main(["check", "--commit", ${JSON.stringify(sha)}, "--full-a", "--snapshot", snap.path], { cwd: ${JSON.stringify(repo)}, platform: "linux", out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0,
  ${executorStub ? `resolveExecutor: () => (${executorStub}),` : ""}
  spawn: undefined, runScans: async () => ({ scans: [], notes: [], cacheHits: [] }) });
console.log(JSON.stringify({ code, out: out.join("\\n").slice(-1500) }));\n`.replace("spawn: undefined, ", ""));
    // The host ownership record and snapshots stay in this fixture, never in the real state or data root.
    const env = { ...process.env, NODE_OPTIONS: "", RK_GATE_CACHE: "0", PATH: `${shimDir}:${directDir}:${process.env.PATH}`, RIGHTKIT_SNAPSHOT_ROOT: path.join(dir, "snaps"), RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
    const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", cwd: repo });
    let res = {}; try { res = JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { res = { stderr: r.stderr }; }
    return { ...res, shim: fs.existsSync(path.join(dir, "shim-ran")), direct: fs.existsSync(path.join(dir, "direct-ran")) };
  };
  const fixed = runAs("fixed", null);
  assert.equal(fixed.shim, false, `the RustRight shim was never launched (${JSON.stringify(fixed)})`);
  assert.equal(fixed.direct, true, `the direct cargo answered cargo metadata (${JSON.stringify(fixed)})`);
  assert.match(fixed.out ?? "", /RustRight shims taken off PATH for direct cargo/);
  // Control: the route label alone (direct, bare cargo, PATH unchanged) launches the shim, so the sentinels tell the two apart.
  const labelOnly = runAs("label-only", `{ kind: "direct", cargo: ["cargo"] }`);
  assert.equal(labelOnly.shim, true, `control: with PATH unchanged the shim answers (${JSON.stringify(labelOnly)})`);
});
// These two simulate a Linux host in-process: a colon-separated PATH of real fixture directories and POSIX paths. On a
// Windows runner the fixture directories carry a drive letter ("D:\\..."), which a colon PATH cannot hold, so they run
// on Mac and Linux only; the platform functions they call are pure, so the Windows controls run there too.
const POSIX_HOST_ONLY = process.platform === "win32" && "simulates a Linux host (colon PATH of real fixture directories)";
test("[behavior] Linux direct holds in every managed-cargo helper, whatever control root, socket or shim the environment names (Mac control broker-managed)", { skip: POSIX_HOST_ONLY }, (t) => {
  // An environment that makes a Mac broker-managed: a control root holding runtime-current/rustright, a broker socket, the agent-bin shim first.
  const dir = tmp(t, "rk-gc-helpers-");
  const control = path.join(dir, "control"); const agentBin = path.join(dir, ".rightkit-managed", "agent-bin"); const toolchain = path.join(dir, "toolchain");
  write(control, { "runtime-current/rustright": "#!/bin/sh\n" }); write(agentBin, { cargo: "#!/bin/sh\n# cargo-guard.mjs\n" }); write(toolchain, { cargo: "#!/bin/sh\n" });
  const env = { RIGHTKIT_CONTROL_ROOT: control, RIGHTKIT_DATA_ROOT: control, RIGHTKIT_BUILD_BROKER_SOCKET: path.join(control, "broker.sock"), PATH: `${agentBin}:${toolchain}:/usr/bin`, HOME: dir };
  assert.equal(directPlatform("linux"), true); assert.equal(directPlatform("darwin"), false); assert.equal(directPlatform("win32"), false);
  assert.equal(managedRuntimeClient(env, "linux"), undefined, "Linux finds no broker client");
  assert.equal(managedRuntimeClient(env, "darwin"), path.join(control, "runtime-current", "rustright"), "control: the same env on a Mac finds it");
  assert.equal(brokerManagedHost(env, "linux"), false); assert.equal(brokerManagedHost(env, "darwin"), true);
  assert.equal(managedCargoShimOnPath(env, "linux"), false); assert.equal(managedCargoShimOnPath(env, "darwin"), true);
  const launches = [];
  const record = (command, args, options) => { launches.push({ command, args, PATH: options?.env?.PATH }); return { status: 0, stdout: "", stderr: "" }; };
  runCargoSync(["metadata", "--format-version", "1"], { env }, { platform: "linux", spawn: record });
  assert.equal(launches[0].command, "cargo", "Linux runs the real cargo, never the client");
  assert.deepEqual(launches[0].args, ["metadata", "--format-version", "1"]);
  assert.equal(launches[0].PATH, `${toolchain}:/usr/bin`, "the agent-bin shim directory is off PATH");
  runCargoSync(["metadata"], { env }, { platform: "darwin", spawn: record, exists: (p) => p === path.join(control, "runtime-current", "rustright") || p === env.RIGHTKIT_BUILD_BROKER_SOCKET });
  assert.equal(launches[1].command, path.join(control, "runtime-current", "rustright"), "control: on a Mac the broker client runs");
  const dev = developmentEnvironment(env, "linux");
  assert.equal(dev.PATH, `${toolchain}:/usr/bin`, "a Linux dev build never re-adds the agent-bin");
  assert.equal(dev.RIGHTKIT_BUILD_BROKER_SOCKET, env.RIGHTKIT_BUILD_BROKER_SOCKET, "an inherited variable is left, but nothing on Linux reads it");
  const exec = resolveExecutor({ env, platform: "linux", exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), which: () => [path.join(agentBin, "cargo")] });
  assert.equal(exec.kind, "direct"); assert.equal(exec.env.PATH, `${toolchain}:/usr/bin`);
});

test("[behavior] Linux direct holds at every launch boundary: quiet lease, sidecar host probe, nested-broker check, configured rustright commands (Mac/Windows controls)", { skip: POSIX_HOST_ONLY }, async (t) => {
  const { quietLeaseLane } = await import("./bin/rightkit-gate.mjs");
  const { resolveCommand } = await import("./development.mjs");
  const { isNestedBrokerContext } = await import("./stage-sidecar.mjs");
  const { buildLaunch } = await import("./build-launch.mjs");
  const dir = tmp(t, "rk-gc-launch-");
  const agentBin = path.join(dir, ".rightkit-managed", "agent-bin"); const toolchain = path.join(dir, "toolchain");
  write(agentBin, { cargo: "#!/bin/sh\n# cargo-guard.mjs\n", rustright: "#!/bin/sh\n" }); write(toolchain, { cargo: "#!/bin/sh\n", rustc: "#!/bin/sh\n" });
  const env = { PATH: `${agentBin}:${toolchain}:/usr/bin`, RIGHTKIT_BUILD_BROKER_SOCKET: path.join(dir, "broker.sock"), HOME: dir };
  // Quiet lease: a direct host never asks the broker, even with rustright on PATH.
  const spawned = [];
  const lane = (platform) => quietLeaseLane({ platform, out: () => {}, spawn: (cmd, args) => { spawned.push([cmd, ...args].join(" ")); return cmd === "which" || cmd === "where" ? { status: 0, stdout: `${agentBin}/rustright\n` } : { status: 1, stdout: "", stderr: "x" }; } });
  assert.equal(await lane("linux").acquire({}), null);
  assert.ok(!spawned.some((c) => c.startsWith("rustright quiet")), "no broker quiet lease on Linux");
  await lane("darwin").acquire({});
  assert.ok(spawned.some((c) => c.startsWith("rustright quiet acquire")), "control: a Mac asks the broker");
  // Sidecar host probe and configured commands: rustright cargo|rustc|rustdoc run the real tool on Linux, shims off PATH.
  for (const tool of ["cargo", "rustc", "rustdoc"]) {
    const r = resolveCommand("rustright", [tool, "-vV"], { root: dir, env, platform: "linux" });
    assert.deepEqual([r.command, r.args], [tool, ["-vV"]], tool);
    assert.equal(r.env.PATH, `${toolchain}:/usr/bin`, `${tool}: agent-bin off PATH`);
  }
  assert.throws(() => resolveCommand("rustright", ["quiet", "acquire"], { root: dir, env, platform: "linux" }), /no direct form/);
  // Nested-broker check: a broker socket variable means nothing on Linux.
  const tauri = { ...env, TAURI_ENV_PLATFORM: "linux" };
  assert.equal(isNestedBrokerContext(tauri, "linux"), false);
  assert.equal(isNestedBrokerContext(tauri, "darwin"), true, "control: on a Mac it is a nested broker context");
  const heavy = { PATH: "/usr/bin", TAURI_ENV_PLATFORM: "linux", RIGHTSUITE_HEAVY_WORK_OWNER: "1" };
  assert.equal(isNestedBrokerContext(heavy, "linux"), false, "heavy-command's owner flag means no broker on Linux either");
  assert.equal(isNestedBrokerContext(heavy, "darwin"), true, "control: Mac heavy owner inside Tauri is nested");
  assert.equal(isNestedBrokerContext(heavy, "win32"), true, "control: Windows heavy owner inside Tauri is nested");
  // The one build door: configured rustright cargo runs cargo directly with shims off PATH; plain cargo carries the same env.
  const exists = (p) => fs.existsSync(p);
  const viaDoor = buildLaunch("rustright", ["cargo", "build", "--locked"], { env, platform: "linux", exists });
  assert.deepEqual([viaDoor.command, viaDoor.args, viaDoor.env?.PATH], ["cargo", ["build", "--locked"], `${toolchain}:/usr/bin`]);
  const plain = buildLaunch("cargo", ["build"], { env, platform: "linux", exists });
  assert.deepEqual([plain.command, plain.env?.PATH], ["cargo", `${toolchain}:/usr/bin`]);
  assert.throws(() => buildLaunch("rustright", ["run", "rightkit-qa"], { env, platform: "linux", exists }), /no broker on this linux host/);
  const clientDir = path.join(dir, "client"); write(clientDir, { rustright: "#!/bin/sh\n" }); fs.chmodSync(path.join(clientDir, "rustright"), 0o755);
  const macControl = buildLaunch("cargo", ["build"], { env: { ...env, PATH: `${clientDir}:/usr/bin` }, platform: "darwin", exists });
  assert.deepEqual([macControl.command, macControl.args[0]], [path.join(clientDir, "rustright"), "cargo"], "control: on a Mac cargo is a broker job");
});

test("[behavior] E10: the executor has no caller input, so a human and a managed agent route the same way", () => {
  // resolveExecutor reads only the host (env, platform, PATH, files): an agent marker in the environment changes nothing.
  for (const host of ["mac", "dell", "linux"]) assert.deepEqual(route(host, { env: { CLAUDECODE: "1", RIGHTKIT_AGENT: "1" } }), route(host), host);
});

// ---- host policy per product: the execution policy right-release enforces before any local build ----------------------------------------
// assertReleaseExecutionAllowed (execution-policy.mjs) is what cli/right-release.mjs and development.mjs call before compiling; the
// build-control hook mirrors it (githubActionsOnlyViolation). A public (Orthic-Labs) app declares compile=github-actions-only in
// .rightgit.json and may build locally only with a .rightkit-local-build.json grant naming exactly its origin.
function policyApp(t, { origin, rightgit, grant }) {
  const root = tmp(t, "rk-gc-policy-");
  write(root, { ".git/config": origin ? `[remote "origin"]\n\turl = ${origin}\n` : "" });
  if (rightgit) write(root, { ".rightgit.json": JSON.stringify(rightgit) });
  if (grant) write(root, { [LOCAL_BUILD_FILE]: JSON.stringify(grant) });
  return root;
}
test("[behavior] host policy: a private app compiles locally; a public app needs its local-build grant or CI; RightKit's grant allows it", (t) => {
  const local = { GITHUB_ACTIONS: "" };
  const privateApp = policyApp(t, { origin: "git@github.com:bogusyogi/someapp.git" });
  assert.equal(assertReleaseExecutionAllowed({ cwd: privateApp, environment: local }).compile, null);
  const publicApp = policyApp(t, { origin: "git@github.com:Orthic-Labs/someapp.git", rightgit: { compile: GITHUB_ACTIONS_ONLY } });
  assert.throws(() => assertReleaseExecutionAllowed({ cwd: publicApp, environment: local }), (e) => e.code === GITHUB_ACTIONS_ONLY_ERROR);
  assert.equal(assertReleaseExecutionAllowed({ cwd: publicApp, environment: { GITHUB_ACTIONS: "true" } }).compile, GITHUB_ACTIONS_ONLY);
  const granted = policyApp(t, { origin: "git@github.com:Orthic-Labs/someapp.git", rightgit: { compile: GITHUB_ACTIONS_ONLY }, grant: { allow: "build-test-publish", repository: "Orthic-Labs/someapp" } });
  assert.equal(assertReleaseExecutionAllowed({ cwd: granted, environment: local }).compile, GITHUB_ACTIONS_ONLY);
  const wrongGrant = policyApp(t, { origin: "git@github.com:Orthic-Labs/someapp.git", rightgit: { compile: GITHUB_ACTIONS_ONLY }, grant: { allow: "build-test-publish", repository: "Orthic-Labs/other" } });
  assert.throws(() => assertReleaseExecutionAllowed({ cwd: wrongGrant, environment: local }), (e) => e.code === GITHUB_ACTIONS_ONLY_ERROR);
  // RightKit itself: its committed grant names its own origin.
  const rk = JSON.parse(read(LOCAL_BUILD_FILE));
  assert.equal(rk.allow, "build-test-publish");
});
test("[pending] A-M, A-D, A-L: a public app's local compile on its origin host (contract §2 requires one for every cell; RK_GITHUB_ACTIONS_ONLY refuses a public app without a local-build grant)", { todo: "owned by Adrian (execution policy: which public apps get a local-build grant) and step 11 (installed runs)" }, () => {
  assert.fail("a public app without a grant cannot compile locally today");
});

// ---- E1: class A first, with real subprocesses ----------------------------------------------------------------------------------------
// A driver runs runScans in a child process without the test harness's broker guard (it would stop the launches the test must observe).
// Fakes for cargo and a test script write sentinels when launched; class A is red (node --check of a broken file) or green.
function e1Fixture(t, { red }) {
  const dir = tmp(t, "rk-gc-e1-");
  const bin = path.join(dir, "bin"); fs.mkdirSync(bin);
  const compiled = path.join(dir, "compile-ran"); const tested = path.join(dir, "test-ran");
  if (process.platform === "win32") write(bin, { "cargo.cmd": `@if "%1"=="--version" (echo cargo 1.99.0-fake) else (echo %*> "${compiled}")\r\n` });
  else { write(bin, { cargo: `#!/bin/sh\nif [ "$1" = "--version" ]; then echo "cargo 1.99.0-fake"; else echo "$*" > '${compiled}'; fi\n` }); fs.chmodSync(path.join(bin, "cargo"), 0o755); }
  write(dir, { "content/src/a.mjs": red ? "export const = ;\n" : "export const a = 1;\n" });
  const plan = {
    schema: "fixture", roots: { contentRoot: path.join(dir, "content"), liveRoot: path.join(dir, "content") }, changed: ["src/a.mjs"], notes: [], deferred: [],
    scans: [
      { id: "node-check:src/a.mjs", class: "A", tool: "node", args: ["--check", "src/a.mjs"], root: "content", command: "node --check src/a.mjs" },
      { id: "check:fixture", class: "B", tool: "cargo", args: ["check", "--locked"], root: "live", command: "cargo check --locked" },
      { id: "test:npm:fixture", class: "B", kind: "test", executes: true, tool: "node", args: ["-e", `require("node:fs").writeFileSync(${JSON.stringify(tested)}, "ran"); console.log("# pass 1"); console.log("# fail 0")`], root: "live", command: "node -e <test>" },
    ],
  };
  const driver = path.join(dir, "driver.mjs");
  fs.writeFileSync(driver, `const { runScans } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "deterministic-scans.mjs")).href)});
const plan = ${JSON.stringify(plan)};
const r = await runScans(plan, { classes: ["A", "B"], jobs: 1 });
console.log(JSON.stringify(r.scans.map((s) => ({ id: s.id, status: s.status, code: s.code ?? null }))));\n`);
  const env = { ...process.env, NODE_OPTIONS: "", PATH: `${bin}${path.delimiter}${process.env.PATH}` };
  const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", cwd: dir });
  const rows = JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop() || "[]");
  return { rows, compiled: fs.existsSync(compiled), tested: fs.existsSync(tested), stderr: r.stderr };
}

test("[behavior] E1: a red class A starts no compile and no later-stage scan (real subprocesses, sentinel fakes)", (t) => {
  const red = e1Fixture(t, { red: true });
  const by = Object.fromEntries(red.rows.map((r) => [r.id, r]));
  assert.equal(by["node-check:src/a.mjs"]?.status, "fail", red.stderr);
  assert.equal(by["check:fixture"]?.status, "not-run");
  assert.equal(by["test:npm:fixture"]?.status, "not-run");
  assert.equal(red.compiled, false, "no compile launched");
  assert.equal(red.tested, false, "no post-class-A test launched");
  // Control: the same plan with class A green launches both, so the sentinels would have shown a launch.
  const green = e1Fixture(t, { red: false });
  assert.equal(Object.fromEntries(green.rows.map((r) => [r.id, r.status]))["node-check:src/a.mjs"], "pass", green.stderr);
  assert.equal(green.compiled, true, "control: the fake compiler is reachable");
  assert.equal(green.tested, true, "control: the later-stage test is reachable");
});

test("[behavior] E1: the door's dev and test stages never launch right-release after a red check", async (t) => {
  const repo = tmp(t, "rk-gc-door-");
  for (const a of [["init", "-q", "-b", "main"], ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "c"]]) spawnSync("git", a, { cwd: repo });
  const launches = []; const out = []; const err = [];
  const io = { cwd: repo, out: (m) => out.push(m), err: (m) => err.push(m), resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), runCheck: async () => 1, spawnInherit: (cmd, args) => { launches.push([cmd, ...args]); return { status: 0 }; } };
  for (const to of ["dev", "test"]) {
    assert.equal(await main(["--to", to], io), 1);
    assert.ok(err.some((l) => l.includes(`--to ${to} stopped at check: nothing compiled`)), err.join("\n"));
  }
  assert.deepEqual(launches, [], "right-release was never launched");
  // Control: a green check does launch it, so the recorder would have seen a launch.
  const green = { ...io, runCheck: async () => 0 };
  assert.equal(await main(["--to", "dev"], green), 0);
  assert.equal(launches.length, 1);
  assert.match(launches[0].join(" "), /right-release\.mjs dev/);
});

test("[behavior] E1: a class A row that executes repository code (lint, generator) and launches a compiler is refused by the launch guard", (t) => {
  // A non-test class A row with executes:true, like eslint or generated-docs, whose code spawns `cargo build`: the guard refuses it before the
  // fake cargo runs. Real child process, harness broker guard dropped; control: the same row unguarded (class B) does launch the fake.
  const dir = tmp(t, "rk-gc-exec-");
  const bin = path.join(dir, "bin"); fs.mkdirSync(bin);
  const ran = path.join(dir, "compile-ran");
  if (process.platform === "win32") write(bin, { "cargo.cmd": `@echo %*> "${ran}"\r\n` });
  else { write(bin, { cargo: `#!/bin/sh\necho "$*" > '${ran}'\n` }); fs.chmodSync(path.join(bin, "cargo"), 0o755); }
  write(dir, { "content/gen.mjs": "const r = require('node:child_process').spawnSync('cargo', ['build'], { shell: true, stdio: 'inherit' }); process.exit(r.status ?? 1);\n".replace("require('node:child_process')", "(await import('node:child_process'))") });
  const runWith = (cls) => {
    const plan = { schema: "fixture", roots: { contentRoot: path.join(dir, "content"), liveRoot: path.join(dir, "content") }, changed: ["gen.mjs"], notes: [], deferred: [],
      scans: [{ id: "node-check:gen.mjs", class: "A", tool: "node", args: ["--check", "gen.mjs"], root: "content", command: "node --check gen.mjs" },
        { id: "generated-docs", class: cls, executes: true, tool: "node", args: ["gen.mjs"], root: "live", command: "node gen.mjs" }] };
    const driver = path.join(dir, `driver-${cls}.mjs`);
    fs.writeFileSync(driver, `const { runScans } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "deterministic-scans.mjs")).href)});
const r = await runScans(${JSON.stringify(plan)}, { classes: ["A", "B"], jobs: 1 });
console.log(JSON.stringify(r.scans.map((s) => ({ id: s.id, status: s.status, detail: String(s.detail ?? "").slice(0, 400) }))));\n`);
    const r = spawnSync(process.execPath, [driver], { env: { ...process.env, NODE_OPTIONS: "", PATH: `${bin}${path.delimiter}${process.env.PATH}` }, encoding: "utf8", cwd: dir });
    return JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop() || "[]").find((s) => s.id === "generated-docs");
  };
  const guarded = runWith("A");
  assert.equal(guarded?.status, "fail");
  assert.match(guarded.detail, /class A refuses a compile\/emit launch \(cargo\)/);
  assert.equal(fs.existsSync(ran), false, "no compile launched under class A");
  const control = runWith("B");
  assert.equal(fs.existsSync(ran), true, `control: unguarded, the row launches the fake cargo (${JSON.stringify(control)})`);
});

// ---- the inventory against the canonical enumerations ([drift]) ------------------------------------------------------------------------
/** Rows of the inventory's tables: first cell a backticked id, second the class it records for the code today. */
function inventoryRows(section) {
  const text = fs.readFileSync(INVENTORY, "utf8");
  const start = text.indexOf(`\n## ${section}`);
  assert.ok(start >= 0, `the inventory has a "## ${section}" section`);
  const end = text.indexOf("\n## ", start + 1);
  const body = text.slice(start, end < 0 ? undefined : end);
  return Object.fromEntries([...body.matchAll(/^\| `([^`]+)` \| ([^|]+) \|/gm)].filter((m) => m[1] !== "id").map((m) => [m[1], m[2].trim()]));
}
const prefixOf = (id) => (/^test:/.test(id) ? id.split(":").slice(0, 2).join(":") : id.split(":")[0]);

test("[drift] the class A inventory names every scan prefix and test class the planner can emit, with the class the code gives it", () => {
  const rows = inventoryRows("Scans");
  const code = { ...Object.fromEntries(CLASS_A_SCAN_PREFIXES.map((p) => [p, "A"])), ...Object.fromEntries(CLASS_B_SCAN_PREFIXES.map((p) => [p, "B"])), ...TEST_SCAN_CLASS };
  assert.deepEqual(Object.keys(rows).sort(), Object.keys(code).sort(), "inventory rows = CLASS_A_SCAN_PREFIXES + CLASS_B_SCAN_PREFIXES + TEST_SCAN_CLASS");
  for (const [id, cls] of Object.entries(code)) assert.match(rows[id], new RegExp(`^${cls}\\b`), `${id}: the inventory records class ${rows[id]}, the code says ${cls}`);
  for (const id of LIVE_SCAN_IDS) assert.ok(rows[id], `live scan ${id} is inventoried`);
  for (const id of REGISTRY_SCAN_IDS) assert.ok(rows[id], `registry-dependent scan ${id} is inventoried`);
});

test("[drift] the inventory names every app rule and RightKit preflight rule, with its tier", () => {
  const rows = inventoryRows("Rules");
  const ids = [...new Set([...APP_RULES, ...PREFLIGHT_RULES].map((r) => r.id))].sort();
  assert.deepEqual(Object.keys(rows).sort(), ids);
  for (const r of APP_RULES) assert.match(rows[r.id], new RegExp(`^A \\(app, ${COMPILE_TIER_IDS.has(r.id) ? "compile" : "release"} tier\\)`), r.id);
  for (const r of PREFLIGHT_RULES.filter((p) => !APP_RULES.some((a) => a.id === p.id))) assert.match(rows[r.id], /^A \(RightKit preflight\)/, r.id);
});

test("[drift] the inventory names every stage, executor kind, entry bin and execution policy the code declares", () => {
  const rows = inventoryRows("Entries, stages and executors");
  const bins = Object.keys(JSON.parse(read("packages/release/package.json")).bin ?? {});
  const want = [...STAGES.map((s) => `stage:${s}`), ...EXECUTOR_KINDS.map((k) => `executor:${k}`), ...bins.map((b) => `bin:${b}`), `policy:${GITHUB_ACTIONS_ONLY}`, `policy:${LOCAL_BUILD_FILE}`].sort();
  assert.deepEqual(Object.keys(rows).sort(), want);
});

test("[behavior] every scan the planner emits for a broad fixture repository has an inventory row with the same class", (t) => {
  const root = tmp(t, "rk-gc-plan-");
  write(root, {
    "package.json": JSON.stringify({ name: "fixture", private: true, scripts: { test: "node --test" } }), "pnpm-lock.yaml": "lockfileVersion: '9.0'\n", "pnpm-workspace.yaml": "packages:\n  - packages/*\n",
    "packages/web/package.json": JSON.stringify({ name: "@fixture/web", scripts: { test: "node --test" } }), "packages/web/tsconfig.json": "{}", "packages/web/eslint.config.mjs": "export default [];\n", "packages/web/src/a.mjs": "export {};\n",
    "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n', "Cargo.lock": "version = 4\n", "deny.toml": "", "crates/core/Cargo.toml": '[package]\nname = "core"\nversion = "0.1.0"\nedition = "2021"\n', "crates/core/src/lib.rs": "\n", "crates/core/CHANGELOG.md": "# Changelog\n",
    ".github/workflows/ci.yml": "on: push\njobs: {}\n", "scripts/run.sh": "#!/bin/sh\necho hi\n", "data/a.json": "{}", "data/b.toml": "a = 1\n", "data/c.yaml": "a: 1\n",
    "packages/web/biome.json": "{}", "ci/advisory-exceptions.json": "{\"exceptions\":[]}\n", "scripts/ci/a.test.mjs": "\n", "Package.swift": "// swift-tools-version:5.9\n",
    "packages/release/rightkit-ownership.json": "{}", "crates/rightkit-ownership/catalog/ownership-catalog.json": "{}",
  });
  const files = []; const walk = (d, rel) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const r = rel ? `${rel}/${e.name}` : e.name; if (e.isDirectory()) walk(path.join(d, e.name), r); else files.push(r); } };
  walk(root, "");
  const plan = planScans({ changed: files, contentRoot: root, liveRoot: root, trustedRoot: root, includeTests: true, quietTests: "local", tier: "release", cargoCommand: ["cargo"],
    rustUnits: [{ name: "core", dir: "crates/core", crate: "core", cargoDir: "." }], npmUnits: [{ name: "web", dir: "packages/web" }], npmRoots: ["."], liveExecution: true,
    crateFeatures: { core: { alsoSerial: true } }, platform: "darwin" });
  const rows = inventoryRows("Scans");
  // The prefixes this fixture must emit (the rest need inputs a fixture cannot give honestly: path-policy a refused path, suite-api RightKit's
  // in-tree generator, generated-docs its generator inputs, check/check-dependent/check-features feature or lock layouts, check-all-features no
  // producer; each is still bound by the [drift] row test).
  const emitted = new Set(plan.scans.map((s) => prefixOf(s.id)));
  for (const p of ["rustfmt", "node-check", "json-parse", "toml-parse", "yaml-parse", "ownership-catalog", "actionlint", "shellcheck", "pnpm-lockfile", "pnpm-audit", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list", "changelog-version", "license", "secret-scan", "advisory-exceptions", "tsc", "eslint", "biome", "clippy", "doc", "swift-build", "test:rust", "test:rust-serial", "test:npm", "test:scripts-ci"]) assert.ok(emitted.has(p), `the fixture emits ${p} (emitted: ${[...emitted].sort().join(", ")})`);
  for (const s of plan.scans) {
    const p = prefixOf(s.id);
    assert.ok(rows[p], `${s.id}: prefix ${p} has no inventory row`);
    assert.match(rows[p], new RegExp(`^${s.class}\\b`), `${s.id}: planned class ${s.class}, inventory ${rows[p]}`);
  }
});

// ---- joint plan step 4c: the door builds the selected commit's gate-owned snapshot (direct host) ----

test("[behavior] E16/§1: on Linux the door compiles the selected commit's snapshot; every cargo scan writes to the checkout's resolved target directory, never into the snapshot (real subprocesses, fake cargo)", { skip: process.platform === "win32" ? "POSIX fake cargo" : false }, (t) => {
  const dir = tmp(t, "rk-gc-4c-");
  const toolDir = path.join(dir, "toolchain", "bin");
  // Fake cargo: `metadata` answers for the directory it runs in, with target_directory <cwd>/rk-target, so the checkout's answer
  // (<checkout>/rk-target) and a snapshot's (<snapshot>/rk-target) differ. Every call is logged.
  write(toolDir, { cargo: `#!/bin/sh\necho "$PWD $*" >> '${path.join(dir, "cargo-ran")}'\nif [ "$1" = "metadata" ]; then node -e 'const r=process.cwd();console.log(JSON.stringify({packages:[{name:"core",manifest_path:r+"/crates/core/Cargo.toml",publish:null,id:"core",targets:[{kind:["lib"],name:"core",src_path:r+"/crates/core/src/lib.rs"}],dependencies:[],features:{}}],workspace_members:["core"],workspace_root:r,target_directory:r+"/rk-target",resolve:null}))'; exit 0; fi\nexit 0\n` });
  fs.chmodSync(path.join(toolDir, "cargo"), 0o755);
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  spawnSync("git", ["init", "-q", "-b", "main"], { cwd: repo });
  write(repo, { "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n', "Cargo.lock": "version = 4\n", "crates/core/Cargo.toml": '[package]\nname = "core"\nversion = "0.1.0"\nedition = "2021"\n', "crates/core/src/lib.rs": "pub fn committed() {}\n" });
  spawnSync("git", ["add", "-A"], { cwd: repo }); spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c"], { cwd: repo });
  const sha = String(spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout).trim();
  // The checkout is dirty: the door builds the commit anyway, and the edit never reaches the snapshot.
  fs.writeFileSync(path.join(repo, "crates/core/src/lib.rs"), "pub fn uncommitted() {}\n");
  const driver = path.join(dir, "driver.mjs");
  fs.writeFileSync(driver, `const { main } = await import(${JSON.stringify(pathToFileURL(path.join(HERE, "bin", "rightkit-gate.mjs")).href)});
const fs = await import("node:fs");
const out = []; const plans = [];
const code = await main(["--to", "compile", "--commit", ${JSON.stringify(sha)}], { cwd: ${JSON.stringify(repo)}, platform: "linux", out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0,
  runScans: async (plan) => { plans.push(plan.scans.filter((s) => s.tool === "cargo").map((s) => ({ id: s.id, cls: s.class, env: s.env ?? {}, root: plan.roots.liveRoot }))); return { scans: [], notes: [], cacheHits: [] }; } });
console.log(JSON.stringify({ code, out: out.join("\\n").slice(-40000), plans }));\n`);
  const env = { ...process.env, NODE_OPTIONS: "", RK_GATE_CACHE: "0", PATH: `${toolDir}:${process.env.PATH}`, RIGHTKIT_SNAPSHOT_ROOT: path.join(dir, "snaps"), RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP;
  const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", cwd: repo });
  let res = {}; try { res = JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { res = { stderr: r.stderr, stdout: r.stdout }; }
  assert.equal(res.code, 0, JSON.stringify(res).slice(0, 3000));
  const snapDir = (res.out.match(/snapshot (\S+) is commit/) ?? [])[1];
  assert.ok(snapDir && snapDir.startsWith(path.join(dir, "snaps")), `the door synced the gate-owned snapshot (${snapDir})`);
  assert.equal(fs.readFileSync(path.join(snapDir, "crates/core/src/lib.rs"), "utf8"), "pub fn committed() {}\n", "the snapshot holds the commit, not the dirty checkout");
  assert.match(res.out, /build ownership held /);
  assert.match(res.out, new RegExp(`output directory for workspace \\.: ${path.join(repo, "rk-target").replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} \\(the checkout's\\)`));
  // The check stage plans first (class A runs from it); the compile stage's plan is the last one.
  assert.equal(res.plans.length, 2, "one plan for the check stage, one for the compile stage");
  const compile = res.plans[1];
  assert.ok(compile.some((s) => s.cls === "B"), "the compile stage planned class B cargo scans");
  assert.equal(compile[0].root, snapDir, "class B reads the snapshot");
  for (const s of compile.filter((x) => x.cls === "B")) {
    assert.equal(s.env.CARGO_TARGET_DIR, path.join(repo, "rk-target"), `${s.id} writes to the checkout's target directory`);
    assert.ok(!s.env.CARGO_TARGET_DIR.startsWith(snapDir), `${s.id} never writes into the snapshot`);
  }
  // The only cargo the gate ran in the checkout is the read-only metadata that resolved its target directory.
  const ran = fs.readFileSync(path.join(dir, "cargo-ran"), "utf8").trim().split("\n");
  for (const line of ran.filter((l) => l.startsWith(`${repo} `))) assert.match(line, /^\S+ metadata --offline --format-version 1 --no-deps --manifest-path /, line);
  assert.equal(fs.existsSync(path.join(dir, "build-ownership", "owner.json")), false, "the door released the host");
  // The direct host's own receipt (step 4d): broker field names, the commit, trees at start and exit, the ownership and the outputs.
  const line = res.out.split("\n").find((l) => l.startsWith("rightkit-gate: direct receipt "));
  assert.ok(line, "the compile printed its direct receipt");
  const receipt = JSON.parse(line.slice("rightkit-gate: direct receipt ".length));
  assert.equal(receipt.schema, "rightkit.gate-direct-receipt/1"); assert.equal(receipt.executor, "direct");
  assert.deepEqual(receipt.commit, { sha, tree: String(spawnSync("git", ["rev-parse", `${sha}^{tree}`], { cwd: repo, encoding: "utf8" }).stdout).trim() });
  assert.deepEqual(receipt.snapshot, { path: snapDir, treeAtStart: receipt.commit.tree, treeAtExit: receipt.commit.tree });
  assert.match(receipt.ownership.handle ?? "", /^[0-9a-f-]{36}$/, "the door's ownership handle");
  assert.equal(receipt.outputDir, path.join(repo, "rk-target")); assert.equal(receipt.exitStatus, 0);
  assert.match(receipt.requestId, /^[0-9a-f-]{36}$/); assert.ok(Number.isFinite(Date.parse(receipt.issuedAt)));
});

test("[behavior] E6/§1: the door builds the commit, not the checkout: a dirty checkout neither blocks nor leaks, an edit mid-stage never reaches the snapshot, a tampered snapshot voids the stage (exit 2), a second request waits for the held ownership (exit 1)", { skip: process.platform === "win32" && "simulates a Linux host, whose ownership reads ps" }, async (t) => {
  const dir = tmp(t, "rk-gc-4c-e6-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  spawnSync("git", ["init", "-q", "-b", "main"], { cwd: repo });
  write(repo, { "README.md": "committed\n", "package.json": '{"name":"x","private":true}\n' });
  spawnSync("git", ["add", "-A"], { cwd: repo }); spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c"], { cwd: repo });
  const sha = String(spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout).trim();
  fs.writeFileSync(path.join(repo, "README.md"), "dirty\n");
  const env = { ...process.env, RIGHTKIT_SNAPSHOT_ROOT: path.join(dir, "snaps"), RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" });
  const door = async (compile, extra = {}) => {
    const out = [];
    // No receipt carries over between these runs: each one compiles.
    const code = await main(["--to", "compile", "--commit", sha], { cwd: repo, platform: "linux", env, spawn, out: (m) => out.push(m), err: (m) => out.push(m), runCheck: async () => 0, runCompile: compile, readReceipt: () => null, writeReceipt: () => {}, ...extra });
    return { code, out: out.join("\n") };
  };
  const snapReadme = () => { const m = fs.readdirSync(path.join(dir, "snaps")).find((n) => !n.includes(".")); return path.join(dir, "snaps", m, "README.md"); };
  // An edit to the checkout during the stage: the snapshot keeps the commit's bytes, the stage passes.
  let ok = await door(async () => { fs.writeFileSync(path.join(repo, "README.md"), "edited during the stage\n"); return fs.readFileSync(snapReadme(), "utf8") === "committed\n" ? 0 : 1; });
  assert.equal(ok.code, 0, ok.out);
  assert.match(ok.out, /--to compile passed/);
  // A second request while this door holds the host: refused before anything runs (exit 1), the snapshot untouched.
  let second = null;
  ok = await door(async () => {
    const before = fs.statSync(snapReadme()).mtimeMs;
    const out = []; const env2 = { ...env }; delete env2.RIGHTKIT_BUILD_OWNERSHIP; delete env2.RIGHTKIT_BUILD_OWNERSHIP_FILE;
    const code = await main(["--to", "compile", "--commit", sha], { cwd: repo, platform: "linux", env: env2, spawn, out: (m) => out.push(m), err: (m) => out.push(m), runCheck: async () => 0, runCompile: async () => 0, readReceipt: () => null, writeReceipt: () => {} });
    second = { code, out: out.join("\n"), untouched: fs.statSync(snapReadme()).mtimeMs === before };
    return 0;
  });
  assert.equal(ok.code, 0, ok.out);
  assert.equal(second.code, 1, second.out); assert.match(second.out, /BLOCKED: build ownership .* held by pid \d+ \(door:compile of [0-9a-f]{40}\).*nothing ran/);
  assert.equal(second.untouched, true, "the second request never synced");
  // A snapshot tampered during the stage: the stage covers no known bytes (exit 2), recorded as not passed.
  const tampered = await door(async () => { fs.writeFileSync(snapReadme(), "tampered\n"); return 0; });
  assert.equal(tampered.code, 2, tampered.out); assert.match(tampered.out, /RK_CHECK_ERROR: \[compile\]/); assert.match(tampered.out, /stopped at compile: the result covers no known bytes/);
  // Hosted CI and broker hosts keep the checkout route (no ownership, no snapshot).
  const ci = await door(async () => 0, { env: { ...env, GITHUB_ACTIONS: "true" } });
  assert.doesNotMatch(ci.out, /build ownership|snapshot .* is commit/); assert.match(ci.out, /BLOCKED: the checkout differs from HEAD/, "hosted CI keeps today's clean-checkout rule");
  void git;
});


// Step 4c review fixtures: the commit decides, never the checkout; the snapshot compile keeps the guarded repository-code rows.
function doorFixture(t, files, prefix) {
  const dir = tmp(t, prefix);
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  spawnSync("git", ["init", "-q", "-b", "main"], { cwd: repo });
  write(repo, files);
  spawnSync("git", ["add", "-A"], { cwd: repo }); spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c"], { cwd: repo });
  const sha = String(spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout).trim();
  const env = { ...process.env, RIGHTKIT_SNAPSHOT_ROOT: path.join(dir, "snaps"), RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" });
  const door = async (extra = {}) => {
    const out = [];
    const code = await main(["--to", "compile", "--commit", sha], { cwd: repo, platform: "linux", env, spawn, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {}, ...extra });
    return { code, out: out.join("\n") };
  };
  return { repo, sha, door };
}

test("[behavior] §1 4c review: the snapshot compile runs a RightKit-style docs generator from the verified snapshot, never defers it", { skip: process.platform === "win32" && "simulates a Linux host, whose ownership reads ps" }, async (t) => {
  const { door } = doorFixture(t, { "scripts/generate-index.mjs": "process.exit(0);\n", "crates/x/README.md": "x\n", "README.md": "r\n" }, "rk-gc-4c-gen-");
  const plans = [];
  const r = await door({ runScans: async (plan) => { plans.push(plan); return { scans: [], notes: [], cacheHits: [] }; } });
  assert.equal(r.code, 0, r.out);
  const row = plans.at(-1)?.scans.find((s) => s.id === "generated-docs");
  assert.ok(row, `the compile plan has the generated-docs row (${plans.at(-1)?.scans.map((s) => s.id).join(", ")})`);
  assert.equal(row.deferred, undefined, `generated-docs runs, it is not deferred (${row.deferred})`);
  assert.equal(row.executes, true, "it is a guarded repository-code row");
  assert.equal(plans.at(-1).roots.liveRoot, plans.at(-1).roots.contentRoot, "it runs from the snapshot itself");
});

test("[behavior] §1 4c review: whether the compile stages sidecars is the commit's config, whatever the checkout adds, edits or hides", { skip: process.platform === "win32" && "simulates a Linux host, whose ownership reads ps" }, async (t) => {
  // The commit declares sidecars; the checkout removed them: still refused (part two of 4c stages them from a snapshot).
  const declared = doorFixture(t, { "right-release.config.json": '{"sidecars":[{"name":"a"}]}\n', "README.md": "r\n" }, "rk-gc-4c-side-");
  fs.writeFileSync(path.join(declared.repo, "right-release.config.json"), "{}\n");
  let compiled = false;
  let r = await declared.door({ runCompile: async () => { compiled = true; return 0; } });
  assert.equal(r.code, 1, r.out); assert.match(r.out, /BLOCKED: \[compile\] right-release\.config\.json stage sidecars/); assert.equal(compiled, false, "nothing compiled");
  // The commit declares none; the checkout added some: compiles.
  const none = doorFixture(t, { "right-release.config.json": "{}\n", "README.md": "r\n" }, "rk-gc-4c-side2-");
  fs.writeFileSync(path.join(none.repo, "right-release.config.json"), '{"sidecars":[{"name":"a"}]}\n');
  r = await none.door({ runCompile: async () => { compiled = true; return 0; } });
  assert.equal(r.code, 0, r.out); assert.equal(compiled, true, "the commit's config has no sidecars, so the compile ran");
});

test("[behavior] §1 4c review: an npm test that reaches cargo during a snapshot compile writes to the checkout's target directory (real pnpm -> script -> cargo chain), or is refused before launch when its workspace cannot be proven", { skip: process.platform === "win32" ? "POSIX fake cargo" : false }, (t) => {
  const dir = tmp(t, "rk-gc-4c-npm-");
  const snap = path.join(dir, "snap"); const checkoutTarget = path.join(dir, "checkout", "target");
  write(snap, {
    "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n', "crates/core/Cargo.toml": '[package]\nname = "core"\nversion = "0.1.0"\n', "crates/core/src/lib.rs": "\n",
    "package.json": '{"name":"root","private":true}\n', "pnpm-workspace.yaml": "packages:\n  - packages/*\n",
    "packages/app/package.json": '{"name":"app","private":true,"scripts":{"test":"cargo test --locked"}}\n',
    "packages/redirect/package.json": '{"name":"redirect","private":true,"scripts":{"test":"cargo test --locked --manifest-path ../../crates/core/Cargo.toml"}}\n',
  });
  write(path.join(dir, "outside"), { "packages/lonely/package.json": "{}\n" });
  const files = ["Cargo.toml", "crates/core/Cargo.toml", "crates/core/src/lib.rs", "package.json", "pnpm-workspace.yaml", "packages/app/package.json", "packages/redirect/package.json"];
  const plan = planScans({ changed: files, contentRoot: snap, liveRoot: snap, trustedRoot: snap, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
    rustUnits: [{ name: "core", dir: "crates/core", crate: "core", cargoDir: "." }], npmUnits: [{ name: "app", dir: "packages/app" }, { name: "redirect", dir: "packages/redirect" }], npmRoots: ["."],
    liveExecution: true, targetDirs: { ".": checkoutTarget }, platform: process.platform });
  const app = plan.scans.find((s) => s.id === "test:npm:app");
  assert.ok(app, `test:npm:app is planned (${plan.scans.map((s) => s.id).join(", ")})`);
  assert.equal(app.env.CARGO_TARGET_DIR, checkoutTarget, "bound to the enclosing workspace's checkout target directory");
  assert.equal(plan.scans.some((s) => s.id === "test:npm:redirect"), false, "a script that redirects cargo is never launched");
  assert.ok(plan.scans.some((s) => s.internal === "refuse" && /npm unit redirect: .*the path to cargo \(test\) redirects it/.test(s.detail)), "it is refused with the reason");
  // The real chain: pnpm runs the package's test script, which runs cargo; the fake cargo records the directory it was given.
  const pnpm = spawnSync(process.platform === "win32" ? "where" : "which", ["pnpm"], { encoding: "utf8" });
  if (pnpm.status !== 0) { t.diagnostic("no pnpm on this host: the planned env was checked, the nested launch was not"); return; }
  const toolDir = path.join(dir, "bin");
  write(toolDir, { cargo: `#!/bin/sh\necho "cargo $* CARGO_TARGET_DIR=$CARGO_TARGET_DIR" >> '${path.join(dir, "cargo-ran")}'\nexit 0\n` });
  fs.chmodSync(path.join(toolDir, "cargo"), 0o755);
  const r = spawnSync(app.tool, app.args, { cwd: snap, encoding: "utf8", env: { ...process.env, ...app.env, PATH: `${toolDir}:${process.env.PATH}`, NODE_OPTIONS: "" } });
  assert.equal(r.status, 0, `${r.stdout}\n${r.stderr}`);
  const ran = fs.readFileSync(path.join(dir, "cargo-ran"), "utf8").trim();
  assert.equal(ran, `cargo test --locked CARGO_TARGET_DIR=${checkoutTarget}`, "the nested cargo wrote to the checkout's target directory, not the snapshot's");
  assert.equal(fs.existsSync(path.join(snap, "target")), false);
});

test("[behavior] §1 4c review: an npm unit with no enclosing Cargo workspace that reaches cargo is refused before launch on a snapshot compile", () => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gc-4c-npm2-")));
  try {
    write(dir, { "package.json": '{"name":"root","private":true}\n', "pnpm-workspace.yaml": "packages:\n  - tools/*\n", "tools/x/package.json": '{"name":"x","private":true,"scripts":{"test":"cargo build"}}\n' });
    const plan = planScans({ changed: ["package.json", "pnpm-workspace.yaml", "tools/x/package.json"], contentRoot: dir, liveRoot: dir, trustedRoot: dir, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
      rustUnits: [], npmUnits: [{ name: "x", dir: "tools/x" }], npmRoots: ["."], liveExecution: true, targetDirs: {}, platform: process.platform });
    assert.equal(plan.scans.some((s) => s.id === "test:npm:x"), false);
    assert.ok(plan.scans.some((s) => s.internal === "refuse" && /npm unit x: .*reaches cargo from tools\/x, which no Cargo\.toml encloses/.test(s.detail)), plan.scans.map((s) => `${s.id} ${s.detail ?? ""}`).join("\n"));
    // Without a snapshot compile (no targetDirs) nothing changes: the row is planned as before.
    const live = planScans({ changed: ["package.json", "pnpm-workspace.yaml", "tools/x/package.json"], contentRoot: dir, liveRoot: dir, trustedRoot: dir, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
      rustUnits: [], npmUnits: [{ name: "x", dir: "tools/x" }], npmRoots: ["."], liveExecution: true, platform: process.platform });
    assert.equal(live.scans.some((s) => s.id === "test:npm:x"), true);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("[behavior] §1 4c review (target binding): only what a test reaches decides; a JS-only build needs no Cargo, an unreached script's cd blocks nothing, and a build hop into another package binds that package's workspace", () => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gc-4c-reach-")));
  try {
    write(dir, {
      "package.json": '{"name":"root","private":true}\n', "pnpm-workspace.yaml": "packages:\n  - packages/*\n  - native\n",
      // A: JS-only: builds with its own build script, then tests; no Cargo anywhere it reaches.
      "packages/js/package.json": '{"name":"js","private":true,"scripts":{"build":"tsc -p .","test":"pnpm run build && node --test"}}\n',
      // B: its test runs cargo directly; an unrelated release script cd's elsewhere and must not block it.
      "packages/rs/package.json": '{"name":"rs","private":true,"scripts":{"test":"cargo test --locked","release":"cd dist && cargo publish"}}\n',
      // C: its test builds the separate native package, whose build runs cargo in the native workspace.
      "packages/hop/package.json": '{"name":"hop","private":true,"scripts":{"test":"pnpm --dir ../../native run build && tsc --noEmit"}}\n',
      "native/package.json": '{"name":"native","private":true,"scripts":{"build":"cargo build --locked"}}\n',
      "native/Cargo.toml": '[workspace]\nmembers = []\n', "Cargo.toml": '[workspace]\nmembers = ["packages/rs"]\n', "packages/rs/Cargo.toml": '[package]\nname = "rs"\nversion = "0.1.0"\n',
    });
    const files = ["package.json", "pnpm-workspace.yaml", "packages/js/package.json", "packages/rs/package.json", "packages/hop/package.json", "native/package.json", "native/Cargo.toml", "Cargo.toml", "packages/rs/Cargo.toml"];
    const npmUnits = [{ name: "js", dir: "packages/js" }, { name: "rs", dir: "packages/rs" }, { name: "hop", dir: "packages/hop" }, { name: "native", dir: "native" }];
    const plan = (targetDirs, rustUnits) => planScans({ changed: files, contentRoot: dir, liveRoot: dir, trustedRoot: dir, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
      rustUnits, npmUnits, npmRoots: ["."], liveExecution: true, targetDirs, platform: process.platform });
    const row = (p, n) => p.scans.find((s) => s.id === `test:npm:${n}`);
    const refusedFor = (p, n) => p.scans.find((s) => s.internal === "refuse" && new RegExp(`npm unit ${n}:`).test(s.detail))?.detail ?? null;
    // JS-only repository (no Cargo workspace at all): nothing is bound and nothing refused.
    const jsOnly = plan({}, []);
    assert.ok(row(jsOnly, "js"), "a JS-only build test is planned"); assert.equal(row(jsOnly, "js").env.CARGO_TARGET_DIR, undefined); assert.equal(refusedFor(jsOnly, "js"), null);
    // With Rust: each test binds exactly the workspace it reaches.
    const targets = { ".": "/checkout/target", native: "/checkout/native/target" };
    const p = plan(targets, [{ name: "rs", dir: "packages/rs", crate: "rs", cargoDir: "." }]);
    // With Rust in the repository, `node --test` may start cargo from test code the scripts do not show: refused (fail-closed allowlist).
    assert.equal(row(p, "js"), undefined); assert.match(refusedFor(p, "js") ?? "", /runs `node --test` \(node\), which this check cannot follow/);
    assert.equal(row(p, "rs")?.env.CARGO_TARGET_DIR, "/checkout/target", `the unreached release script's cd blocks nothing (${refusedFor(p, "rs")})`);
    assert.equal(row(p, "hop")?.env.CARGO_TARGET_DIR, "/checkout/native/target", `the --dir hop binds the native workspace, not the unit's (${refusedFor(p, "hop")})`);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("[behavior] §1 4c review (mixed reach): a test that reaches cargo in two workspaces is refused before launch; every reach in one workspace still binds it", () => {
  // Root review of 431fbeae: only the first call was read, so `cargo test && pnpm --dir native run build` bound the enclosing workspace's
  // CARGO_TARGET_DIR for the whole test and native's independent build wrote into it.
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-gc-4c-mixed-")));
  try {
    write(dir, {
      "package.json": '{"name":"root","private":true}\n', "pnpm-workspace.yaml": "packages:\n  - packages/*\n  - native\n",
      // Direct cargo in the enclosing workspace, then a build hop into the independent native workspace.
      "packages/mix/package.json": '{"name":"mix","private":true,"scripts":{"test":"cargo test --locked && pnpm --dir ../../native run build"}}\n',
      // Two build hops, each into a different workspace.
      "packages/twohop/package.json": '{"name":"twohop","private":true,"scripts":{"test":"pnpm --dir ../../native run build && pnpm --dir ../rs run build && tsc --noEmit"}}\n',
      // Direct cargo and a hop that both land in the enclosing workspace: one workspace, still bound.
      "packages/same/package.json": '{"name":"same","private":true,"scripts":{"test":"cargo test --locked && pnpm --dir ../rs run build"}}\n',
      // The second reach hides behind a same-package script: still read.
      "packages/deep/package.json": '{"name":"deep","private":true,"scripts":{"test":"pnpm run unit && pnpm run e2e","unit":"cargo test --locked","e2e":"pnpm --dir ../../native run build"}}\n',
      // Root review of f883d5ca: (A) a same-package build script that itself hops; (B) retargets whatever the script is called, and exec.
      "packages/samehop/package.json": '{"name":"samehop","private":true,"scripts":{"test":"cargo test --locked && pnpm run build","build":"pnpm --dir ../../native run build"}}\n',
      "packages/nativetest/package.json": '{"name":"nativetest","private":true,"scripts":{"test":"cargo test --locked && pnpm --dir ../../native run test"}}\n',
      "packages/filterexec/package.json": '{"name":"filterexec","private":true,"scripts":{"test":"cargo test --locked && pnpm --filter native exec cargo build"}}\n',
      "packages/yarncwd/package.json": '{"name":"yarncwd","private":true,"scripts":{"test":"cargo test --locked && yarn --cwd ../../native build"}}\n',
      // Limits are refusals, never a silent stop: ten nested scripts, and a script longer than the read limit.
      "packages/deepchain/package.json": JSON.stringify({ name: "deepchain", private: true, scripts: Object.fromEntries([["test", "pnpm run s1"], ...Array.from({ length: 10 }, (_, k) => [`s${k + 1}`, k === 9 ? "cargo test" : `pnpm run s${k + 2}`])]) }) + "\n",
      "packages/huge/package.json": JSON.stringify({ name: "huge", private: true, scripts: { test: `tsc ${"x".repeat(17000)} && cargo test` } }) + "\n",
      // Root review of 15300d82 (BLOCK3): each of these bound the enclosing workspace alone.
      // cd moves the build into native; read without the cd, `pnpm run build` looked like this package's own cargo build.
      "packages/cdhop/package.json": '{"name":"cdhop","private":true,"scripts":{"test":"cd ../../native && pnpm run build","build":"cargo build"}}\n',
      "packages/runfilter/package.json": '{"name":"runfilter","private":true,"scripts":{"test":"cargo test && pnpm run --filter native build"}}\n',
      "packages/trailingdir/package.json": '{"name":"trailingdir","private":true,"scripts":{"test":"cargo test && pnpm build --dir ../../native","build":"tsc"}}\n',
      "packages/envsecond/package.json": '{"name":"envsecond","private":true,"scripts":{"test":"pnpm run a && pnpm run b","a":"cargo test","b":"CARGO_TARGET_DIR=target pnpm run a"}}\n',
      "packages/yarnws/package.json": '{"name":"yarnws","private":true,"scripts":{"test":"cargo test && yarn workspace native run build"}}\n',
      "packages/bunrun/package.json": '{"name":"bunrun","private":true,"scripts":{"test":"cargo test && bun run --cwd ../../native build"}}\n',
      "packages/cargoc/package.json": '{"name":"cargoc","private":true,"scripts":{"test":"cargo -C ../../native build"}}\n',
      "packages/cargoconfig/package.json": '{"name":"cargoconfig","private":true,"scripts":{"test":"cargo --config build.target-dir=\\"x\\" test"}}\n',
      "packages/nodeside/package.json": '{"name":"nodeside","private":true,"scripts":{"test":"cargo test && node v.mjs"}}\n',
      "packages/rs/package.json": '{"name":"rs","private":true,"scripts":{"build":"cargo build --locked","test":"cargo test --locked"}}\n',
      "native/package.json": '{"name":"native","private":true,"scripts":{"build":"cargo build --locked","test":"cargo test --locked"}}\n',
      "native/Cargo.toml": '[workspace]\nmembers = []\n', "Cargo.toml": '[workspace]\nmembers = ["packages/rs"]\n', "packages/rs/Cargo.toml": '[package]\nname = "rs"\nversion = "0.1.0"\n',
    });
    const units = ["mix", "twohop", "same", "deep", "rs", "samehop", "nativetest", "filterexec", "yarncwd", "deepchain", "huge", "cdhop", "runfilter", "trailingdir", "envsecond", "yarnws", "bunrun", "cargoc", "cargoconfig", "nodeside"];
    const files = ["package.json", "pnpm-workspace.yaml", ...units.map((n) => `packages/${n}/package.json`), "native/package.json", "native/Cargo.toml", "Cargo.toml", "packages/rs/Cargo.toml"];
    const npmUnits = units.map((n) => ({ name: n, dir: `packages/${n}` })).concat([{ name: "native", dir: "native" }]);
    const p = planScans({ changed: files, contentRoot: dir, liveRoot: dir, trustedRoot: dir, includeTests: true, quietTests: "local", tier: "compile", cargoCommand: ["cargo"],
      rustUnits: [{ name: "rs", dir: "packages/rs", crate: "rs", cargoDir: "." }], npmUnits, npmRoots: ["."], liveExecution: true,
      targetDirs: { ".": "/checkout/target", native: "/checkout/native/target" }, platform: process.platform });
    const row = (n) => p.scans.find((s) => s.id === `test:npm:${n}`);
    const refusedFor = (n) => p.scans.find((s) => s.internal === "refuse" && new RegExp(`npm unit ${n}:`).test(s.detail))?.detail ?? null;
    for (const n of ["mix", "twohop", "deep", "samehop", "nativetest", "filterexec", "yarncwd"]) {
      assert.equal(row(n), undefined, `${n} is not planned with one workspace's target directory`);
      assert.match(refusedFor(n) ?? "", /more than one workspace \(\., native|more than one workspace \(native, \./, `${n}: ${refusedFor(n)}`);
    }
    assert.equal(row("deepchain"), undefined); assert.match(refusedFor("deepchain") ?? "", /deeper than 8 scripts/);
    assert.equal(row("huge"), undefined); assert.match(refusedFor("huge") ?? "", /longer than 16384 characters/);
    for (const [n, why] of [
      ["cdhop", /runs `cd \.\.\/\.\.\/native` \(cd\)/],
      ["runfilter", /mixed|more than one workspace \(\., native|more than one workspace \(native, \./],
      ["trailingdir", /passes arguments to packages\/trailingdir's build/],
      ["envsecond", /with its own environment/],
      ["yarnws", /yarn command \(workspace\)/],
      ["bunrun", /\(bun\), which this check cannot follow/],
      ["cargoc", /the path to cargo \(test\) redirects it/],
      ["cargoconfig", /the path to cargo \(test\) redirects it/],
      ["nodeside", /\(node\), which this check cannot follow/],
    ]) { assert.equal(row(n), undefined, `${n} is not planned`); assert.match(refusedFor(n) ?? "", why, `${n}: ${refusedFor(n)}`); }
    // A requiresBroker declaration says the test reaches cargo in a way the scripts do not show: never bound, whatever they show.
    assert.match(cargoReachOf({ unitDir: "packages/rs", scripts: { test: "cargo test" }, declared: { call: { reason: "x" } } }).unprovable, /requiresBroker/);
    assert.deepEqual(cargoReachOf({ unitDir: "packages/rs", scripts: { test: "cargo test && pnpm -s run lint", lint: "eslint ." } }), { dir: "packages/rs" }, "allowed forms still bind");
    assert.equal(row("native")?.env.CARGO_TARGET_DIR, "/checkout/native/target", "a single reach into native is unchanged");
    assert.equal(row("same")?.env.CARGO_TARGET_DIR, "/checkout/target", `every reach in the enclosing workspace binds it (${refusedFor("same")})`);
    assert.equal(row("rs")?.env.CARGO_TARGET_DIR, "/checkout/target", "a single direct reach is unchanged");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

