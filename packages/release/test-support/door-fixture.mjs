// A real door on a simulated Linux host for tests of the stage loop (audit gate-broker-2026-10-30): real git, a real snapshot sync into the
// test's own root, real build ownership, an in-memory receipt store. Everything that would compile, seal or reach a broker is a seam the
// caller passes to `door(args, extra)`.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "../background-process.mjs";
import { main } from "../bin/rightkit-gate.mjs";

export function tmpDir(t, prefix) {
  const dir = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

export function doorFixture(t, files = { "README.md": "committed\n" }, prefix = "rk-door-fx-") {
  const dir = tmpDir(t, prefix);
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  const git = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@t"); git("config", "user.name", "t");
  const write = (rel, body) => { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); };
  for (const [rel, body] of Object.entries(files)) write(rel, body);
  git("add", "-A"); git("commit", "-q", "-m", "c");
  const commit = () => git("rev-parse", "HEAD").stdout.trim();
  const snaps = path.join(dir, "snaps");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : c === "pnpm" && a[0] === "--version" ? { status: 0, stdout: "11.25.0\n", stderr: "" } : { status: 1, stdout: "", stderr: "not in this fixture" });
  const receipts = new Map();
  const door = async (args, extra = {}) => {
    const out = []; const err = [];
    const code = await main(args, {
      cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0,
      runCheck: async () => 0, runCompile: async () => 0, readReceipt: ({ commit: c }) => receipts.get(c) ?? null, writeReceipt: ({ receipt }) => receipts.set(receipt.commit, structuredClone(receipt)),
      resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), ...extra,
    });
    return { code, out: out.join("\n"), err: err.join("\n"), all: [...out, ...err].join("\n") };
  };
  const snapshot = () => path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".") && fs.statSync(path.join(snaps, n)).isDirectory()));
  return { dir, repo, snaps, env, spawn, receipts, door, snapshot, write, git: (...a) => git(...a), commit };
}
