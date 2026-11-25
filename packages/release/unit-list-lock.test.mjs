// NN13: a unit list is one run under one snapshot lock (bin/rightkit-gate.mjs runSharedUnitList). The lock is taken once for the list, every
// unit's run joins it (runUnitSnapshot reads io.unitListLock) and it is released once, after the last unit. The lock is real here and is not
// re-entrant: a unit that took its own would be refused as busy, which is what the first form of this change did (2026-11-20: the join sat in
// the door's stage runner, which a `--unit` run never reaches, so every unit after the list's lock would have been refused).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { main, runSharedUnitList } from "./bin/rightkit-gate.mjs";
import { takeSnapshotLock } from "./snapshot.mjs";

const GIT = ["-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"];

/** A checkout with one commit that tracks two units and the local-build grant a unit run asks for. Returns { dir, commit, base }. */
function checkout(t) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-unit-list-lock-")));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const dir = path.join(base, "checkout");
  for (const crate of ["rightkit-one", "rightkit-two"]) {
    fs.mkdirSync(path.join(dir, "crates", crate, "src"), { recursive: true });
    fs.writeFileSync(path.join(dir, "crates", crate, "Cargo.toml"), `[package]\nname = "${crate}"\nversion = "0.1.0"\nedition = "2021"\n`);
    fs.writeFileSync(path.join(dir, "crates", crate, "src", "lib.rs"), "");
  }
  fs.writeFileSync(path.join(dir, ".rightkit-local-build.json"), "{}\n");
  const git = (...args) => { const r = spawnSync("git", [...GIT, ...args], { cwd: dir, encoding: "utf8" }); assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`); return r.stdout.trim(); };
  git("init", "-q"); git("add", "-A"); git("commit", "-q", "-m", "two units");
  return { dir, commit: git("rev-parse", "HEAD"), base };
}

/** What a gate run is given: git answers from the checkout, nothing else starts, no broker on this host, and the snapshot sync stops the unit. */
function harness(f, { sync }) {
  const lines = []; const taken = []; const released = [];
  const io = {
    cwd: f.dir, platform: process.platform,
    env: Object.fromEntries(Object.entries(process.env).filter(([name]) => !/^(GITHUB_ACTIONS|TAURI_CONFIG|RK_|RIGHTKIT_|RUSTRIGHT_)/i.test(name))),
    out: (m) => lines.push(String(m)), err: (m) => lines.push(String(m)),
    spawn: (cmd, args, options = {}) => (cmd === "git" ? spawnSync("git", args, { cwd: options.cwd ?? f.dir, encoding: "utf8", input: options.input }) : { status: 1, stdout: "", stderr: `${cmd} is not started by this test`, signal: null }),
    resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"], env: {} }),
    snapshotRoot: path.join(f.base, "snapshots"),
    takeSnapshotLock: (args) => {
      // The real lock (on Windows it takes several seconds to contain its holder; that is the lock's own cost, not this change's).
      const release = takeSnapshotLock({ ...args, managedRoot: null });
      taken.push(release.token);
      const counted = () => { released.push(release.token); release(); };
      return Object.assign(counted, { held: release.held, lockFile: release.lockFile, token: release.token });
    },
    syncSnapshot: sync,
  };
  return { io, lines, taken, released };
}

test("a unit list takes the snapshot lock once; every unit joins it; it is released once, after the last unit", async (t) => {
  const f = checkout(t);
  const joined = [];
  const h = harness(f, { sync: ({ lock }) => { joined.push({ token: lock?.held?.token, held: lock?.held?.held?.() === true, file: lock?.held?.lockFile }); throw Object.assign(new Error("stopped by the test before any file is written"), { code: "busy" }); } });
  const o = { exists: fs.existsSync, readFile: (file) => fs.readFileSync(file, "utf8"), writeFile: fs.writeFileSync, mkdir: fs.mkdirSync, ...h.io };
  const exit = await runSharedUnitList({ o, io: h.io, stage: "check", commit: f.commit, selected: [{ selector: "cargo:rightkit-one" }, { selector: "cargo:rightkit-two" }], inventory: null, root: f.dir });
  const log = h.lines.join("\n");
  assert.equal(h.taken.length, 1, `one lock for the whole list:\n${log}`);
  assert.equal((log.match(/snapshot lock held /g) ?? []).length, 1, `one \`snapshot lock held\` line:\n${log}`);
  assert.equal(joined.length, 2, `both units reached their snapshot sync under the list's lock:\n${log}`);
  for (const j of joined) { assert.equal(j.token, h.taken[0]); assert.equal(j.held, true); }
  assert.doesNotMatch(log, /snapshot is held by|snapshot lock .* (busy|is not held)/, "no unit was refused by the list's own lock");
  assert.deepEqual(h.released, h.taken, "released once, by the list");
  assert.equal(fs.existsSync(joined[0].file), false, "the lock file is gone after the list");
  assert.notEqual(exit, 0, "both units were stopped by the test, so the list did not pass");
});

test("a unit run on its own still takes and releases its own lock", async (t) => {
  const f = checkout(t);
  const h = harness(f, { sync: () => { throw Object.assign(new Error("stopped by the test before any file is written"), { code: "busy" }); } });
  const exit = await main(["--to", "check", "--unit", "cargo:rightkit-one", "--commit", f.commit], h.io);
  const log = h.lines.join("\n");
  assert.equal(h.taken.length, 1, log);
  assert.deepEqual(h.released, h.taken, log);
  assert.equal(exit, 1, log);
});
