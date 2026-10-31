// Adrian, 2026-10-31: the broker is mandatory on Mac and Dell; the one exception is a build through the gate without the broker that he
// expressly asked or approved, recorded in the repository being built (.rightkit/broker-exceptions.json) and read from the commit.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { BROKER_EXCEPTIONS_FILE, BROKER_EXCEPTION_ERRORS, authorizeNoBroker, describeBrokerException, exceptionHostOf, unitsOfSelector } from "./broker-exception.mjs";
import { main } from "./bin/rightkit-gate.mjs";

const NOW = Date.parse("2026-10-31T12:00:00Z");
const entry = (over = {}) => ({ id: "adrian-2026-10-31", approvedBy: "Adrian", quote: "build rightkit-license outside the broker", issue: "https://github.com/bogusyogi/rightkit/issues/1", units: ["rust:rightkit-license"], hosts: ["mac", "dell"], expires: "2026-11-30", ...over });
const record = (...entries) => JSON.stringify({ schema: 1, exceptions: entries });
const code = (fn) => { try { fn(); } catch (e) { return e.code; } return null; };
const ask = (over = {}) => authorizeNoBroker({ id: "adrian-2026-10-31", record: record(entry()), platform: "darwin", units: ["rust:rightkit-license"], now: NOW, ...over });

test("no record, no id, or an unknown id refuses with a typed code", () => {
  assert.equal(code(() => ask({ record: null })), BROKER_EXCEPTION_ERRORS.missing);
  assert.equal(code(() => ask({ record: record(entry({ id: "other" })) })), BROKER_EXCEPTION_ERRORS.missing);
  assert.equal(code(() => ask({ id: undefined })), BROKER_EXCEPTION_ERRORS.usage);
  assert.equal(code(() => ask({ id: "--to" })), BROKER_EXCEPTION_ERRORS.usage);
});

test("an expired record refuses; the expiry day itself still counts", () => {
  assert.equal(code(() => ask({ record: record(entry({ expires: "2026-10-30" })) })), BROKER_EXCEPTION_ERRORS.expired);
  assert.equal(code(() => ask({ record: record(entry({ expires: "2026-10-31" })) })), null);
});

test("the wrong host refuses, and Linux is never an exception host", () => {
  assert.equal(code(() => ask({ record: record(entry({ hosts: ["dell"] })) })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => ask({ platform: "win32", record: record(entry({ hosts: ["mac"] })) })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => ask({ platform: "linux" })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(exceptionHostOf("darwin"), "mac");
  assert.equal(exceptionHostOf("win32"), "dell");
});

test("every selected unit must be covered; a whole run (no --unit) needs units [\"*\"]", () => {
  assert.equal(code(() => ask({ units: ["rust:rightkit-license", "rust:rightkit-other"] })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ units: ["npm:@rightkit/x"] })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ units: unitsOfSelector(undefined) })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ units: unitsOfSelector(undefined), record: record(entry({ units: ["*"] })) })), null);
  assert.deepEqual(unitsOfSelector("cargo:rightkit-license"), ["rust:rightkit-license"]);
  assert.deepEqual(unitsOfSelector("npm:@rightkit/x"), ["npm:@rightkit/x"]);
});

test("a malformed record or entry refuses as invalid", () => {
  assert.equal(code(() => ask({ record: "{" })), BROKER_EXCEPTION_ERRORS.invalid);
  assert.equal(code(() => ask({ record: JSON.stringify({ schema: 2, exceptions: [] }) })), BROKER_EXCEPTION_ERRORS.invalid);
  for (const bad of [{ approvedBy: "Astra" }, { quote: "" }, { issue: "1" }, { units: [] }, { units: ["cargo:x"] }, { hosts: ["linux"] }, { expires: undefined }, { expires: "soon" }]) {
    assert.equal(code(() => ask({ record: record(entry(bad)) })), BROKER_EXCEPTION_ERRORS.invalid, JSON.stringify(bad));
  }
  assert.equal(code(() => ask({ record: record(entry(), entry()) })), BROKER_EXCEPTION_ERRORS.invalid, "duplicate id");
});

test("a valid record yields the exception with id, quote, issue, units, host and the outside-the-broker note", () => {
  const x = ask();
  assert.equal(x.exceptionId, "adrian-2026-10-31");
  assert.equal(x.host, "mac");
  assert.deepEqual(x.units, ["rust:rightkit-license"]);
  const line = describeBrokerException(x);
  for (const part of ["adrian-2026-10-31", "build rightkit-license outside the broker", "https://github.com/bogusyogi/rightkit/issues/1", "rust:rightkit-license", "mac", "outside the broker by exception"]) assert.ok(line.includes(part), part);
});

// ---- the real entry: the record is read from the commit and routes the gate direct ------------------------------------------------

function fixture(t, files) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-broker-exc-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@t"); git("config", "user.name", "t");
  for (const [rel, body] of Object.entries({ "README.md": "r\n", "right-release.config.json": "{}\n", ...files })) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git("add", "-A"); git("commit", "-q", "-m", "c");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  for (const k of ["GITHUB_ACTIONS", "TAURI_CONFIG"]) delete env[k];
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" });
  const run = async (args, platform, extra = {}) => {
    const out = []; const err = []; const resolved = [];
    const exit = await main(args, { cwd: repo, platform, env, spawn, snapshotRoot: path.join(dir, "snaps"), out: (m) => out.push(m), err: (m) => err.push(m), now: () => NOW,
      resolveExecutor: (a) => { resolved.push(a); return { kind: "direct", cargo: ["cargo"] }; }, ...extra });
    return { exit, out: out.join("\n"), err: err.join("\n"), resolved };
  };
  return { repo, run };
}

test("the gate refuses --no-broker without a committed record, expired, on the wrong host, or for an uncovered unit, and runs nothing", async (t) => {
  const none = fixture(t, {});
  let r = await none.run(["--to", "compile", "--route", "--no-broker", "x"], "darwin");
  assert.equal(r.exit, 1); assert.match(r.err, /RK_BROKER_EXCEPTION_MISSING/); assert.equal(r.resolved.length, 0);

  const expired = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(entry({ expires: "2026-10-01", units: ["*"] })) });
  r = await expired.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.match(r.err, /RK_BROKER_EXCEPTION_EXPIRED/); assert.equal(r.exit, 1);

  const live = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(entry({ hosts: ["mac"], units: ["rust:rightkit-license"] })) });
  r = await live.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "win32");
  assert.match(r.err, /RK_BROKER_EXCEPTION_HOST/);
  r = await live.run(["--to", "compile", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.match(r.err, /RK_BROKER_EXCEPTION_UNIT/); assert.equal(r.exit, 1); assert.equal(r.resolved.length, 0, "a whole run needs units [\"*\"]");
  r = await live.run(["--to", "compile", "--route", "--no-broker"], "darwin");
  assert.equal(r.exit, 2); assert.match(r.err, /RK_BROKER_EXCEPTION_USAGE/);
});

test("a valid record routes the same stages through the direct executor and prints the exception; no flag leaves the broker route alone", async (t) => {
  const f = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(entry({ units: ["*"] })) });
  const withFlag = await f.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.equal(withFlag.exit, 0, withFlag.err);
  assert.ok(withFlag.resolved.length > 0 && withFlag.resolved.every((a) => a.forceDirect === true));
  assert.match(withFlag.out, /BROKER EXCEPTION exceptionId=adrian-2026-10-31 host=mac/);
  assert.match(withFlag.out, /outside the broker by exception/);
  assert.match(withFlag.out, /build rightkit-license outside the broker/);
  const without = await f.run(["--to", "compile", "--route"], "darwin");
  assert.ok(without.resolved.length > 0 && without.resolved.every((a) => a.forceDirect === undefined));
  assert.doesNotMatch(without.out, /BROKER EXCEPTION/);
});
