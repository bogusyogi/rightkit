// rightkit#1301: the stage grant (rustright.snapshot-grant/1) the gate writes for the third-party children of a stage on a host with no PATH
// wrapper (the Dell): the Tauri CLI's own Cargo calls run under it. The broker is a FIXTURE: test-support/fake-broker.mjs models the rules
// the real broker enforces on a grant call (file location and bytes, exact body fields, env, command allowlist, snapshot confinement,
// expiry, maxCalls over distinct nonces, one call at a time). No real broker, no Cargo.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { spawn } from "./background-process.mjs";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { carriedEnv } from "./bin/rightkit-gate.mjs";
import { installCarrier } from "./carried-cargo.mjs";
import { spawnSync } from "./background-process.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { GRANT_FIELDS, GRANT_MAX_CALLS_LIMIT, SNAPSHOT_GRANT_ENV, SNAPSHOT_GRANT_SCHEMA, STAGE_GRANT_MAX_CALLS, STAGE_GRANT_TTL_MS, writeStageGrant } from "./snapshot-request.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client is a POSIX script" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** A fixture broker host with a Windows-style carrier (map only, no wrapper) and a stage grant. */
function host(t, grant = {}) {
  const fx = makeFakeBroker(t);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: "win32", env: {}, grant: { primary: fx.checkout, callingGate: GATE, ...grant } });
  t.after(() => installed.cleanup());
  const childEnv = (over = {}) => {
    const env = { PATH: process.env.PATH, HOME: process.env.HOME, ...installed.env, ...over };
    for (const [k, v] of Object.entries(over)) if (v === undefined) delete env[k];
    return env;
  };
  const inA = path.join(fx.snapshot, "crates", "a");
  /** One call of the third-party child: the fixture client, with the grant env and no carrier of its own. */
  const call = (args, { cwd = inA, over = {} } = {}) => spawnSync(fx.client, ["cargo", ...args], { cwd, env: childEnv(over), encoding: "utf8" });
  return { fx, installed, childEnv, call, inA, descriptor: () => JSON.parse(installed.env[SNAPSHOT_GRANT_ENV]) };
}

test("the grant file: a direct child of snapshot-requests, named grant-<uuid v4>.json, a regular owner-only file well under 4 MiB, hashing to the descriptor", { skip: POSIX }, (t) => {
  const h = host(t);
  const { path: file, sha256 } = h.descriptor();
  assert.equal(path.dirname(file), path.join(h.fx.controlRoot, "snapshot-requests"));
  const id = /^grant-(.+)\.json$/.exec(path.basename(file))?.[1];
  assert.match(id, UUID_V4);
  const meta = fs.lstatSync(file);
  assert.ok(meta.isFile() && !meta.isSymbolicLink() && meta.size < 4 * 1024 * 1024);
  assert.equal(meta.mode & 0o077, 0, "owner-only");
  assert.equal(sha256, crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex"));
  assert.ok(Buffer.byteLength(h.installed.env[SNAPSHOT_GRANT_ENV]) <= 4096, "the descriptor is at most 4096 bytes");
  assert.equal(h.installed.grant.body.grantId, id);
});

test("the grant body has exactly the schema's fields: schema, grantId, primary, commit, snapshot, declaredInputs, ownership, allowedWorkspaces, maxCalls, expiresAt", { skip: POSIX }, (t) => {
  const h = host(t);
  const body = JSON.parse(fs.readFileSync(h.descriptor().path, "utf8"));
  assert.deepEqual(Object.keys(body), [...GRANT_FIELDS]);
  assert.equal(body.schema, SNAPSHOT_GRANT_SCHEMA); assert.equal(SNAPSHOT_GRANT_SCHEMA, "rustright.snapshot-grant/1");
  assert.match(body.grantId, UUID_V4);
  assert.equal(body.primary, h.fx.checkout, "the absolute primary-checkout root (tauri runs inside the snapshot)");
  assert.deepEqual(body.commit, { sha: h.fx.sb.commit.sha, tree: h.fx.sb.commit.tree });
  assert.deepEqual(body.snapshot, { path: h.fx.snapshot, expectedTree: body.commit.tree });
  assert.deepEqual(body.declaredInputs, { inputs: [] });
  assert.deepEqual(body.ownership, { handle: h.fx.sb.handle });
  assert.deepEqual(body.allowedWorkspaces, [h.fx.snapshot, path.join(h.fx.snapshot, "native")], "the commit's workspaces inside the snapshot");
  assert.ok(body.allowedWorkspaces.every((w) => w === h.fx.snapshot || w.startsWith(`${h.fx.snapshot}${path.sep}`)));
  assert.equal(body.maxCalls, STAGE_GRANT_MAX_CALLS); assert.equal(STAGE_GRANT_MAX_CALLS, 64);
  assert.match(body.expiresAt, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z$/);
  const ahead = Date.parse(body.expiresAt) - Date.now();
  assert.ok(ahead > STAGE_GRANT_TTL_MS - 60_000 && ahead <= STAGE_GRANT_TTL_MS, "the stage deadline");
  assert.deepEqual(Object.keys(body.commit), ["sha", "tree"]); assert.deepEqual(Object.keys(body.snapshot), ["path", "expectedTree"]);
});

test("the writer refuses what the broker would: empty or outside workspaces, maxCalls out of 1..4096, a relative primary, a bad snapshot tree, no gate, no handle, a link in place of the directory", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const ok = { controlRoot: fx.controlRoot, primary: fx.checkout, commit: fx.sb.commit, snapshot: fx.sb.snapshot, handle: fx.sb.handle, allowedWorkspaces: [fx.snapshot], callingGate: GATE };
  const bad = (over, why) => assert.throws(() => writeStageGrant({ ...ok, ...over }), (e) => e.code === "invalid" || e.code === "too-large", why);
  bad({ allowedWorkspaces: [] }, "empty workspaces"); bad({ allowedWorkspaces: [fx.checkout] }, "a workspace outside the snapshot"); bad({ allowedWorkspaces: ["rel"] }, "relative workspace");
  bad({ maxCalls: 0 }, "maxCalls 0"); bad({ maxCalls: GRANT_MAX_CALLS_LIMIT + 1 }, "maxCalls 4097"); bad({ maxCalls: 1.5 }, "fractional maxCalls");
  bad({ primary: "rel" }, "relative primary"); bad({ snapshot: { path: fx.snapshot, expectedTree: "f".repeat(40) } }, "expectedTree is not the commit's tree");
  bad({ callingGate: "" }, "no calling gate"); bad({ handle: "" }, "no handle"); bad({ uuid: () => "not-a-uuid" }, "not a uuid v4");
  bad({ declaredInputs: ["/abs"] }, "absolute declared input");
  assert.equal(writeStageGrant({ ...ok, maxCalls: GRANT_MAX_CALLS_LIMIT }).body.maxCalls, 4096);
  const elsewhere = path.join(fx.root, "elsewhere"); fs.mkdirSync(elsewhere);
  const dir = path.join(fx.controlRoot, "snapshot-requests"); fs.rmSync(dir, { recursive: true, force: true }); fs.symlinkSync(elsewhere, dir);
  bad({}, "snapshot-requests is a link"); assert.deepEqual(fs.readdirSync(elsewhere), [], "nothing was written through the link");
});

test("the child's environment: RUSTRIGHT_SNAPSHOT_GRANT and RUSTRIGHT_CALLING_GATE only; no SNAPSHOT_REQUEST, no ATTEST_NONCE, no legacy spelling, even when the stage inherited them", { skip: POSIX }, (t) => {
  const h = host(t);
  assert.deepEqual(Object.keys(h.installed.env).filter((k) => /RUSTRIGHT|RIGHTKIT/.test(k)).sort(), ["RIGHTKIT_CARRIED_CARGO_MAP", "RUSTRIGHT_CALLING_GATE", "RUSTRIGHT_SNAPSHOT_GRANT"]);
  assert.equal(h.installed.env.RUSTRIGHT_CALLING_GATE, GATE);
  const carrier = { carried: true, grant: true, env: h.installed.env };
  const stale = { RUSTRIGHT_SNAPSHOT_REQUEST: "x", RIGHTKIT_SNAPSHOT_REQUEST: "x", RUSTRIGHT_ATTEST_NONCE: "n", RIGHTKIT_ATTEST_NONCE: "n", RUSTRIGHT_REQUEST_ID_FILE: "/f", RIGHTKIT_SNAPSHOT_GRANT: "old", RUSTRIGHT_SNAPSHOT_GRANT: "old", RIGHTKIT_CALLING_GATE: "legacy", CARGO_TARGET_DIR: "/t", KEEP: "1" };
  const env = carriedEnv(carrier, { PATH: "/bin", ...stale }, { Cargo_Build_Target_Dir: "/u" });
  assert.deepEqual(Object.keys(env).filter((k) => /RUSTRIGHT|RIGHTKIT/.test(k)).sort(), ["RIGHTKIT_CARRIED_CARGO_MAP", "RUSTRIGHT_CALLING_GATE", "RUSTRIGHT_SNAPSHOT_GRANT"]);
  assert.equal(env.RUSTRIGHT_SNAPSHOT_GRANT, h.installed.env.RUSTRIGHT_SNAPSHOT_GRANT, "the stage's own grant, not a stale one");
  assert.equal(env.KEEP, "1"); assert.equal(env.CARGO_TARGET_DIR, undefined);
  // A stage without a grant (the Mac's wrapper) strips none of a child's per-call variables here: the seam refuses an inherited one itself.
  assert.equal(carriedEnv({ carried: true, env: {} }, { RUSTRIGHT_ATTEST_NONCE: "n" }).RUSTRIGHT_ATTEST_NONCE, "n");
});

test("calls under the grant: build and run with --locked or --frozen, metadata --no-deps and the read-only list are accepted, each with a fresh nonce, inside the snapshot's workspaces", { skip: POSIX }, (t) => {
  const h = host(t);
  for (const args of [["build", "--locked", "--bins"], ["run", "--frozen"], ["metadata", "--no-deps", "--format-version", "1"], ["--version"], ["build", "--locked", "--manifest-path", path.join(h.fx.snapshot, "native", "Cargo.toml")]]) {
    const r = h.call(args);
    assert.equal(r.status, 0, `cargo ${args.join(" ")}: ${r.stderr}`);
  }
  const calls = h.fx.grantCalls();
  assert.equal(calls.length, 5);
  assert.equal(new Set(calls.map((c) => c.nonce)).size, 5, "the client mints a nonce per call");
  assert.equal(new Set(calls.map((c) => c.grantId)).size, 1);
  assert.ok(calls.every((c) => !c.envNames.includes("RUSTRIGHT_SNAPSHOT_REQUEST") && !c.envNames.includes("RUSTRIGHT_ATTEST_NONCE")));
  assert.deepEqual(h.fx.calls(), [], "no per-call request was made: the grant carried them");
});

test("refused under the grant: an unlocked build, --config and -Z, a cwd or path outside the snapshot, a tampered file, an expired grant, a wrong name, a link, both spellings, a request or nonce beside it, no gate", { skip: POSIX }, (t) => {
  const h = host(t);
  const refused = (r, why, re = /invalid snapshot grant/) => { assert.equal(r.status, 2, why); assert.match(r.stderr, re, why); };
  refused(h.call(["build"]), "an unlocked build", /not a grant-carried command/);
  refused(h.call(["build", "--locked", "--config", "build.target-dir='x'"]), "--config", /never allowed/);
  refused(h.call(["build", "--locked", "-Zunstable-options"]), "-Z", /never allowed/);
  refused(h.call(["install", "x"]), "install", /not a grant-carried/);
  refused(h.call(["build", "--locked"], { cwd: h.fx.checkout }), "a cwd in the primary checkout", /outside every allowed workspace/);
  refused(h.call(["build", "--locked", "--target-dir", "/elsewhere"]), "a path outside the snapshot", /outside the snapshot/);
  refused(h.call(["build", "--locked", "--manifest-path", path.join(h.fx.checkout, "Cargo.toml")]), "a manifest in the checkout", /outside the snapshot/);
  refused(h.call(["build", "--locked"], { over: { RUSTRIGHT_SNAPSHOT_REQUEST: JSON.stringify({ path: "/x", sha256: "0".repeat(64) }) } }), "a request beside the grant", /sets no snapshot request/);
  refused(h.call(["build", "--locked"], { over: { RUSTRIGHT_ATTEST_NONCE: "a".repeat(64) } }), "a nonce set by the gate", /leaves the nonce unset/);
  refused(h.call(["build", "--locked"], { over: { RIGHTKIT_SNAPSHOT_GRANT: h.installed.env[SNAPSHOT_GRANT_ENV] } }), "the legacy spelling", /legacy spelling/);
  refused(h.call(["build", "--locked"], { over: { RUSTRIGHT_CALLING_GATE: undefined } }), "no calling gate", /no calling gate/);
  assert.deepEqual(h.fx.grantCalls(), [], "no refused call was counted");
  // The file itself: one changed byte breaks the sha256 match; a link, a wrong name and a copy outside the directory are refused.
  const { path: file } = h.descriptor();
  const original = fs.readFileSync(file);
  fs.writeFileSync(file, original.toString("utf8").replace('"maxCalls":64', '"maxCalls":65'));
  refused(h.call(["build", "--locked"]), "a tampered grant", /does not match its sha256/);
  fs.writeFileSync(file, original);
  assert.equal(h.call(["build", "--locked"]).status, 0, "the untouched file verifies again");
  const dir = path.dirname(file);
  const link = path.join(dir, `grant-${"1".repeat(8)}-1111-4111-8111-${"1".repeat(12)}.json`);
  fs.symlinkSync(file, link);
  refused(h.call(["build", "--locked"], { over: { RUSTRIGHT_SNAPSHOT_GRANT: JSON.stringify({ path: link, sha256: h.descriptor().sha256 }) } }), "a link", /not a regular file/);
  const renamed = path.join(dir, "grant-x.json"); fs.copyFileSync(file, renamed);
  refused(h.call(["build", "--locked"], { over: { RUSTRIGHT_SNAPSHOT_GRANT: JSON.stringify({ path: renamed, sha256: h.descriptor().sha256 }) } }), "a wrong name", /not named grant-<uuid v4>\.json/);
  const outside = path.join(h.fx.root, "grant-copy.json"); fs.copyFileSync(file, outside);
  refused(h.call(["build", "--locked"], { over: { RUSTRIGHT_SNAPSHOT_GRANT: JSON.stringify({ path: outside, sha256: h.descriptor().sha256 }) } }), "outside snapshot-requests", /directly in snapshot-requests/);
});

test("expiry: a grant past its deadline is refused; maxCalls counts distinct nonces and the call after the last is refused", { skip: POSIX }, (t) => {
  const expired = host(t, { ttlMs: -1000, now: Date.now() });
  const r = expired.call(["build", "--locked"]);
  assert.equal(r.status, 2); assert.match(r.stderr, /the grant has expired/);
  const few = host(t, { maxCalls: 2 });
  assert.equal(few.call(["build", "--locked"]).status, 0);
  assert.equal(few.call(["metadata", "--no-deps"]).status, 0);
  const third = few.call(["build", "--locked"]);
  assert.equal(third.status, 2); assert.match(third.stderr, /the grant's 2 calls are used/);
  assert.equal(few.fx.grantCalls().length, 2);
});

test("concurrency: a call while another of the same grant is running is refused; calls run one at a time", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior({ holdMs: 1500 });
  const run = () => new Promise((resolve) => {
    const child = spawn(h.fx.client, ["cargo", "build", "--locked"], { cwd: h.inA, env: h.childEnv() });
    let stderr = ""; child.stderr.on("data", (d) => { stderr += d; });
    child.on("close", (status) => resolve({ status, stderr }));
  });
  const first = run();
  // Wait until the first call is running (its busy marker exists), however long the client takes to start.
  const busyDir = path.join(h.fx.state, "grants");
  for (let i = 0; i < 100 && !(fs.existsSync(busyDir) && fs.readdirSync(busyDir).some((n) => n.endsWith(".busy"))); i += 1) await new Promise((resolve) => setTimeout(resolve, 50));
  const second = await run();
  assert.equal(second.status, 2); assert.match(second.stderr, /already running: calls run one at a time/);
  assert.equal((await first).status, 0);
  h.fx.setBehavior({});
  assert.equal(h.call(["build", "--locked"]).status, 0, "once the first call ends the next is accepted");
});

test("the grant file is removed with the stage, and the carrier's cleanup is idempotent", { skip: POSIX }, (t) => {
  const h = host(t);
  const file = h.installed.grant.file;
  assert.ok(fs.existsSync(file));
  h.installed.cleanup();
  assert.equal(fs.existsSync(file), false);
  assert.deepEqual(fs.readdirSync(path.join(h.fx.controlRoot, "snapshot-requests")), []);
  h.installed.cleanup();
});

test("a carrier with no grant option writes no grant (the Mac keeps the PATH wrapper for its third-party children)", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  assert.equal(installed.grant, undefined); assert.equal(installed.env[SNAPSHOT_GRANT_ENV], undefined);
  assert.ok(installed.env.PATH.startsWith(path.dirname(installed.shim)), "the wrapper is first on PATH");
  assert.ok(!fs.existsSync(path.join(fx.controlRoot, "snapshot-requests")), "no grant file was written");
});
