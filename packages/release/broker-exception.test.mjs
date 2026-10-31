// Adrian, 2026-10-31: the broker is mandatory on Mac and Dell; the one exception is a build through the gate without the broker that he
// expressly asked or approved, recorded in the repository being built (.rightkit/broker-exceptions.json) and read from the commit.
// #1325 (Adrian, 2026-10-31): RightKit names its unit on every run (one unit, a unit list written for the run, or all units by his exception in
// ci/all-units-exceptions.json); every other repository is one fixed, committed build definition (`cargoWorkspaces` in ci/gate.config.json)
// that a run builds whole, and a broker exception there needs no `units`. Tests named "data:" call the real functions with constructed inputs.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import {
  ALL_UNITS_ERRORS, ALL_UNITS_EXCEPTIONS_FILE, BROKER_EXCEPTIONS_FILE, BROKER_EXCEPTION_ERRORS, CARGO_WORKSPACE_LIST_ERRORS, CARGO_WORKSPACE_LIST_FILE, CARGO_WORKSPACE_LIST_KEY, RIGHTKIT_IDENTITY_FILE,
  authorizeAllUnits, authorizeNoBroker, checkCargoWorkspaceList, describeAllUnitsException, describeBrokerException, exceptionHostOf, isRightKitIdentity, unitsOfSelector,
} from "./broker-exception.mjs";
import { defaultSpawn } from "./deterministic-scans.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { main, readUnitList, rightkitDoorRule, rightkitIdentityAt } from "./bin/rightkit-gate.mjs";

const NOW = Date.parse("2026-10-31T12:00:00Z");
const FAR = Date.parse("2099-01-01T00:00:00Z");
const entry = (over = {}) => ({ id: "adrian-2026-10-31", approvedBy: "Adrian", quote: "build rightkit-license outside the broker", issue: "https://github.com/bogusyogi/rightkit/issues/1", units: ["rust:rightkit-license"], hosts: ["mac", "dell"], expires: "2026-11-30", ...over });
const record = (...entries) => JSON.stringify({ schema: 1, exceptions: entries });
const code = (fn) => { try { fn(); } catch (e) { return e.code; } return null; };
const ask = (over = {}) => authorizeNoBroker({ id: "adrian-2026-10-31", record: record(entry()), platform: "darwin", units: ["rust:rightkit-license"], now: NOW, ...over });
/** The repository identity of RightKit itself: the release package's own package.json, committed. */
const RIGHTKIT = { [RIGHTKIT_IDENTITY_FILE]: '{"name":"@rightkit/release"}\n' };

test("no record, no id, or an unknown id refuses with a typed code", () => {
  assert.equal(code(() => ask({ record: null })), BROKER_EXCEPTION_ERRORS.missing);
  assert.equal(code(() => ask({ record: record(entry({ id: "other" })) })), BROKER_EXCEPTION_ERRORS.missing);
  assert.equal(code(() => ask({ id: undefined })), BROKER_EXCEPTION_ERRORS.usage);
  assert.equal(code(() => ask({ id: "--to" })), BROKER_EXCEPTION_ERRORS.usage);
});

test("an expired record refuses; the expiry day itself still counts", () => {
  assert.equal(code(() => ask({ record: record(entry({ expires: "2026-10-30" })) })), BROKER_EXCEPTION_ERRORS.expired);
  assert.equal(code(() => ask({ record: record(entry({ expires: "2026-10-31" })) })), null);
  assert.equal(code(() => ask({ record: record(entry({ expires: "2026-10-31T11:00:00Z" })) })), BROKER_EXCEPTION_ERRORS.expired, "a timestamp is exact");
});

// Adrian, 2026-10-31: "exception lasts for what i asked for, till the time it's published. what fucking expiry does that involve?"
test("expires is optional: a record without one has no date and never expires; one with a date is still validated and enforced", () => {
  const { expires: _dropped, ...undated } = entry();
  assert.equal("expires" in undated, false);
  const x = ask({ record: record(undated), now: FAR });
  assert.equal(x.exceptionId, "adrian-2026-10-31");
  assert.equal(x.expires, null, "no date is recorded as null, never as a made-up one");
  // Every other refusal still holds without a date: host and unit coverage are what bound an undated exception.
  assert.equal(code(() => ask({ record: record(undated), platform: "linux" })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => ask({ record: record(undated), units: ["rust:rightkit-other"] })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ record: record(undated), units: unitsOfSelector(undefined) })), BROKER_EXCEPTION_ERRORS.unit);
  // A date that is present is read as ever: a past one refuses, a future one passes, and a present-but-unusable one is invalid, never "absent".
  assert.equal(code(() => ask({ record: record(entry({ expires: "2026-10-30" })) })), BROKER_EXCEPTION_ERRORS.expired);
  assert.equal(ask({ record: record(entry({ expires: "2026-11-30" })) }).expires, "2026-11-30");
  for (const bad of [{ expires: null }, { expires: "" }, { expires: "soon" }, { expires: 20261130 }, { expires: "2026-13-45" }]) {
    assert.equal(code(() => ask({ record: record(entry(bad)) })), BROKER_EXCEPTION_ERRORS.invalid, JSON.stringify(bad));
  }
});

test("data: the committed .rightkit/broker-exceptions.json is a record the gate accepts: each entry covers every unit it names, on every host it names, undated or not", () => {
  const file = fs.readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", BROKER_EXCEPTIONS_FILE), "utf8");
  const platformOf = { mac: "darwin", dell: "win32" };
  for (const e of JSON.parse(file).exceptions) {
    for (const host of e.hosts) {
      for (const unit of e.units) assert.equal(authorizeNoBroker({ id: e.id, record: file, platform: platformOf[host], units: [unit], now: Date.parse("2000-01-01T00:00:00Z") }).exceptionId, e.id, `${e.id} ${host} ${unit}`);
    }
  }
});

test("the wrong host refuses, and Linux is never an exception host", () => {
  assert.equal(code(() => ask({ record: record(entry({ hosts: ["dell"] })) })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => ask({ platform: "win32", record: record(entry({ hosts: ["mac"] })) })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => ask({ platform: "linux" })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(exceptionHostOf("darwin"), "mac");
  assert.equal(exceptionHostOf("win32"), "dell");
});

test("every selected unit must be covered; a whole run (no --unit) needs units [\"*\"] (RightKit: the unit is named)", () => {
  assert.equal(code(() => ask({ units: ["rust:rightkit-license", "rust:rightkit-other"] })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ units: ["npm:@rightkit/x"] })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ units: unitsOfSelector(undefined) })), BROKER_EXCEPTION_ERRORS.unit);
  assert.equal(code(() => ask({ units: unitsOfSelector(undefined), record: record(entry({ units: ["*"] })) })), null);
  assert.deepEqual(unitsOfSelector("cargo:rightkit-license"), ["rust:rightkit-license"]);
  assert.deepEqual(unitsOfSelector("npm:@rightkit/x"), ["npm:@rightkit/x"]);
  // A unit list covers each listed unit, and only those.
  const both = record(entry({ units: ["rust:rightkit-license", "npm:@rightkit/x"] }));
  assert.equal(code(() => ask({ record: both, units: ["rust:rightkit-license", "npm:@rightkit/x"] })), null);
  assert.equal(code(() => ask({ record: both, units: ["rust:rightkit-license", "npm:@rightkit/y"] })), BROKER_EXCEPTION_ERRORS.unit);
});

test("a malformed record or entry refuses as invalid", () => {
  assert.equal(code(() => ask({ record: "{" })), BROKER_EXCEPTION_ERRORS.invalid);
  assert.equal(code(() => ask({ record: JSON.stringify({ schema: 2, exceptions: [] }) })), BROKER_EXCEPTION_ERRORS.invalid);
  for (const bad of [{ approvedBy: "Astra" }, { quote: "" }, { issue: "1" }, { units: [] }, { units: ["cargo:x"] }, { hosts: ["linux"] }, { expires: null }, { expires: "soon" }]) {
    assert.equal(code(() => ask({ record: record(entry(bad)) })), BROKER_EXCEPTION_ERRORS.invalid, JSON.stringify(bad));
  }
  assert.equal(code(() => ask({ record: record(entry(), entry()) })), BROKER_EXCEPTION_ERRORS.invalid, "duplicate id");
});

test("a valid record yields the exception with id, quote, issue, units, host and the outside-the-broker note", () => {
  const x = ask();
  assert.equal(x.exceptionId, "adrian-2026-10-31");
  assert.equal(x.host, "mac");
  assert.deepEqual(x.units, ["rust:rightkit-license"]);
  assert.equal(x.inRightKit, true);
  const line = describeBrokerException(x);
  for (const part of ["adrian-2026-10-31", "build rightkit-license outside the broker", "https://github.com/bogusyogi/rightkit/issues/1", "rust:rightkit-license", "mac", "outside the broker by exception"]) assert.ok(line.includes(part), part);
});

// ---- #1325: the repository identity and what a broker exception names outside RightKit --------------------------------------------

test("data: RightKit is told apart by the release package's own name in the commit's packages/release/package.json, never by a path guess", () => {
  assert.equal(RIGHTKIT_IDENTITY_FILE, "packages/release/package.json");
  assert.equal(isRightKitIdentity('{"name":"@rightkit/release","version":"1.2.3"}'), true);
  for (const other of ['{"name":"@rightkit/qa"}', '{"name":"release"}', '{"name":"@rightkit/release-x"}', "{}", "[]", "null", "not json", "", undefined]) assert.equal(isRightKitIdentity(other), false, String(other));
});

test("data: outside RightKit a broker exception needs no units and covers the repository's whole run; hosts, expiry and Adrian's approval still bind it", () => {
  const { units: _units, ...noUnits } = entry();
  const outside = (over = {}) => ask({ inRightKit: false, units: unitsOfSelector(undefined), ...over });
  const x = outside({ record: record(noUnits) });
  assert.deepEqual(x.units, ["*"], "the run is the whole repository");
  assert.equal(x.inRightKit, false);
  assert.match(describeBrokerException(x), /units=\*/);
  // Units a record does state never narrow a repository's run (every run outside RightKit is a whole run) ...
  assert.equal(code(() => outside({ record: record(entry({ units: ["rust:rightkit-license"] })) })), null);
  assert.equal(code(() => outside({ record: record(entry({ units: ["npm:@rightkit/x"] })), units: ["rust:something-else"] })), null);
  // ... but are still read as units when present, so a typo is not silently accepted.
  for (const bad of [{ units: [] }, { units: ["cargo:x"] }, { units: null }, { units: "*" }]) assert.equal(code(() => outside({ record: record(entry(bad)) })), BROKER_EXCEPTION_ERRORS.invalid, JSON.stringify(bad));
  assert.equal(code(() => outside({ record: record(noUnits), platform: "linux" })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => outside({ record: record({ ...noUnits, hosts: ["dell"] }) })), BROKER_EXCEPTION_ERRORS.host);
  assert.equal(code(() => outside({ record: record({ ...noUnits, expires: "2026-10-30" }) })), BROKER_EXCEPTION_ERRORS.expired);
  assert.equal(code(() => outside({ record: record({ ...noUnits, approvedBy: "Astra" }) })), BROKER_EXCEPTION_ERRORS.invalid);
  assert.equal(code(() => outside({ record: record(noUnits), now: FAR })), null, "no date, no expiry");
  // In RightKit (and for a caller that does not say) the units are required: the unit is named every run.
  assert.equal(code(() => ask({ record: record(noUnits) })), BROKER_EXCEPTION_ERRORS.invalid);
  assert.equal(code(() => ask({ record: record(noUnits), inRightKit: true })), BROKER_EXCEPTION_ERRORS.invalid);
});

// ---- #1325: RightKit's all-units exception (ci/all-units-exceptions.json) ---------------------------------------------------------

const allRecord = (...entries) => JSON.stringify({ exceptions: entries });
const allEntry = (over = {}) => ({ id: "adrian-all-2026-10-31", approvedBy: "Adrian", quote: "all with my exception", issue: "https://github.com/bogusyogi/rightkit/issues/1325", ...over });
const allAsk = (over = {}) => authorizeAllUnits({ id: "adrian-all-2026-10-31", record: allRecord(allEntry()), now: NOW, ...over });

test("data: --all-units-exception needs an id and a committed entry quoting Adrian; the date is optional and honoured when present", () => {
  assert.equal(ALL_UNITS_EXCEPTIONS_FILE, "ci/all-units-exceptions.json");
  assert.equal(code(() => allAsk({ id: undefined })), ALL_UNITS_ERRORS.usage);
  assert.equal(code(() => allAsk({ id: "" })), ALL_UNITS_ERRORS.usage);
  assert.equal(code(() => allAsk({ id: "--route" })), ALL_UNITS_ERRORS.usage);
  assert.equal(code(() => allAsk({ record: null })), ALL_UNITS_ERRORS.missing, "the commit holds no record");
  assert.equal(code(() => allAsk({ record: allRecord(allEntry({ id: "other" })) })), ALL_UNITS_ERRORS.missing, "no entry of that id");
  assert.equal(code(() => allAsk({ record: allRecord() })), ALL_UNITS_ERRORS.missing, "an empty record");
  assert.equal(code(() => allAsk({ record: "{" })), ALL_UNITS_ERRORS.invalid);
  assert.equal(code(() => allAsk({ record: "{}" })), ALL_UNITS_ERRORS.invalid, "no exceptions array");
  assert.equal(code(() => allAsk({ record: JSON.stringify({ exceptions: {} }) })), ALL_UNITS_ERRORS.invalid);
  assert.equal(code(() => allAsk({ record: allRecord(allEntry(), allEntry()) })), ALL_UNITS_ERRORS.invalid, "duplicate id");
  for (const bad of [{ approvedBy: "Astra" }, { approvedBy: undefined }, { quote: "" }, { quote: "  " }, { quote: 5 }, { issue: "" }, { issue: 1325 }, { expires: null }, { expires: "" }, { expires: "soon" }, { expires: "2026-13-45" }]) {
    assert.equal(code(() => allAsk({ record: allRecord(allEntry(bad)) })), ALL_UNITS_ERRORS.invalid, JSON.stringify(bad));
  }
  // No date: never expires. A date: a past one refuses, the day itself and a future one pass.
  const x = allAsk({ now: FAR });
  assert.deepEqual({ ...x }, { exceptionId: "adrian-all-2026-10-31", quote: "all with my exception", issue: "https://github.com/bogusyogi/rightkit/issues/1325", expires: null });
  assert.equal(Object.isFrozen(x), true);
  assert.equal(code(() => allAsk({ record: allRecord(allEntry({ expires: "2026-10-30" })) })), ALL_UNITS_ERRORS.expired);
  assert.equal(code(() => allAsk({ record: allRecord(allEntry({ expires: "2026-10-31" })) })), null);
  assert.equal(allAsk({ record: allRecord(allEntry({ expires: "2026-11-30" })) }).expires, "2026-11-30");
  // CI's own entries (any non-empty issue text) read the same here.
  assert.equal(allAsk({ record: allRecord(allEntry({ issue: "bogusyogi/rightkit#1325" })) }).issue, "bogusyogi/rightkit#1325");
  const line = describeAllUnitsException(x);
  for (const part of ["adrian-all-2026-10-31", "all with my exception", "issues/1325", "every unit of RightKit"]) assert.ok(line.includes(part), part);
});

// ---- #1325: every other repository's fixed build definition (ci/gate.config.json cargoWorkspaces) -----------------------------------

test("data: a repository with Cargo workspaces and no committed list is refused with the exact list the gate discovered; an agreeing list passes", () => {
  assert.equal(CARGO_WORKSPACE_LIST_FILE, "ci/gate.config.json");
  assert.equal(CARGO_WORKSPACE_LIST_KEY, "cargoWorkspaces");
  const refuse = (record, discovered = ["tauri-app-next/qa-e2e", "tauri-app-next"]) => { try { checkCargoWorkspaceList({ discovered, record }); } catch (e) { return e; } return null; };
  // No file, or a file with no list: refused, and the message is the list to commit (sorted, deduplicated), never a generated file.
  for (const record of [null, undefined, "{}", JSON.stringify({ rust: {} }), JSON.stringify({ fixtures: [] }), "[]", "null"]) {
    const e = refuse(record);
    assert.equal(e?.code, CARGO_WORKSPACE_LIST_ERRORS.missing, String(record));
    assert.match(e.message, /"cargoWorkspaces": \["tauri-app-next","tauri-app-next\/qa-e2e"\]/);
    assert.match(e.message, /every repository but RightKit commits one fixed build definition/);
    assert.match(e.message, /ci\/gate\.config\.json/);
  }
  assert.match(refuse("{ not json").message, /not valid JSON/, "an unreadable config is named, and the list is still printed");
  assert.match(refuse("{ not json").message, /"cargoWorkspaces": \["tauri-app-next","tauri-app-next\/qa-e2e"\]/);
  assert.equal(refuse("{ not json").code, CARGO_WORKSPACE_LIST_ERRORS.missing);
  // A repository with no Cargo workspace owes no list.
  assert.deepEqual({ ...checkCargoWorkspaceList({ discovered: [], record: null }) }, { workspaces: [], listed: false });
  assert.deepEqual({ ...checkCargoWorkspaceList({ discovered: [], record: JSON.stringify({ rust: {} }) }) }, { workspaces: [], listed: false });
  assert.deepEqual({ ...checkCargoWorkspaceList({ discovered: [], record: "{ not json" }) }, { workspaces: [], listed: false });
  assert.deepEqual({ ...checkCargoWorkspaceList({ discovered: [], record: JSON.stringify({ cargoWorkspaces: [] }) }) }, { workspaces: [], listed: true });
  // The same set, in any order, passes (other keys of the config are not read).
  const agreed = checkCargoWorkspaceList({ discovered: ["engine", ".", "apps/x/src-tauri", "engine"], record: JSON.stringify({ rust: { a: {} }, fixtures: ["f"], cargoWorkspaces: ["apps/x/src-tauri", ".", "engine"] }) });
  assert.deepEqual({ ...agreed }, { workspaces: [".", "apps/x/src-tauri", "engine"], listed: true });
  assert.equal(Object.isFrozen(agreed), true);
});

test("data: a list that differs from the discovered workspaces is refused, missing or extra, and the message carries the list that agrees", () => {
  const differs = (list, discovered) => { try { checkCargoWorkspaceList({ discovered, record: JSON.stringify({ cargoWorkspaces: list }) }); } catch (e) { return e; } return null; };
  const missing = differs(["."], [".", "tools/b"]);
  assert.equal(missing.code, CARGO_WORKSPACE_LIST_ERRORS.differs);
  assert.match(missing.message, /the commit holds tools\/b which the list does not name/);
  assert.doesNotMatch(missing.message, /the list names/);
  assert.match(missing.message, /The list that agrees: "cargoWorkspaces": \[".","tools\/b"\]/);
  const extra = differs([".", "tools/b", "ghost"], [".", "tools/b"]);
  assert.equal(extra.code, CARGO_WORKSPACE_LIST_ERRORS.differs);
  assert.match(extra.message, /the list names ghost which the commit does not hold/);
  assert.match(extra.message, /The list that agrees: "cargoWorkspaces": \[".","tools\/b"\]/);
  const both = differs(["ghost"], ["."]);
  assert.match(both.message, /the commit holds \. which the list does not name; the list names ghost which the commit does not hold/);
  assert.equal(differs([], ["."]).code, CARGO_WORKSPACE_LIST_ERRORS.differs, "an empty list is a list that omits every workspace");
  assert.equal(differs(["."], []).code, CARGO_WORKSPACE_LIST_ERRORS.differs, "a list that names a workspace the commit does not hold");
});

test("data: a list that is not a plain, unique list of workspace directories is invalid", () => {
  const bad = (list) => { try { checkCargoWorkspaceList({ discovered: ["."], record: JSON.stringify({ cargoWorkspaces: list }) }); } catch (e) { return e.code; } return null; };
  for (const list of ["."/* a string, not a list */, { a: 1 }, null, 5, [1], [""], ["/abs"], ["../x"], ["a/../b"], ["a//b"], ["a/./b"], ["./a"], ["a/"], ["a\\b"], ["C:/x"], [".", "."], ["a", "a"], [[".", "a"]]]) {
    assert.equal(bad(list), CARGO_WORKSPACE_LIST_ERRORS.invalid, JSON.stringify(list));
  }
  assert.equal(bad(["."]), null, "the root workspace is '.'");
  assert.equal(bad(["apps/x/src-tauri", "."]), null);
});

// ---- #1325: the unit list of --units-file ----------------------------------------------------------------------------------------

const scratch = (t, prefix = "rk-unit-list-") => { const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; };
const writeJson = (dir, name, value) => { const file = path.join(dir, name); fs.writeFileSync(file, typeof value === "string" ? value : JSON.stringify(value)); return file; };
const readReal = (file) => fs.readFileSync(file, "utf8");
const unitsError = (file) => { try { readUnitList(readReal, file); } catch (e) { return e; } return null; };

test("data: --units-file reads { units: [selector, ...] } written for the run; every entry is a --unit selector, named once", (t) => {
  const dir = scratch(t);
  assert.deepEqual(readUnitList(readReal, writeJson(dir, "ok.json", { units: ["cargo:rightkit-license", "npm:@rightkit/build-control-policy"] })), ["cargo:rightkit-license", "npm:@rightkit/build-control-policy"], "in the order written");
  assert.deepEqual(readUnitList(readReal, writeJson(dir, "extra.json", { note: "for the Mac run", units: ["cargo:rightkit-a"] })), ["cargo:rightkit-a"], "other keys are not read");
  for (const [name, value] of [
    ["not-json.json", "{ units: "], ["none.json", {}], ["empty.json", { units: [] }], ["string.json", { units: "cargo:rightkit-a" }], ["object.json", { units: { a: 1 } }],
    ["number.json", { units: [1] }], ["rust.json", { units: ["rust:rightkit-a"] }], ["foreign.json", { units: ["npm:@other/x"] }], ["bare.json", { units: ["rightkit-a"] }],
    ["path.json", { units: ["cargo:rightkit-../x"] }], ["upper.json", { units: ["cargo:rightkit-A"] }], ["all.json", { units: ["*"] }], ["list.json", ["cargo:rightkit-a"]],
  ]) {
    const e = unitsError(writeJson(dir, name, value));
    assert.equal(e?.code, "RK_UNITS_FILE", name);
    assert.match(e.message, /^RK_UNITS_FILE: .*units\.|^RK_UNITS_FILE: /, name);
  }
  assert.match(unitsError(writeJson(dir, "dup.json", { units: ["cargo:rightkit-a", "npm:@rightkit/b", "cargo:rightkit-a"] })).message, /names cargo:rightkit-a more than once/);
  assert.equal(unitsError(path.join(dir, "absent.json")).code, "RK_UNITS_FILE", "an unreadable file is the same usage error");
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
  const commit = git("rev-parse", "HEAD").stdout.trim();
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  for (const k of ["GITHUB_ACTIONS", "TAURI_CONFIG"]) delete env[k];
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" });
  const io = (out, err, resolved) => ({ cwd: repo, platform: "darwin", env, spawn, snapshotRoot: path.join(dir, "snaps"), out: (m) => out.push(m), err: (m) => err.push(m), now: () => NOW,
    resolveExecutor: (a) => { resolved.push(a); return { kind: "direct", cargo: ["cargo"] }; } });
  const run = async (args, platform, extra = {}) => {
    const out = []; const err = []; const resolved = [];
    const exit = await main(args, { ...io(out, err, resolved), ...(platform ? { platform } : {}), ...extra });
    return { exit, out: out.join("\n"), err: err.join("\n"), resolved };
  };
  const gitHere = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  return { dir, repo, commit, run, gitHere };
}

test("the gate refuses --no-broker without a committed record, expired, on the wrong host, or for an uncovered unit, and runs nothing", async (t) => {
  const none = fixture(t, {});
  let r = await none.run(["--to", "compile", "--route", "--no-broker", "x"], "darwin");
  assert.equal(r.exit, 1); assert.match(r.err, /RK_BROKER_EXCEPTION_MISSING/); assert.equal(r.resolved.length, 0);

  const expired = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(entry({ expires: "2026-10-01", units: ["*"] })) });
  r = await expired.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.match(r.err, /RK_BROKER_EXCEPTION_EXPIRED/); assert.equal(r.exit, 1);

  // RightKit names its unit: the exception covers the units it lists, and a whole run (no --unit) needs units ["*"].
  const live = fixture(t, { ...RIGHTKIT, [BROKER_EXCEPTIONS_FILE]: record(entry({ hosts: ["mac"], units: ["rust:rightkit-license"] })) });
  r = await live.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "win32");
  assert.match(r.err, /RK_BROKER_EXCEPTION_HOST/);
  r = await live.run(["--to", "compile", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.match(r.err, /RK_BROKER_EXCEPTION_UNIT/); assert.equal(r.exit, 1); assert.equal(r.resolved.length, 0, "a whole run needs units [\"*\"]");
  r = await live.run(["--to", "compile", "--route", "--no-broker"], "darwin");
  assert.equal(r.exit, 2); assert.match(r.err, /RK_BROKER_EXCEPTION_USAGE/);
});

test("a record with no expires routes the gate direct, whatever the date", async (t) => {
  const { expires: _dropped, ...undated } = entry({ units: ["*"] });
  const f = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(undated) });
  const r = await f.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin", { now: () => FAR });
  assert.equal(r.exit, 0, r.err);
  assert.ok(r.resolved.length > 0 && r.resolved.every((a) => a.forceDirect === true));
  assert.doesNotMatch(r.err, /EXPIRED/);
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

test("#1325: outside RightKit the gate takes a broker exception that lists no units, or other units, for the whole run; in RightKit the same record is refused", async (t) => {
  const { units: _units, ...noUnits } = entry();
  const outsideNone = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(noUnits) });
  let r = await outsideNone.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.equal(r.exit, 0, r.err);
  assert.ok(r.resolved.length > 0 && r.resolved.every((a) => a.forceDirect === true));
  assert.match(r.out, /BROKER EXCEPTION exceptionId=adrian-2026-10-31 host=mac units=\* /, "the printed (and recorded) coverage is the whole repository");
  const outsideOther = fixture(t, { [BROKER_EXCEPTIONS_FILE]: record(entry({ units: ["rust:rightkit-license"] })) });
  r = await outsideOther.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.equal(r.exit, 0, r.err);
  const inRightKit = fixture(t, { ...RIGHTKIT, [BROKER_EXCEPTIONS_FILE]: record(noUnits) });
  r = await inRightKit.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.equal(r.exit, 1); assert.match(r.err, /RK_BROKER_EXCEPTION_INVALID.*units must be/); assert.equal(r.resolved.length, 0);
  // The identity is the selected commit's: a checkout edit that adds the RightKit package name changes nothing.
  fs.mkdirSync(path.join(outsideNone.repo, "packages", "release"), { recursive: true });
  fs.writeFileSync(path.join(outsideNone.repo, RIGHTKIT_IDENTITY_FILE), RIGHTKIT[RIGHTKIT_IDENTITY_FILE]);
  r = await outsideNone.run(["--to", "compile", "--route", "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.equal(r.exit, 0, "the commit holds no RightKit identity, so it is still outside RightKit");
});

test("rightkitIdentityAt reads the commit's package.json, not the checkout", (t) => {
  const rk = fixture(t, RIGHTKIT); const plain = fixture(t, {}); const other = fixture(t, { [RIGHTKIT_IDENTITY_FILE]: '{"name":"@rightkit/qa"}\n' });
  assert.equal(rightkitIdentityAt(rk.gitHere, rk.commit), true);
  assert.equal(rightkitIdentityAt(plain.gitHere, plain.commit), false);
  assert.equal(rightkitIdentityAt(other.gitHere, other.commit), false);
  fs.mkdirSync(path.join(plain.repo, "packages", "release"), { recursive: true });
  fs.writeFileSync(path.join(plain.repo, RIGHTKIT_IDENTITY_FILE), RIGHTKIT[RIGHTKIT_IDENTITY_FILE]);
  assert.equal(rightkitIdentityAt(plain.gitHere, plain.commit), false, "an uncommitted file is not the repository's identity");
  assert.equal(rightkitIdentityAt(rk.gitHere, "0".repeat(40)), false, "an unknown commit is not RightKit");
});

// ---- #1325: RightKit's door names its unit on every run ---------------------------------------------------------------------------

test("#1325: a RightKit door run that names no unit is refused before anything runs; every other repository is not asked for a unit", async (t) => {
  const rk = fixture(t, RIGHTKIT);
  for (const args of [["--to", "check"], ["--to", "compile", "--route"], ["--to", "package", "--platform", "mac"], ["--to", "dev"]]) {
    const r = await rk.run(args, "darwin");
    assert.equal(r.exit, 1, args.join(" ")); assert.match(r.err, /RK_UNIT_NOT_NAMED: RightKit names its unit on every run: --unit /);
    assert.match(r.err, /--all-units-exception <id>.*ci\/all-units-exceptions\.json.*nothing ran/);
    assert.equal(r.resolved.length, 0, "no executor was even resolved");
  }
  const plain = fixture(t, {});
  const r = await plain.run(["--to", "compile", "--route"], "darwin");
  assert.equal(r.exit, 0, r.err); assert.doesNotMatch(r.err, /RK_UNIT_NOT_NAMED/);
});

test("#1325: --units-file and --all-units-exception are read strictly: one way to name the unit, a value, a readable list, RightKit only", async (t) => {
  const rk = fixture(t, RIGHTKIT); const plain = fixture(t, {});
  const list = writeJson(rk.dir, "units.json", { units: ["cargo:rightkit-a", "npm:@rightkit/b"] });
  const cases = [
    [rk, ["--units-file", list], 2, /--units-file applies to a --to stage/],
    [rk, ["--all-units-exception", "x"], 2, /--all-units-exception applies to a --to stage/],
    [rk, ["--to", "check", "--unit", "cargo:rightkit-a", "--units-file", list], 2, /name the unit once/],
    [rk, ["--to", "check", "--units-file", list, "--all-units-exception", "x"], 2, /name the unit once/],
    [rk, ["--to", "check", "--unit", "cargo:rightkit-a", "--all-units-exception", "x"], 2, /name the unit once/],
    [rk, ["--to", "check", "--units-file"], 2, /--units-file needs a value/],
    [rk, ["--to", "check", "--units-file", "--route"], 2, /--units-file needs a value/],
    [rk, ["--to", "check", "--all-units-exception"], 2, /--all-units-exception needs a value/],
    [rk, ["--to", "check", "--units-file", writeJson(rk.dir, "bad.json", { units: [] })], 2, /RK_UNITS_FILE/],
    [rk, ["--to", "check", "--units-file", path.join(rk.dir, "absent.json")], 2, /RK_UNITS_FILE/],
    [rk, ["--to", "check", "--units-file", list, "--platform", "mac"], 2, /takes only --to, --units-file and optional --commit/],
    [rk, ["--to", "check", "--units-file", list, "--receipt-out", "r.json"], 2, /takes only --to, --units-file and optional --commit/],
    [rk, ["--to", "compile", "--route", "--units-file", list], 2, /--route runs nothing and takes no --units-file/],
    [rk, ["--to", "compile", "--route", "--all-units-exception", "x"], 2, /--route runs nothing and takes no --all-units-exception/],
    [plain, ["--to", "check", "--units-file", list], 1, /--units-file names RightKit units and [0-9a-f]{12} is not RightKit; .*cargoWorkspaces list committed in ci\/gate\.config\.json/],
    [plain, ["--to", "check", "--all-units-exception", "x"], 1, /--all-units-exception names RightKit units and [0-9a-f]{12} is not RightKit/],
  ];
  for (const [f, args, exit, message] of cases) {
    const r = await f.run(args, "darwin");
    assert.equal(r.exit, exit, `${args.join(" ")}: ${r.err}`);
    assert.match(r.err, message, args.join(" "));
    assert.equal(r.resolved.length, 0, `${args.join(" ")} ran nothing`);
  }
});

test("#1325: --all-units-exception runs the whole RightKit door only under a committed entry quoting Adrian (rightkitDoorRule, real git)", async (t) => {
  const withEntry = fixture(t, { ...RIGHTKIT, [ALL_UNITS_EXCEPTIONS_FILE]: allRecord(allEntry()) });
  const out = []; const err = [];
  const o = { out: (m) => out.push(m), err: (m) => err.push(m), now: () => NOW };
  const flags = { unitsFileFlag: false, allUnitsFlag: true, unitList: null };
  const args = ["--to", "check", "--all-units-exception", "adrian-all-2026-10-31"];
  assert.deepEqual(await rightkitDoorRule(o, args, withEntry.repo, withEntry.gitHere, {}, flags), {}, "no exit: the whole-repository run goes on");
  assert.deepEqual(args, ["--to", "check"], "the flag and its id are removed from what the stage reads");
  assert.ok(out.some((m) => /ALL UNITS EXCEPTION exceptionId=adrian-all-2026-10-31 .*every unit of RightKit, by exception/.test(m)), out.join("\n"));
  assert.deepEqual(err, []);
  // Refused, each typed, with the arguments untouched and nothing printed as granted.
  const refused = async (f, id, expectExit, expectCode, over = {}) => {
    const errs = [];
    const o2 = { out: () => assert.fail("nothing is granted"), err: (m) => errs.push(m), now: () => NOW, ...over };
    const a = ["--to", "check", "--all-units-exception", id];
    const result = await rightkitDoorRule(o2, a, f.repo, f.gitHere, {}, flags);
    assert.deepEqual(result, { exit: expectExit }, `${id}: ${errs.join(" ")}`);
    assert.match(errs.join("\n"), new RegExp(`BLOCKED: ${expectCode}.*nothing ran`));
    assert.deepEqual(a, ["--to", "check", "--all-units-exception", id], "the arguments are not consumed by a refusal");
  };
  await refused(withEntry, "someone-elses", 1, ALL_UNITS_ERRORS.missing);
  await refused(withEntry, undefined, 2, ALL_UNITS_ERRORS.usage);
  await refused(fixture(t, RIGHTKIT), "adrian-all-2026-10-31", 1, ALL_UNITS_ERRORS.missing);
  await refused(fixture(t, { ...RIGHTKIT, [ALL_UNITS_EXCEPTIONS_FILE]: allRecord(allEntry({ approvedBy: "Astra" })) }), "adrian-all-2026-10-31", 1, ALL_UNITS_ERRORS.invalid);
  await refused(fixture(t, { ...RIGHTKIT, [ALL_UNITS_EXCEPTIONS_FILE]: allRecord(allEntry({ expires: "2026-10-01" })) }), "adrian-all-2026-10-31", 1, ALL_UNITS_ERRORS.expired);
  // The record is the selected commit's: an entry that exists only in the checkout does not count.
  const later = fixture(t, RIGHTKIT);
  fs.mkdirSync(path.join(later.repo, "ci"), { recursive: true });
  fs.writeFileSync(path.join(later.repo, ALL_UNITS_EXCEPTIONS_FILE), allRecord(allEntry()));
  await refused(later, "adrian-all-2026-10-31", 1, ALL_UNITS_ERRORS.missing);
  // An undated entry never expires; the same entry outside RightKit is not Adrian's to grant here.
  const undated = fixture(t, { ...RIGHTKIT, [ALL_UNITS_EXCEPTIONS_FILE]: allRecord(allEntry()) });
  assert.deepEqual(await rightkitDoorRule({ out: () => {}, err: () => assert.fail("granted"), now: () => FAR }, ["--to", "check", "--all-units-exception", "adrian-all-2026-10-31"], undated.repo, undated.gitHere, {}, flags), {});
  const outside = fixture(t, { [ALL_UNITS_EXCEPTIONS_FILE]: allRecord(allEntry()) });
  const outsideErr = [];
  assert.deepEqual(await rightkitDoorRule({ out: () => {}, err: (m) => outsideErr.push(m), now: () => NOW }, ["--to", "check", "--all-units-exception", "adrian-all-2026-10-31"], outside.repo, outside.gitHere, {}, flags), { exit: 1 });
  assert.match(outsideErr.join("\n"), /is not RightKit/);
  // An unresolvable commit is the stage's own refusal (and the route probe's answer): the rule goes on without judging.
  assert.deepEqual(await rightkitDoorRule({ out: () => {}, err: () => assert.fail("judged"), now: () => NOW }, ["--to", "check", "--commit", "deadbeef", "--all-units-exception", "x"], withEntry.repo, withEntry.gitHere, {}, flags), {});
});

test("#1325: --units-file runs each listed unit as its own unit run on one commit, in order, and stops at the first that does not pass; a broker exception must cover every listed unit", async (t) => {
  const rk = fixture(t, RIGHTKIT);
  const list = writeJson(rk.dir, "units.json", { units: ["cargo:rightkit-a", "npm:@rightkit/b"] });
  // No unit of this fixture has a committed manifest or local-build grant, so the first unit run refuses itself (the unit path's own check).
  let r = await rk.run(["--to", "check", "--units-file", list], "darwin");
  assert.equal(r.exit, 1, r.err);
  assert.match(r.out, new RegExp(`unit list 1/2: cargo:rightkit-a on commit ${rk.commit.slice(0, 12)}`));
  assert.match(r.err, /unit and local grant must be tracked canonical files/);
  assert.match(r.err, /BLOCKED: unit cargo:rightkit-a ended with exit 1; 1 unit\(s\) of the list did not run/);
  assert.doesNotMatch(r.out, /unit list 2\/2/, "the second unit never started");
  // The same commit is named to every unit run: a later HEAD changes nothing mid-list.
  r = await rk.run(["--to", "check", "--commit", rk.commit, "--units-file", list], "darwin");
  assert.equal(r.exit, 1); assert.match(r.out, /unit list 1\/2/);
  // A broker exception covers a unit list only when it names every listed unit; a covered list reaches the unit runs.
  const onlyA = fixture(t, { ...RIGHTKIT, [BROKER_EXCEPTIONS_FILE]: record(entry({ units: ["rust:rightkit-a"] })) });
  const listA = writeJson(onlyA.dir, "units.json", { units: ["cargo:rightkit-a", "npm:@rightkit/b"] });
  r = await onlyA.run(["--to", "check", "--units-file", listA, "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.equal(r.exit, 1); assert.match(r.err, /RK_BROKER_EXCEPTION_UNIT.*not npm:@rightkit\/b/); assert.doesNotMatch(r.out, /unit list/);
  const both = fixture(t, { ...RIGHTKIT, [BROKER_EXCEPTIONS_FILE]: record(entry({ units: ["rust:rightkit-a", "npm:@rightkit/b"] })) });
  const listBoth = writeJson(both.dir, "units.json", { units: ["cargo:rightkit-a", "npm:@rightkit/b"] });
  r = await both.run(["--to", "check", "--units-file", listBoth, "--no-broker", "adrian-2026-10-31"], "darwin");
  assert.match(r.out, /BROKER EXCEPTION exceptionId=adrian-2026-10-31/);
  assert.match(r.out, /unit list 1\/2: cargo:rightkit-a/, "the exception covered the list and the unit runs started");
});

// ---- #1325: the committed build definition, enforced where the gate discovers the commit's Cargo workspaces -------------------------

const pkg = (name) => `[package]\nname = "${name}"\nversion = "1.0.0"\nedition = "2021"\nlicense = "MIT"\n`;
const WORKSPACES = {
  "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "Cargo.lock": "version = 4\n", "crates/a/Cargo.toml": pkg("a"), "crates/a/src/lib.rs": "pub fn a() {}\n", "crates/a/CHANGELOG.md": "## 1.0.0\n- a\n",
  "tools/b/Cargo.toml": `${pkg("b")}[workspace]\n`, "tools/b/Cargo.lock": "version = 4\n", "tools/b/src/lib.rs": "pub fn b() {}\n", "tools/b/CHANGELOG.md": "## 1.0.0\n- b\n",
};
const gateConfig = (cargoWorkspaces) => `${JSON.stringify(cargoWorkspaces === undefined ? { rust: {} } : { rust: {}, cargoWorkspaces }, null, 2)}\n`;

/** A committed repository, the gate's snapshot of that commit, and a `check --commit --full-a --snapshot` run over it with the scan runner stubbed (nothing compiles). */
function listFixture(t, files) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-ws-list-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const repo = path.join(base, "repo"); fs.mkdirSync(repo);
  const git = (...a) => { const r = spawnSync("git", a, { cwd: repo, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout.trim(); };
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@t"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(repo, "README.md"), "base\n"); git("add", "-A"); git("commit", "-q", "-m", "base");
  const firstCommit = git("rev-parse", "HEAD");
  for (const [rel, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git("add", "-A"); git("commit", "-q", "--allow-empty", "-m", "c");
  const commit = git("rev-parse", "HEAD");
  const snap = syncSnapshot({ checkout: repo, rev: commit, root: path.join(base, "snapshots") });
  const gatePackage = path.join(base, "gate-package"); fs.mkdirSync(gatePackage); fs.writeFileSync(path.join(gatePackage, "stub.mjs"), "// stand-in gate package for the environment binding\n");
  const metadataCalls = [];
  const spawn = (cmd, args, opts = {}) => {
    if (cmd === "pnpm" && args[0] === "--version") return { status: 0, stdout: "11.25.0\n", stderr: "" };
    if (cmd === "cargo" && args[0] === "metadata") {
      metadataCalls.push(path.relative(snap.path, opts.cwd).replaceAll("\\", "/") || ".");
      const rel = path.relative(snap.path, opts.cwd).replaceAll("\\", "/");
      const members = rel === "" ? [["a", path.join(opts.cwd, "crates", "a")]] : [[path.basename(opts.cwd), opts.cwd]];
      return { status: 0, stdout: JSON.stringify({ workspace_root: opts.cwd, packages: members.map(([name, dir]) => ({ name, manifest_path: path.join(dir, "Cargo.toml"), publish: null })) }), stderr: "" };
    }
    return defaultSpawn(cmd, args, opts);
  };
  const run = async (extraArgs = [], extraIo = {}) => {
    const out = []; const err = []; const plans = [];
    const exit = await main(["check", "--commit", commit, "--full-a", "--snapshot", snap.path, ...extraArgs], {
      cwd: repo, gatePackage, env: { ...process.env, RK_GATE_CACHE: "0", RK_GATE_TMPDIR: base }, platform: process.platform, snapshotRoot: path.join(base, "snapshots-own"), spawn, out: (m) => out.push(m), err: (m) => err.push(m),
      appCheck: async () => 0, classA: undefined, selfPath: path.join(base, "elsewhere", "rightkit-gate.mjs"), resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }),
      runScans: async (plan) => { plans.push(plan); return { scans: [], notes: [], cacheHits: [] }; }, ...extraIo,
    });
    return { exit, out: out.join("\n"), err: err.join("\n"), plans, metadataCalls };
  };
  return { repo, commit, firstCommit, base, run, metadataCalls };
}

test("#1325: outside RightKit a run with no committed workspace list is refused, prints the exact list to commit, and scans nothing", async (t) => {
  const f = listFixture(t, WORKSPACES);
  const r = await f.run();
  assert.equal(r.exit, 1, `${r.out}\n${r.err}`);
  assert.match(r.err, /BLOCKED: RK_CARGO_WORKSPACE_LIST_MISSING: the commit holds 2 Cargo workspace\(s\)/);
  assert.match(r.err, /"cargoWorkspaces": \[".","tools\/b"\]/, "the exact list the gate discovered");
  assert.match(r.err, /ci\/gate\.config\.json.*nothing ran/);
  assert.equal(r.plans.length, 0, "no scan was planned into a run, let alone started");
  assert.deepEqual([...new Set(f.metadataCalls)].sort(), [".", "tools/b"], "the workspaces were discovered through cargo, each in its own directory");
});

test("#1325: outside RightKit a run whose commit lists exactly its workspaces builds them all; a missing or an extra entry refuses", async (t) => {
  const agreed = await listFixture(t, { ...WORKSPACES, "ci/gate.config.json": gateConfig(["tools/b", "."]) }).run();
  assert.equal(agreed.exit, 0, `${agreed.out}\n${agreed.err}`);
  assert.equal(agreed.plans.length, 1, "the scans ran");
  assert.doesNotMatch(agreed.err, /RK_CARGO_WORKSPACE_LIST/);
  const missing = await listFixture(t, { ...WORKSPACES, "ci/gate.config.json": gateConfig(["."]) }).run();
  assert.equal(missing.exit, 1, `${missing.out}\n${missing.err}`);
  assert.match(missing.err, /RK_CARGO_WORKSPACE_LIST_DIFFERS.*the commit holds tools\/b which the list does not name.*"cargoWorkspaces": \[".","tools\/b"\].*nothing ran/);
  assert.equal(missing.plans.length, 0);
  const extra = await listFixture(t, { ...WORKSPACES, "ci/gate.config.json": gateConfig([".", "tools/b", "ghost"]) }).run();
  assert.equal(extra.exit, 1);
  assert.match(extra.err, /RK_CARGO_WORKSPACE_LIST_DIFFERS.*the list names ghost which the commit does not hold/);
  const invalid = await listFixture(t, { ...WORKSPACES, "ci/gate.config.json": gateConfig(["../up", "."]) }).run();
  assert.equal(invalid.exit, 1);
  assert.match(invalid.err, /RK_CARGO_WORKSPACE_LIST_INVALID/);
  const config = await listFixture(t, { ...WORKSPACES, "ci/gate.config.json": gateConfig(undefined) }).run();
  assert.equal(config.exit, 1, "a gate config with other keys but no list is no list");
  assert.match(config.err, /RK_CARGO_WORKSPACE_LIST_MISSING/);
});

test("#1325: a repository with no Cargo workspace owes no list; RightKit is judged per unit, not by a list", async (t) => {
  const js = await listFixture(t, { "package.json": '{"name":"x","private":true}\n', "README.md": "x\n" }).run();
  assert.equal(js.exit, 0, `${js.out}\n${js.err}`);
  assert.doesNotMatch(js.err, /RK_CARGO_WORKSPACE_LIST/);
  // RightKit's own repository (its commit holds the release package) is exempt from the list; its preflight and exports checks are the commit's own scripts.
  const rk = await listFixture(t, { ...WORKSPACES, ...RIGHTKIT, "scripts/preflight.mjs": "", "scripts/ci/exports-import-effects.mjs": "" }).run();
  assert.doesNotMatch(rk.err, /RK_CARGO_WORKSPACE_LIST/);
  assert.equal(rk.exit, 0, `${rk.out}\n${rk.err}`);
});

test("#1325: a plan builds nothing, so it says what a run would be refused for and still plans", async (t) => {
  const f = listFixture(t, WORKSPACES);
  const { exit, out, err } = await (async () => {
    const collected = { out: [], err: [] };
    const spawn = (cmd, args, opts = {}) => {
      if (cmd === "cargo" && args[0] === "metadata") {
        const rel = path.relative(f.repo, opts.cwd).replaceAll("\\", "/");
        const members = rel === "" ? [["a", path.join(opts.cwd, "crates", "a")]] : [[path.basename(opts.cwd), opts.cwd]];
        return { status: 0, stdout: JSON.stringify({ workspace_root: opts.cwd, packages: members.map(([name, dir]) => ({ name, manifest_path: path.join(dir, "Cargo.toml"), publish: null })) }), stderr: "" };
      }
      return defaultSpawn(cmd, args, opts);
    };
    const code = await main(["--plan", "--base", f.firstCommit], { cwd: f.repo, env: { ...process.env, RK_GATE_CACHE: "0" }, platform: process.platform, spawn, out: (m) => collected.out.push(m), err: (m) => collected.err.push(m), resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }) });
    return { exit: code, out: collected.out.join("\n"), err: collected.err.join("\n") };
  })();
  assert.doesNotMatch(err, /BLOCKED: RK_CARGO_WORKSPACE_LIST/, "a plan is not refused");
  assert.match(out, /note: a run would be refused: RK_CARGO_WORKSPACE_LIST_MISSING/);
  assert.match(out, /"cargoWorkspaces": \[".","tools\/b"\]/);
  assert.equal(exit, 0, `${out}\n${err}`);
});
