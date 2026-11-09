import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { classifyStagedFiles, installTauriAuthenticode } from "./install-tauri-authenticode.mjs";
import { endRun, startRun } from "./gate-stages.mjs";
import { treeEntries } from "./snapshot.mjs";
import { SOURCE_IDENTITY_FILE, sourceIdentityBytes } from "./snapshot-identity.mjs";

const tracked = "app/src-tauri/binaries/tracked.exe";
const ignored = "app/src-tauri/binaries/helper-x86_64-pc-windows-msvc.exe";
const unignored = "app/src-tauri/resources/unignored.exe";
const write = (file, text) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, text); };
const git = (cwd, ...args) => {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
};
function fixture(t) {
  const dir = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-authenticode-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q");
  git(repo, "config", "user.name", "RightKit test"); git(repo, "config", "user.email", "rightkit@example.invalid");
  write(path.join(repo, ".gitignore"), `/${SOURCE_IDENTITY_FILE}\n/app/src-tauri/binaries/*.exe\n`);
  write(path.join(repo, "app/package.json"), '{"name":"app"}\n');
  write(path.join(repo, tracked), "committed executable");
  write(path.join(repo, "app/src-tauri/tauri.conf.json"), JSON.stringify({ bundle: { externalBin: ["binaries/helper"] } }));
  git(repo, "add", "."); git(repo, "add", "-f", tracked); git(repo, "commit", "-qm", "selected");
  const commit = git(repo, "rev-parse", "HEAD"); const tree = git(repo, "rev-parse", "HEAD^{tree}");
  const snapshot = path.join(dir, "snapshot");
  fs.cpSync(repo, snapshot, { recursive: true, filter: (file) => file !== path.join(repo, ".git") });
  const stateFile = `${snapshot}.state.json`;
  const entries = treeEntries(repo, commit);
  const state = { schema: "rightkit.snapshot-state/1", repo, commit, tree, paths: entries.map((entry) => entry.path) };
  write(stateFile, JSON.stringify(state));
  write(path.join(snapshot, SOURCE_IDENTITY_FILE), sourceIdentityBytes({ checkout: repo, commit, entries }));
  for (const root of [repo, snapshot]) { write(path.join(root, ignored), "staged executable"); write(path.join(root, unignored), "unstaged executable"); }
  const appRoot = path.join(snapshot, "app"); const tauriDir = path.join(appRoot, "src-tauri");
  const classify = (files = [tracked, ignored, unignored], env = {}) => classifyStagedFiles(files.map((file) => path.join(snapshot, file)), { appRoot, tauriDir, env });
  return { dir, repo, snapshot, stateFile, state, commit, tree, appRoot, tauriDir, classify };
}
const refused = (run) => assert.throws(run, { code: "TAURI_AUTHENTICODE_TRACKED_RESOURCE" });

test("#1549: a nested git-free snapshot classifies the selected tree and commit ignore rules", (t) => {
  const f = fixture(t);
  assert.equal(fs.existsSync(path.join(f.snapshot, ".git")), false);
  assert.deepEqual([...f.classify().values()], ["tracked", "ignored", "unignored"]);
  // The checkout's current index and state's path cache must not make a selected-commit resource signable.
  git(f.repo, "rm", "--cached", tracked);
  write(f.stateFile, JSON.stringify({ ...f.state, paths: [] }));
  assert.deepEqual([...f.classify().values()], ["tracked", "ignored", "unignored"]);
});

test("#1549: ordinary checkouts retain index-based tracked, ignored and unignored classification", (t) => {
  const f = fixture(t);
  const files = [tracked, ignored, unignored].map((file) => path.join(f.repo, file));
  assert.deepEqual([...classifyStagedFiles(files, { appRoot: path.join(f.repo, "app"), env: {} }).values()], ["tracked", "ignored", "unignored"]);
});

test("#1549: missing, malformed or stale snapshot proof refuses", (t) => {
  const f = fixture(t); const identity = path.join(f.snapshot, SOURCE_IDENTITY_FILE);
  const original = fs.readFileSync(identity);
  fs.rmSync(identity); refused(() => f.classify());
  write(identity, original); write(f.stateFile, "{"); refused(() => f.classify());
  write(f.stateFile, JSON.stringify({ ...f.state, tree: "b".repeat(40) })); refused(() => f.classify());
  write(f.stateFile, JSON.stringify(f.state));
  write(identity, JSON.stringify({ schema: "rightkit.source-identity.v1", commit: "a".repeat(40), trees: { "": f.tree } }));
  refused(() => f.classify());
  write(identity, original); fs.rmSync(f.stateFile); refused(() => f.classify());
});

test("#1549: reset seam commit and legacy live source split agree with snapshot proof when supplied", (t) => {
  const f = fixture(t);
  assert.equal(f.classify([ignored], { RUSTRIGHT_GATE_COMMIT: f.commit }).values().next().value, "ignored");
  refused(() => f.classify([ignored], { RUSTRIGHT_GATE_COMMIT: "a".repeat(40) }));
  const source = { checkout: f.repo, snapshot: f.snapshot, commit: f.commit, tree: f.tree, appDir: "app" };
  const run = startRun({ commonDir: path.join(f.repo, ".git"), commit: f.commit, tree: f.tree, stage: "dev", source });
  t.after(() => endRun(run));
  assert.equal(f.classify([ignored], run.env).values().next().value, "ignored");
  const body = JSON.parse(fs.readFileSync(run.file, "utf8"));
  write(run.file, JSON.stringify({ ...body, source: { ...source, snapshot: f.repo } }));
  refused(() => f.classify([ignored], run.env));
});

test("#1549: signing sources cannot escape snapshot, follow links or share hard links", (t) => {
  const f = fixture(t); const outside = path.join(f.dir, "outside.exe"); write(outside, "outside");
  refused(() => classifyStagedFiles([outside], { appRoot: f.appRoot, env: {} }));
  const staged = path.join(f.snapshot, ignored);
  fs.rmSync(staged); fs.linkSync(outside, staged); refused(() => f.classify([ignored]));
  fs.rmSync(staged);
  // Windows file-symlink creation requires a provisioned privilege; directory junctions do not.
  if (process.platform !== "win32") {
    fs.symlinkSync(outside, staged); refused(() => f.classify([ignored])); fs.rmSync(staged);
  }
  fs.rmSync(path.dirname(staged), { recursive: true });
  const linkedDir = path.join(f.dir, "linked-binaries"); fs.mkdirSync(linkedDir); write(path.join(linkedDir, path.basename(staged)), "linked executable");
  fs.symlinkSync(linkedDir, path.dirname(staged), process.platform === "win32" ? "junction" : "dir");
  refused(() => f.classify([ignored]));
});

test("#1549: ignored resources still refuse before marker patch or signing when selected commit tracks them", (t) => {
  const f = fixture(t); const debugDir = path.join(f.dir, "output"); write(path.join(debugDir, "App.exe"), "raw app");
  write(path.join(f.tauriDir, "tauri.conf.json"), JSON.stringify({ bundle: { resources: ["binaries/tracked.exe"] } }));
  let effects = 0;
  assert.throws(() => installTauriAuthenticode({
    descriptor: { kind: "tauri-authenticode", productName: "App", targetTriple: "x86_64-pc-windows-msvc", manifestPath: "src-tauri/Cargo.toml", expectedInstalledPath: "App.exe" },
    debugDir, appRoot: f.appRoot,
    deps: { classifyStaged: (files, options) => classifyStagedFiles(files, { ...options, env: {} }), patchMarker: () => { effects++; }, signWindows: () => { effects++; } },
  }), (error) => error.code === "TAURI_AUTHENTICODE_TRACKED_RESOURCE" && /is tracked by git/.test(error.message));
  assert.equal(effects, 0);
  assert.equal(fs.readFileSync(path.join(f.snapshot, tracked), "utf8"), "committed executable");
});
