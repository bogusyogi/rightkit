// The gate-owned cargo ledger (cargo-ledger.mjs): what the runtime cargo wrapper records and how the scan runner reads it back.
// Data tests call the real functions on real files. The runner tests run the real planner and the real scan runner over an in-memory repository;
// the runner's own `spawn` seam stands in for the wrapper in one way only: it files each cargo call under the token the call's environment carries,
// with the real appendLedger, exactly as cargo-target-shim.mjs runShim does (`appendLedger(map.ledgerDir, env[RIGHTKIT_CARGO_SCAN], entry)`).
// Whether the real wrapper receives that environment from a real cargo-deny scan is proved only by a gate run on a host.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { LEDGER_SCHEMA, OK_STATUSES, SCAN_TOKEN_ENV, UNSCOPED, appendLedger, readLedger, scanTokenOf, unclaimedLedgers } from "./cargo-ledger.mjs";
import { planScans, runScans, scanFailures } from "./deterministic-scans.mjs";

const scratch = (t) => { const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-ledger-"))); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; };
const call = (id, status, over = {}) => ({ id, mode: "direct", cwd: "/c/repo", argv: ["metadata", "--locked"], status, ...over });
const file = (dir, token) => path.join(dir, `${token}.jsonl`);

test("data: scanTokenOf is stable per scan id, sixteen hex characters, never the id itself", () => {
  assert.equal(scanTokenOf("cargo-deny"), scanTokenOf("cargo-deny"));
  assert.match(scanTokenOf("cargo-deny"), /^[0-9a-f]{16}$/);
  assert.notEqual(scanTokenOf("cargo-deny"), scanTokenOf("cargo-audit"));
  assert.equal(scanTokenOf("cargo-deny"), crypto.createHash("sha256").update("cargo-deny").digest("hex").slice(0, 16));
  assert.equal(SCAN_TOKEN_ENV, "RIGHTKIT_CARGO_SCAN");
  assert.deepEqual([...OK_STATUSES], ["ran", "verified"]);
  assert.equal(Object.isFrozen(OK_STATUSES), true);
});

test("data: appendLedger needs an absolute directory, creates it private, and files a record under a valid token or under 'unscoped', never under a path of the caller's making", (t) => {
  const root = scratch(t);
  for (const bad of [undefined, "", "relative/ledger"]) assert.throws(() => appendLedger(bad, scanTokenOf("a"), call("1", "pending")), /the cargo ledger directory is not set/, String(bad));
  const dir = path.join(root, "nested", "ledger");
  const token = scanTokenOf("cargo-deny");
  appendLedger(dir, token, call("1", "pending"));
  appendLedger(dir, token, call("1", "ran", { exit: 0 }));
  const lines = fs.readFileSync(file(dir, token), "utf8").trim().split("\n").map((l) => JSON.parse(l));
  assert.equal(lines.length, 2);
  assert.ok(lines.every((l) => l.schema === LEDGER_SCHEMA && !Number.isNaN(Date.parse(l.at))));
  assert.deepEqual(lines.map((l) => l.status), ["pending", "ran"]);
  if (process.platform !== "win32") {
    assert.equal(fs.statSync(dir).mode & 0o777, 0o700);
    assert.equal(fs.statSync(file(dir, token)).mode & 0o777, 0o600);
  }
  // A token that is not exactly sixteen lowercase hex characters (cleared, invented, or a path) files under "unscoped".
  for (const bad of [undefined, null, "", "short", "G".repeat(16), "A".repeat(16), "0".repeat(17), "../../escape", `${"a".repeat(15)}/`]) appendLedger(dir, bad, call("x", "ran"));
  assert.deepEqual(fs.readdirSync(dir).sort(), [`${token}.jsonl`, `${UNSCOPED}.jsonl`].sort(), "nothing was written outside the ledger directory under a caller's token");
  assert.equal(fs.readFileSync(file(dir, UNSCOPED), "utf8").trim().split("\n").length, 9);
  assert.equal(fs.existsSync(path.join(root, "escape.jsonl")), false);
});

test("data: appendLedger refuses to follow a link at the ledger file", { skip: process.platform === "win32" }, (t) => {
  const dir = scratch(t); const target = path.join(dir, "elsewhere.txt");
  fs.writeFileSync(target, "kept\n");
  fs.symlinkSync(target, file(dir, scanTokenOf("a")));
  assert.throws(() => appendLedger(dir, scanTokenOf("a"), call("1", "ran")));
  assert.equal(fs.readFileSync(target, "utf8"), "kept\n");
});

test("data: readLedger keeps the latest record of each call and reports every call that is not a completed success", (t) => {
  const dir = scratch(t); const token = scanTokenOf("cargo-deny");
  assert.deepEqual(readLedger(dir, token), { entries: [], problems: [] }, "no file: no calls");
  appendLedger(dir, token, call("a", "pending")); appendLedger(dir, token, call("a", "ran", { exit: 0 }));
  appendLedger(dir, token, call("b", "pending")); appendLedger(dir, token, call("b", "verified"));
  let read = readLedger(dir, token);
  assert.deepEqual(read.entries.map((e) => [e.id, e.status]), [["a", "ran"], ["b", "verified"]], "a later record replaces the earlier one of the same call");
  assert.deepEqual(read.problems, []);
  appendLedger(dir, token, call("c", "pending", { argv: ["fetch", "--locked"], cwd: "/c/repo/ws" }));
  appendLedger(dir, token, call("d", "refused", { adapter: "rightkit-swift", argv: ["build"], code: "RK_X", message: "not admitted" }));
  appendLedger(dir, token, call("e", "unverified", { argv: ["deny", "check"] }));
  appendLedger(dir, token, call("f", "refused", { argv: ["audit"] }));
  read = readLedger(dir, token);
  assert.equal(read.entries.length, 6);
  assert.deepEqual(read.problems, [
    "cargo fetch --locked (from /c/repo/ws) never completed: the wrapper recorded it and no outcome, so it is unverified",
    "rightkit-swift build (from /c/repo) was refused (RK_X): not admitted",
    "cargo deny check (from /c/repo) was unverified",
    "cargo audit (from /c/repo) was refused",
  ]);
});

test("data: readLedger names an unreadable or malformed record and still reads the rest; any other read error is thrown", (t) => {
  const dir = scratch(t); const token = scanTokenOf("a");
  fs.writeFileSync(file(dir, token), [
    JSON.stringify({ schema: LEDGER_SCHEMA, id: "ok", status: "ran", argv: ["metadata"], cwd: "/c" }), "", "   ", "{ not json",
    JSON.stringify({ schema: "other/1", id: "x", status: "ran" }), JSON.stringify({ schema: LEDGER_SCHEMA, status: "ran" }), JSON.stringify({ schema: LEDGER_SCHEMA, id: 7, status: "ran" }), "null",
  ].join("\n"));
  const read = readLedger(dir, token);
  assert.deepEqual(read.entries.map((e) => e.id), ["ok"]);
  assert.deepEqual(read.problems, [`an unreadable ledger record in ${token}.jsonl`, `a malformed ledger record in ${token}.jsonl`, `a malformed ledger record in ${token}.jsonl`, `a malformed ledger record in ${token}.jsonl`, `a malformed ledger record in ${token}.jsonl`]);
  // An invalid token reads the unscoped file, as appendLedger filed it.
  appendLedger(dir, "not-a-token", call("u", "ran"));
  assert.deepEqual(readLedger(dir, undefined).entries.map((e) => e.id), ["u"]);
  fs.mkdirSync(file(dir, scanTokenOf("dir")));
  assert.throws(() => readLedger(dir, scanTokenOf("dir")), /EISDIR|EPERM|EACCES/, "a read that fails for another reason than a missing file is not read as no calls");
});

test("data: unclaimedLedgers lists the ledger files no scan claimed, and nothing else", (t) => {
  const dir = scratch(t);
  assert.deepEqual(unclaimedLedgers(path.join(dir, "absent"), new Set()), [], "no directory, no ledgers");
  const [a, b] = [scanTokenOf("a"), scanTokenOf("b")];
  appendLedger(dir, a, call("1", "ran")); appendLedger(dir, b, call("2", "ran")); appendLedger(dir, undefined, call("3", "ran"));
  fs.writeFileSync(path.join(dir, "notes.txt"), "not a ledger\n");
  assert.deepEqual(unclaimedLedgers(dir, new Set([a, b])), [UNSCOPED]);
  assert.deepEqual(unclaimedLedgers(dir, new Set([a])).sort(), [b, UNSCOPED].sort());
  assert.deepEqual(unclaimedLedgers(dir, new Set([a, b, UNSCOPED])), []);
  const file1 = path.join(dir, "plain-file");
  fs.writeFileSync(file1, "x");
  assert.throws(() => unclaimedLedgers(file1, new Set()), /ENOTDIR/, "a ledger directory that is not a directory is an error, not an empty ledger");
});

// ---- G3: cargo-deny's own cargo calls carry the deny scan's ledger token ---------------------------------------------------------------
// Evidence of the defect on HEAD: gate-compile.log:125 — every Rust repository's run failed on "cargo calls were filed under 1 scan token(s) no scan
// owns (unscoped)": cargo-deny's prefetch (`cargo fetch --locked`) and its full `cargo metadata` were spawned without the scan's token.

const ROOT = path.join(path.sep, "c", "repo");
const norm = (p) => p.replaceAll("\\", "/");
const memoryFs = (files) => {
  const all = new Map(Object.entries(files).map(([k, v]) => [norm(path.join(ROOT, k)), v]));
  return { exists: (p) => all.has(norm(p)) || [...all.keys()].some((k) => k.startsWith(`${norm(p)}/`)), readFile: (p) => { const v = all.get(norm(p)); if (v === undefined) throw new Error(`ENOENT ${p}`); return v; }, realpath: (p) => p };
};
const FILES = {
  "crates/foo/Cargo.toml": 'edition = "2024"\nversion = "1.2.3"\nlicense = "MIT"\n', "crates/foo/CHANGELOG.md": "## 1.2.3\n- x\n", "crates/foo/src/lib.rs": "pub fn f() {}\n", "Cargo.lock": "version = 4\n",
  "ci/advisory-exceptions.json": JSON.stringify({ schema: 1, exceptions: [] }), "deny.toml": '[advisories]\nunmaintained = "workspace"\nyanked = "deny"\nignore = []\n', ".cargo/audit.toml": "[advisories]\nignore = []\n",
};
const digest = (tool) => crypto.createHash("sha256").update(`${tool} enrolled bytes`).digest("hex");
const POLICY = Object.freeze(Object.fromEntries(["cargo-deny", "cargo-audit"].map((tool) => [tool, { path: `/opt/rk-tools/${tool}/${tool}`, sha256: digest(tool), version: "1.0.0" }])));
const TOOL_OPS = { realpath: (p) => p, stat: () => ({ isFile: () => true, mode: 0o755 }), digestFile: (p) => digest(path.posix.basename(p)) };
const PROBE_TOKEN = scanTokenOf("tool-probe");

/**
 * The runner's spawn seam, playing the runtime wrapper's one observable act: every call of the executable named `cargo` is filed in the ledger
 * under the token its environment carries (the real appendLedger; a call with no token is filed "unscoped"), pending then ran, as runShim does.
 * `tokenOf(args, env)` lets a test model code that clears or replaces the token for a call.
 */
function wrapperSpawn(ledgerDir, { tokenOf = (_args, env) => env[SCAN_TOKEN_ENV], status = () => "ran" } = {}) {
  const calls = []; let n = 0;
  const spawn = (command, args, options = {}) => {
    const name = path.basename(command).replace(/\.exe$/, "");
    const isProbe = args.at(-1) === "--version";
    if (isProbe) {
      if (name === "cargo") { const token = tokenOf(args, options.env ?? {}); appendLedger(ledgerDir, token, call(`p${n += 1}`, "ran", { argv: args })); }
      return { status: 0, stdout: `${name} 1.0.0\n`, stderr: "" };
    }
    if (name === "cargo") {
      n += 1; const id = `c${n}`;
      const token = tokenOf(args, options.env ?? {});
      appendLedger(ledgerDir, token, call(id, "pending", { argv: args, cwd: options.cwd }));
      appendLedger(ledgerDir, token, call(id, status(args), { argv: args, cwd: options.cwd, ...(status(args) === "refused" ? { code: "RK_REFUSED", message: "not an admitted shape" } : {}) }));
      calls.push({ sub: args.find((a) => !a.startsWith("-")), token: options.env?.[SCAN_TOKEN_ENV] });
    }
    return { status: 0, stdout: args.includes("metadata") ? "{}" : "", stderr: "" };
  };
  return { spawn, calls };
}

function cargoRun(t, spawnOver = {}) {
  const ledgerDir = path.join(scratch(t), "ledger");
  const fsx = memoryFs(FILES);
  const plan = planScans({ changed: ["crates/foo/src/lib.rs", "crates/foo/Cargo.toml"], contentRoot: ROOT, liveRoot: ROOT, ...fsx, platform: "win32" });
  const w = wrapperSpawn(ledgerDir, spawnOver);
  const run = () => runScans(plan, {
    ...fsx, classes: ["A"], exclude: (s) => !/^cargo-(metadata|deny|audit|package-list)/.test(s.id), env: {}, execPath: "node", toolPolicy: POLICY, toolOps: TOOL_OPS, spawn: w.spawn,
    cargoLedger: { dir: ledgerDir }, ensureAdvisoryDb: () => "/cache/advisory-db/pinned", prepareDenyOverlay: () => ({ configPath: "/tmp/rk-deny/deny.toml", cleanup() {} }),
  });
  return { ledgerDir, plan, w, run };
}

test("G3: a cargo-deny scan files its prefetch, its full metadata and itself under its own token; the run ends with no unscoped record and no ledger row", async (t) => {
  const r = cargoRun(t);
  assert.deepEqual(r.plan.scans.filter((s) => /^cargo-/.test(s.id)).map((s) => s.id), ["cargo-metadata", "cargo-package-list:foo", "cargo-deny", "cargo-audit"]);
  const result = await r.run();
  assert.equal(result.ok, true, JSON.stringify(scanFailures(result)));
  assert.deepEqual(result.scans.map((s) => [s.id, s.status]), [["cargo-metadata", "pass"], ["cargo-package-list:foo", "pass"], ["cargo-deny", "pass"], ["cargo-audit", "pass"]]);
  assert.equal(result.scans.some((s) => s.id.startsWith("cargo-ledger")), false, "no row of the ledger's own");
  assert.equal(fs.existsSync(file(r.ledgerDir, UNSCOPED)), false, "no call was filed unscoped");
  assert.deepEqual(unclaimedLedgers(r.ledgerDir, new Set([PROBE_TOKEN, ...["cargo-metadata", "cargo-package-list:foo", "cargo-deny", "cargo-audit"].map(scanTokenOf)])), []);
  // The deny scan's token holds all three of its calls, in the order they ran; every other cargo scan holds its own one call.
  const subsOf = (id) => readLedger(r.ledgerDir, scanTokenOf(id)).entries.map((e) => e.argv.find((a) => !a.startsWith("-")));
  assert.deepEqual(subsOf("cargo-deny"), ["fetch", "metadata", "deny"]);
  assert.deepEqual(subsOf("cargo-metadata"), ["metadata"]);
  assert.deepEqual(subsOf("cargo-package-list:foo"), ["package"]);
  assert.deepEqual(subsOf("cargo-audit"), ["audit"]);
  // Every call the seam saw carried the token of the scan that made it (the deny scan's three share one).
  const tokens = r.w.calls.map((c) => [c.sub, c.token]);
  assert.deepEqual(tokens, [["metadata", scanTokenOf("cargo-metadata")], ["package", scanTokenOf("cargo-package-list:foo")], ["fetch", scanTokenOf("cargo-deny")], ["metadata", scanTokenOf("cargo-deny")], ["deny", scanTokenOf("cargo-deny")], ["audit", scanTokenOf("cargo-audit")]]);
  assert.ok(fs.existsSync(file(r.ledgerDir, PROBE_TOKEN)), "the gate's own version probes are filed under the probe token it claims");
});

test("G3 (control): a cargo call that reaches the wrapper with no scan token still fails the run as an unscoped record, naming the call", async (t) => {
  // The defect, modelled: the deny scan's prefetch carries no token (what HEAD spawned).
  const r = cargoRun(t, { tokenOf: (args, env) => (args.includes("fetch") ? undefined : env[SCAN_TOKEN_ENV]) });
  const result = await r.run();
  assert.equal(result.ok, false);
  const row = result.scans.find((s) => s.id === "cargo-ledger");
  assert.ok(row, "the ledger's own row fails the run");
  assert.equal(row.status, "fail"); assert.equal(row.code, "RK_CARGO_LEDGER");
  assert.match(row.detail, /filed under 1 scan token\(s\) no scan owns \(unscoped\)/);
  assert.match(row.detail, /cargo fetch --locked \(ran, from /);
  assert.equal(fs.existsSync(file(r.ledgerDir, UNSCOPED)), true);
});

test("G3: a call the deny scan's prefetch or metadata made that the wrapper refused fails the cargo-deny scan itself, naming the call", async (t) => {
  const r = cargoRun(t, { status: (args) => (args.includes("fetch") ? "refused" : "ran") });
  const result = await r.run();
  assert.equal(result.ok, false);
  assert.equal(result.scans.some((s) => s.id === "cargo-ledger"), false, "attributed to its scan, not an anonymous row");
  const deny = result.scans.find((s) => s.id === "cargo-deny");
  assert.equal(deny.status, "fail"); assert.equal(deny.code, "RK_CARGO_LEDGER");
  assert.match(deny.detail, /cargo-deny: the gate's cargo ledger records 1 cargo call\(s\) that did not complete as a verified success: cargo fetch --locked .* was refused \(RK_REFUSED\): not an admitted shape/);
  for (const id of ["cargo-metadata", "cargo-package-list:foo", "cargo-audit"]) assert.equal(result.scans.find((s) => s.id === id).status, "pass", id);
});
