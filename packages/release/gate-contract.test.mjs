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
import { CLASS_A_SCAN_PREFIXES, CLASS_B_SCAN_PREFIXES, TEST_SCAN_CLASS, LIVE_SCAN_IDS, REGISTRY_SCAN_IDS, planScans } from "./deterministic-scans.mjs";
import { EXECUTOR_KINDS, resolveExecutor } from "./gate-executor.mjs";
import { STAGES } from "./gate-stages.mjs";
import { RULES as APP_RULES, COMPILE_TIER_IDS } from "./app-check.mjs";
import { RULES as PREFLIGHT_RULES } from "../../scripts/preflight.mjs";
import { main } from "./bin/rightkit-gate.mjs";
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
  E6: { covered: [["snapshot.test.mjs", "E6: a changed byte, a deleted file, a planted visible file refuse verification; the next sync repairs them"], ["snapshot-check.test.mjs", "E17: a snapshot changed while class A runs refuses with RK_CHECK_ERROR, exit 2"]], pending: [["step 4", "checkout edits during a stage and a second request against held build ownership"], ["step 9", "stale or altered output, helper, native output, final artifact, app SHA, run or attempt reject"]] },
  E7: { covered: [], pending: [["step 9", "candidate-to-final evidence binding (transformations, final digest, unchanged promotion)"]] },
  E8: { covered: [], pending: [["step 11", "canary legs per origin with required app SHAs"]] },
  E9: { covered: [], pending: [["step 9", "durable retry/restart reconciliation at dispatch and publication boundaries"]] },
  E10: { covered: [["gate-contract.test.mjs", "[behavior] E10: the executor has no caller input, so a human and a managed agent route the same way"]], pending: [["step 11", "installed entry on Mac/Dell for both callers; a personal terminal outside the entry is not intercepted"]] },
  E11: { covered: [], pending: [["step 11", "pinned qualification policy, native installers and targets on their native hosts"]] },
  E12: { covered: [], pending: [["pulse#492 (Pulse owner)", "headless Linux guest join, messaging, reconnect, revoked/expired denial"]] },
  E13: { covered: [], pending: [["step 7", "workspace Clippy route on Mac, Dell (broker) and Linux (direct)"]] },
  E14: { covered: [], pending: [["step 7", "batched Clippy equivalence before the per-crate path retires"]] },
  E15: { covered: [], pending: [["step 7", "failure collection with attribution across independent failures"]] },
  E16: { covered: [], pending: [["step 4", "Linux output directory pinned to the checkout's resolved target_directory"]] },
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
  for (const [id, cell] of Object.entries(CELLS)) if (cell.host === "linux") assert.deepEqual(route("linux"), { kind: "direct", cargo: ["cargo"] }, id);
});
test("[behavior] executor: a Linux host always runs direct, even with RustRight on PATH, a broker socket variable or a managed cargo shim (contract §1 'One entry')", () => {
  assert.deepEqual(route("linux", { env: { __rustright: "/usr/local/bin/rustright" } }), { kind: "direct", cargo: ["cargo"] });
  assert.deepEqual(route("linux", { env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/run/rustright.sock" } }), { kind: "direct", cargo: ["cargo"] });
  const shim = resolveExecutor({ env: { PATH: "/home/a/.rightkit-managed/agent-bin:/usr/bin", RIGHTKIT_DATA_ROOT: "/home/a/.rightkit-managed" }, platform: "linux", exists: () => true, readFile: () => "cargo-guard.mjs", which: (c) => (c === "cargo" ? ["/home/a/.rightkit-managed/agent-bin/cargo"] : []) });
  assert.deepEqual(shim, { kind: "direct", cargo: ["cargo"] });
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
