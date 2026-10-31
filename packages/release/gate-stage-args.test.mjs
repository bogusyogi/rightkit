// E2E L3-02: the door's stage arguments are parsed strictly. A flag with no value, `--flag=value`, an unknown flag, a repeated value flag or a
// stray word used to be ignored, so `--commit=<old sha>` silently built HEAD, a trailing `--commit` silently built HEAD and a trailing `--to`
// ran the default gate and printed a pass for a different command. Every one of them is now a refusal (exit 2) before git, ownership, snapshot or
// any process is touched. Arguments after `--` belong to right-release and are not read.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { main } from "./bin/rightkit-gate.mjs";

const tmp = (t, prefix) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
function fixture(t) {
  const dir = tmp(t, "rk-stage-args-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@t"); git("config", "user.name", "t");
  fs.writeFileSync(path.join(repo, "README.md"), "r\n"); fs.writeFileSync(path.join(repo, "right-release.config.json"), "{}\n");
  git("add", "-A"); git("commit", "-q", "-m", "c");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  for (const k of ["GITHUB_ACTIONS", "TAURI_CONFIG"]) delete env[k];
  return async (args) => {
    const out = [], err = [], touched = [];
    const spawn = (c, a, opts) => { touched.push(c); return c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "x" }; };
    const trap = (name) => () => { touched.push(name); throw new Error(`called ${name}`); };
    const code = await main(args, { cwd: repo, platform: "linux", env, spawn, snapshotRoot: path.join(dir, "snaps"), out: (m) => out.push(m), err: (m) => err.push(m), resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }),
      acquireOwnership: trap("acquireOwnership"), syncSnapshot: trap("syncSnapshot"), runCheck: trap("runCheck"), runCompile: trap("runCompile"), spawnInherit: trap("spawnInherit"), appCheck: trap("appCheck"), writeReceipt: trap("writeReceipt") });
    return { code, out: out.join("\n"), err: err.join("\n"), touched };
  };
}

const SHA = "0".repeat(40);
const REFUSED = [
  [["--to"], /--to needs a stage/],
  [["--to", "check", "--route", "--commit"], /--commit needs a value/],
  [["--to", "check", "--commit"], /--commit needs a value/],
  [["--to", "compile", "--platform"], /--platform needs a value/],
  [["--to", "check", "--route", `--commit=${SHA}`], /unknown option "--commit=0{40}"/],
  [["--to=check"], /unknown option "--to=check"/],
  [["--to", "check", "--route", "--bogus-flag"], /unknown option "--bogus-flag"/],
  [["--to", "check", "--route", "--commit", SHA, "--commit", SHA], /--commit was given twice/],
  [["--to", "package", "--platform", "win", "--platform", "mac", "--route"], /--platform was given twice/],
  [["--to", "check", "stray"], /unexpected argument "stray"/],
  [["--to", "dev", "--commit", "--route"], /--commit needs a value/],
];
for (const [args, why] of REFUSED) {
  test(`stage arguments: \`${args.join(" ")}\` is refused (exit 2) before anything runs`, async (t) => {
    const run = fixture(t);
    const r = await run(args);
    assert.equal(r.code, 2, r.err);
    assert.match(r.err, why);
    assert.deepEqual(r.touched.filter((c) => c !== "git" && c !== "which" && c !== "where"), [], "no process but git and which, and no ownership, snapshot, check or receipt");
    assert.equal(r.out.includes("rightkit-gate.route/1"), false, "no route answer for a malformed request");
    assert.equal(/passed/.test(r.out), false, "no pass printed for a different command");
  });
}

test("stage arguments: the documented forms still run, and arguments after `--` belong to right-release", async (t) => {
  const run = fixture(t);
  for (const args of [["--to", "check", "--route"], ["--to", "compile", "--route", "--commit", "HEAD"], ["--to", "package", "--platform", "win", "--route"], ["--to", "dev", "--route", "--", "--commit=x", "--unknown", "stray"]]) {
    const r = await run(args);
    assert.equal(r.code, 0, `${args.join(" ")}: ${r.err}`);
    assert.ok(JSON.parse(r.out.split("\n").find((l) => l.startsWith("{"))).schema === "rightkit-gate.route/1");
  }
});
