// Fixtures for snapshot-aware Class A and live-scans (snapshot-check.mjs; joint plan step 2; rightkit#1257). Each names what it
// holds: E17 (the selected commit is checked whatever the checkout's HEAD and dirt; a mid-check snapshot change refuses), E18 (live
// scans answer the broker's parser; a live-scan failure is never a reusable red), §1 (committed-but-ignored inputs are bound; no
// digest reads the live checkout), and decision A (offline install from the primary checkout's recorded store).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { defaultSpawn, isLiveScan, LIVE_SCAN_IDS } from "./deterministic-scans.mjs";
import { repoKey, keyPath, syncSnapshot } from "./snapshot.mjs";
import {
  CheckError, DETERMINISTIC_MARKER, LIVE_SCANS_SCHEMA, allowedBuilds, deterministicMarker, installSnapshotDependencies, liveScansCommand,
  modulesStoreDir, pinnedPnpm, resolvePrimaryStore, snapshotOption,
} from "./snapshot-check.mjs";
import { main, discoverPnpmRoots, discoverNpmUnits } from "./bin/rightkit-gate.mjs";

const emptyConfig = path.join(fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sc-cfg-"))), "gitconfig");
fs.writeFileSync(emptyConfig, "");
Object.assign(process.env, { GIT_CONFIG_GLOBAL: emptyConfig, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" });

function git(cwd, ...args) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}
function tmp(t, prefix) {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(d, { recursive: true, force: true }));
  return d;
}
function write(root, files) {
  for (const [name, body] of Object.entries(files)) { const p = path.join(root, ...name.split("/")); fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, body); }
}
const commitAll = (repo, m = "c") => { git(repo, "add", "-A"); git(repo, "commit", "-q", "--allow-empty", "-m", m); return git(repo, "rev-parse", "HEAD"); };

/** A repository whose selected commit A holds a committed-but-ignored file; HEAD (B) is ahead of A and the checkout is dirty. */
function scenario(t) {
  const repo = tmp(t, "rk-sc-repo-");
  git(repo, "init", "-q", "-b", "main");
  write(repo, { ".gitignore": "*.log\nnode_modules/\n", "README.md": "commit A\n", "src/a.txt": "a\n", "kept.log": "committed despite *.log\n" });
  git(repo, "add", "-A"); git(repo, "add", "-f", "kept.log");
  git(repo, "commit", "-q", "-m", "A");
  const a = git(repo, "rev-parse", "HEAD");
  write(repo, { "README.md": "commit B, ahead of A\n" });
  commitAll(repo, "B");
  write(repo, { "src/a.txt": "uncommitted edit\n", "dirty.txt": "untracked\n" });
  const root = tmp(t, "rk-sc-snaps-");
  const snap = syncSnapshot({ checkout: repo, rev: a, root });
  return { repo, a, tree: git(repo, "rev-parse", `${a}^{tree}`), dir: snap.path, root };
}

/** The gate's io for a snapshot check: real git, pnpm --version faked, every git call recorded, app rules and scans stubbed. */
function io(t, s, { scans = [], onScans = null } = {}) {
  const calls = [];
  const spawn = (cmd, args, opts = {}) => {
    if (cmd === "git") calls.push(args.join(" "));
    if (cmd === "pnpm" && args[0] === "--version") return { status: 0, stdout: "11.25.0\n", stderr: "" };
    return defaultSpawn(cmd, args, opts);
  };
  const out = []; const err = [];
  const plans = [];
  return {
    calls, out, err, plans,
    io: {
      cwd: s.repo, env: { ...process.env, RK_GATE_CACHE: "0" }, spawn, out: (m) => out.push(m), err: (m) => err.push(m),
      appCheck: async () => 0, classA: undefined, selfPath: path.join(tmp(t, "rk-sc-self-"), "rightkit-gate.mjs"),
      resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }),
      runScans: async (plan) => { plans.push(plan); if (onScans) onScans(); return { scans, notes: [], cacheHits: [] }; },
    },
  };
}
const bytesLine = (out) => out.find((l) => / class A bytes /.test(l));
const commitDigest = (repo, commit) => {
  const lines = git(repo, "ls-tree", "-r", "--full-tree", commit).split("\n").filter(Boolean).map((l) => { const [meta, f] = l.split("\t"); return [f, meta.split(" ")[2]]; }).sort(([x], [y]) => (x < y ? -1 : x > y ? 1 : 0));
  return crypto.createHash("sha256").update(lines.map(([f, id]) => `${f}\t${id}\n`).join("")).digest("hex");
};

test("E17: a dirty checkout whose HEAD is ahead still checks the selected commit's snapshot, never reading HEAD or status", async (t) => {
  const s = scenario(t);
  const run = io(t, s);
  const code = await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io);
  assert.equal(code, 0, run.err.join("\n"));
  assert.ok(run.out.some((l) => l.includes(`is commit ${s.a} tree ${s.tree} (verified before the check)`)));
  assert.ok(run.out.some((l) => l.includes(`is still tree ${s.tree} after the check`)));
  assert.ok(run.out.some((l) => /class A full pass/.test(l)));
  for (const c of run.calls) {
    assert.doesNotMatch(c, /^status\b/, `snapshot mode ran git ${c}`);
    assert.doesNotMatch(c, /(^| )HEAD(\^|$| )/, `snapshot mode read HEAD: git ${c}`);
  }
});

test("§1: the bytes the verdict names are the commit's, committed-but-ignored file included, whatever the checkout holds", async (t) => {
  const s = scenario(t);
  const first = io(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], first.io), 0, first.err.join("\n"));
  const expected = commitDigest(s.repo, s.a);
  assert.match(bytesLine(first.out), new RegExp(`tracked=${expected} all=${expected} `));
  assert.ok(git(s.repo, "ls-tree", "-r", "--name-only", s.a).split("\n").includes("kept.log"));
  // Other live dirt, same verdict bytes.
  write(s.repo, { "src/a.txt": "a different uncommitted edit\n", "more.log": "x\n" });
  const second = io(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], second.io), 0, second.err.join("\n"));
  assert.equal(bytesLine(second.out).split(" env-at-start=")[0], bytesLine(first.out).split(" env-at-start=")[0]);
  // The plan scans the snapshot: both its content and live roots are the snapshot directory, never the checkout.
  const roots = first.plans[0].roots;
  assert.equal(fs.realpathSync.native(roots.contentRoot), fs.realpathSync.native(s.dir));
  assert.equal(fs.realpathSync.native(roots.liveRoot), fs.realpathSync.native(s.dir));
  assert.equal(roots.cargoLiveRoot, undefined, "no Cargo manifest, no checkout cargo root");
});

test("E17: a snapshot changed while class A runs refuses with RK_CHECK_ERROR, exit 2", async (t) => {
  const s = scenario(t);
  const run = io(t, s, { onScans: () => fs.appendFileSync(path.join(s.dir, "src", "a.txt"), "tampered\n") });
  const code = await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io);
  assert.equal(code, 2);
  assert.ok(run.err.some((l) => /RK_CHECK_ERROR: snapshot .* is tree [0-9a-f]+ after class A, not tree/.test(l)), run.err.join("\n"));
});

test("E17: a snapshot that is not the commit's tree refuses before anything runs", async (t) => {
  const s = scenario(t);
  write(s.dir, { "planted.txt": "not in the commit\n" });
  const run = io(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io), 2);
  assert.ok(run.err.some((l) => /RK_CHECK_ERROR: snapshot .* before the check, not tree/.test(l)), run.err.join("\n"));
  assert.equal(run.plans.length, 0, "no scan planned");
});

test("snapshot option: --commit and --full-a required, absolute real directory, no overlap, never with --worktree", (t) => {
  const s = scenario(t);
  const opt = (args) => () => snapshotOption(args, { checkout: s.repo });
  assert.throws(opt(["check", "--full-a", "--snapshot", s.dir]), CheckError);
  assert.throws(opt(["check", "--commit", s.a, "--snapshot", s.dir]), CheckError);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", "relative/dir"]), /absolute/);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", path.join(s.root, "missing")]), /does not exist/);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", path.join(s.repo, "src")]), /overlap/);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--worktree", "--snapshot", s.dir]), /working tree/);
  if (process.platform !== "win32") {
    const link = path.join(s.root, "link"); fs.symlinkSync(s.dir, link);
    assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", link]), /not a directory/);
  }
  assert.equal(opt(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir])(), fs.realpathSync.native(s.dir));
});

test("rightkit#1257: a red of deterministic failures prints the marker with env=; a live-scan or undecided failure prints none", async (t) => {
  const s = scenario(t);
  const red = (rows) => io(t, s, { scans: rows });
  const det = red([{ id: "node-check:a.mjs", class: "A", status: "fail", code: "EXIT", detail: "x" }, { id: "advisory-exceptions", class: "A", status: "fail", detail: "expired" }]);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], det.io), 1);
  const marker = det.out.find((l) => l.startsWith(DETERMINISTIC_MARKER));
  assert.match(marker ?? "", /^rightkit-gate: verdict deterministic failed=node-check:a\.mjs,advisory-exceptions env=[0-9a-f]{64}$/);
  for (const rows of [
    [{ id: "node-check:a.mjs", class: "A", status: "fail", code: "EXIT" }, { id: "pnpm-audit", class: "A", status: "fail", code: "EXIT" }],
    [{ id: "pnpm-audit:apps/x", class: "A", status: "fail", code: "EXIT" }],
    [{ id: "cargo-deny", class: "A", status: "fail", code: "TIMEOUT" }],
    [{ id: "actionlint", class: "A", status: "fail", code: "TOOL_MISSING" }],
    [{ id: "generated-docs", class: "A", status: "deferred", detail: "trusted root" }],
    [{ id: "tsc:x", class: "A", status: "not-run", code: "CANCELLED" }],
  ]) {
    const r = red(rows);
    assert.notEqual(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], r.io), 0);
    assert.equal(r.out.find((l) => l.startsWith(DETERMINISTIC_MARKER)), undefined, JSON.stringify(rows));
  }
});

test("deterministicMarker: unit rules", () => {
  const env = "e".repeat(64);
  assert.equal(deterministicMarker({ scans: [{ id: "a", status: "pass" }] }, env), null);
  assert.equal(deterministicMarker({ scans: [{ id: "a", status: "fail", code: "EXIT" }] }, null), null);
  assert.equal(deterministicMarker({ scans: [{ id: "a", status: "fail", code: "NO_TESTS" }, { id: "b", status: "pass" }] }, env), `${DETERMINISTIC_MARKER} failed=a env=${env}`);
  for (const code of ["SPAWN_ERROR", "TOOL_VERSION", "ADVISORY_DB", "FETCH", "QUIET_LEASE_UNAVAILABLE", "SCAN_ERROR", "DENY_ERROR", "CLASS_A_REQUIRED"]) {
    assert.equal(deterministicMarker({ scans: [{ id: "a", status: "fail", code }] }, env), null, code);
  }
  assert.deepEqual(LIVE_SCAN_IDS, ["pnpm-audit"]);
  assert.equal(isLiveScan("pnpm-audit"), true); assert.equal(isLiveScan("pnpm-audit:apps/web"), true);
  assert.equal(isLiveScan("pnpm-auditx"), false); assert.equal(isLiveScan("pnpm-lockfile"), false);
});

test("env-digest --snapshot <dir> --commit <sha> equals the env the snapshot check's pass line names", async (t) => {
  const s = scenario(t);
  const run = io(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io), 0, run.err.join("\n"));
  const env = / env=([0-9a-f]{64})$/.exec(bytesLine(run.out))[1];
  const dig = io(t, s);
  assert.equal(await main(["env-digest", "--snapshot", s.dir, "--commit", s.a], dig.io), 0, dig.err.join("\n"));
  assert.equal(dig.out.at(-1), `env=${env}`);
  write(s.dir, { "planted.txt": "x\n" });
  const bad = io(t, s);
  assert.equal(await main(["env-digest", "--snapshot", s.dir, "--commit", s.a], bad.io), 2);
  assert.match(bad.err.at(-1), /RK_CHECK_ERROR/);
});

// ---- decision A: offline install from the primary checkout's recorded store

test("store: the storeDir the checkout's own install recorded wins; else `pnpm store path` in the checkout; else RK_CHECK_ERROR", (t) => {
  const dir = tmp(t, "rk-sc-store-");
  assert.equal(modulesStoreDir('{\n  "storeDir": "/Volumes/D/claude/.pnpm-store/v11",\n}'), "/Volumes/D/claude/.pnpm-store/v11");
  assert.equal(modulesStoreDir("storeDir: /srv/pnpm-store/v11\nnodeLinker: isolated"), "/srv/pnpm-store/v11");
  const checkout = tmp(t, "rk-sc-co-");
  write(checkout, { "node_modules/.modules.yaml": `{\n  "storeDir": ${JSON.stringify(dir)},\n}\n` });
  const never = () => { throw new Error("pnpm store path must not run when the install recorded a store"); };
  assert.deepEqual(resolvePrimaryStore({ checkout, spawn: never }), { storeDir: dir, source: "node_modules/.modules.yaml" });
  const bare = tmp(t, "rk-sc-bare-");
  const asked = [];
  const r = resolvePrimaryStore({ checkout: bare, spawn: (c, a, o) => { asked.push([c, a, o.cwd]); return { status: 0, stdout: `${dir}\n` }; } });
  assert.deepEqual(r, { storeDir: dir, source: "pnpm store path (primary checkout)" });
  assert.deepEqual(asked, [["pnpm", ["store", "path"], path.join(bare, ".")]]);
  assert.throws(() => resolvePrimaryStore({ checkout: bare, spawn: () => ({ status: 0, stdout: "/nonexistent/store\n" }) }), (e) => e instanceof CheckError && /no existing pnpm store/.test(e.message));
});

test("install: pinned pnpm, --offline --frozen-lockfile --ignore-scripts --ignore-pnpmfile into the snapshot from the recorded store", (t) => {
  const store = tmp(t, "rk-sc-store-");
  const checkout = tmp(t, "rk-sc-co-");
  write(checkout, { "node_modules/.modules.yaml": `"storeDir": ${JSON.stringify(store)}\n` });
  const snapshot = tmp(t, "rk-sc-snap-");
  write(snapshot, { "package.json": JSON.stringify({ packageManager: "pnpm@11.25.0" }), "pnpm-lock.yaml": "lockfileVersion: '9.0'\n", "pnpm-workspace.yaml": "packages:\n  - packages/*\nallowBuilds:\n  # note\n  canvas: false\n  esbuild: true\n" });
  const calls = [];
  const ok = (c, a, o) => { calls.push({ c, a, cwd: o.cwd }); return a[0] === "--version" ? { status: 0, stdout: "11.25.0\n" } : { status: 0, stdout: "Done\n" }; };
  let clock = 1000;
  const [rec] = installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: ok, now: () => (clock += 250) });
  const install = calls.find((x) => x.a[0] === "install");
  assert.deepEqual(install.a, ["install", "--offline", "--frozen-lockfile", "--ignore-scripts", "--ignore-pnpmfile", "--store-dir", store, "--reporter", "append-only"]);
  assert.equal(install.cwd, path.join(snapshot, "."));
  assert.equal(rec.storeDir, store);
  assert.equal(rec.pnpm, "11.25.0");
  assert.equal(rec.lockfileSha256, crypto.createHash("sha256").update("lockfileVersion: '9.0'\n").digest("hex"));
  assert.equal(rec.ms, 250);
  assert.deepEqual(rec.allowedBuildsNotRun, ["esbuild"]);
  // A pnpm that is not the pinned one refuses.
  assert.throws(() => installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: (c, a) => (a[0] === "--version" ? { status: 0, stdout: "10.0.0\n" } : { status: 0 }) }), /pins pnpm@11\.25\.0/);
  // A store miss names the package and the root, never a skip.
  const miss = (c, a) => (a[0] === "--version" ? { status: 0, stdout: "11.25.0\n" } : { status: 1, stdout: "", stderr: "[ERR_PNPM_NO_OFFLINE_TARBALL] A package is missing from the store but cannot download it in offline mode. The missing package may be downloaded from https://registry.npmjs.org/katex/-/katex-0.18.11.tgz." });
  assert.throws(() => installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: miss }), (e) => e instanceof CheckError && /root \.: katex-0\.18\.11 is missing from the store/.test(e.message));
  // No lockfile: nothing to install from.
  fs.rmSync(path.join(snapshot, "pnpm-lock.yaml"));
  assert.throws(() => installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: ok }), /no pnpm-lock\.yaml/);
});

test("pins and allowed builds parse the repository's own forms", (t) => {
  const d = tmp(t, "rk-sc-pin-");
  write(d, { "package.json": JSON.stringify({ packageManager: "pnpm@11.25.0+sha512.abc", pnpm: { onlyBuiltDependencies: ["sharp"] } }), "pnpm-workspace.yaml": "onlyBuiltDependencies:\n  - 'better-sqlite3'\nallowBuilds:\n  esbuild: true\n  canvas: false\n" });
  assert.equal(pinnedPnpm(d), "11.25.0");
  assert.deepEqual(allowedBuilds(d), ["better-sqlite3", "esbuild", "sharp"]);
});

// ---- live-scans: the broker's refresh (E18)

/** A JavaScript transcription of rustright verdict_reuse.rs parse_live_scans (same checks, same order). */
function parseLiveScans(exitCode, output) {
  const line = output.split("\n").reverse().find((l) => l.trimStart().startsWith("{"));
  if (!line) throw new Error("no JSON line");
  const answer = JSON.parse(line.trim());
  if (answer.schema !== LIVE_SCANS_SCHEMA) throw new Error("schema");
  if (!Array.isArray(answer.scans)) throw new Error("no scans array");
  const refreshed = answer.scans.map((s) => {
    if (typeof s.id !== "string" || !s.id) throw new Error("no id");
    if (s.status !== "passed" && s.status !== "failed") throw new Error("status");
    return { id: s.id, passed: s.status === "passed" };
  });
  if ((exitCode === 0) !== refreshed.every((s) => s.passed)) throw new Error("exit contradicts statuses");
  if (exitCode !== 0 && exitCode !== 1) throw new Error("exit");
  return refreshed;
}

test("E18 live-scans: one JSON line the broker's parser accepts; exit 0 only when every live scan passed; only live scans run", async (t) => {
  const repo = tmp(t, "rk-sc-live-");
  write(repo, { "package.json": "{}", "pnpm-lock.yaml": "lockfileVersion: '9.0'\n", "apps/web/package.json": "{}", "apps/web/pnpm-lock.yaml": "lockfileVersion: '9.0'\n" });
  const out = []; const err = [];
  const o = { cwd: repo, spawn: defaultSpawn, readFile: (f) => fs.readFileSync(f, "utf8"), exists: fs.existsSync, platform: process.platform, out: (m) => out.push(m), err: (m) => err.push(m) };
  let ran = null;
  const deps = (statusOf) => ({ discoverPnpmRoots, discoverNpmUnits, run: async (plan) => { ran = plan.scans.map((s) => s.id); return { scans: plan.scans.map((s) => ({ id: s.id, status: statusOf(s.id), detail: "advisory GHSA-x" })) }; } });
  assert.equal(await liveScansCommand(o, ["--repo", repo], deps(() => "pass")), 0);
  assert.deepEqual(ran.sort(), ["pnpm-audit", "pnpm-audit:apps/web"]);
  assert.deepEqual(parseLiveScans(0, out.join("\n")).sort((x, y) => (x.id < y.id ? -1 : 1)), [{ id: "pnpm-audit", passed: true }, { id: "pnpm-audit:apps/web", passed: true }]);
  out.length = 0;
  assert.equal(await liveScansCommand(o, ["--repo", repo], deps((id) => (id === "pnpm-audit:apps/web" ? "fail" : "pass"))), 1);
  const parsed = parseLiveScans(1, out.join("\n"));
  assert.equal(parsed.find((s) => s.id === "pnpm-audit:apps/web").passed, false);
  assert.ok(err.some((l) => /live scan pnpm-audit:apps\/web failed: advisory GHSA-x/.test(l)));
  // A scan that could not run (not-run, timeout) is failed, never passed.
  out.length = 0;
  assert.equal(await liveScansCommand(o, ["--repo", repo], deps(() => "not-run")), 1);
  assert.ok(parseLiveScans(1, out.join("\n")).every((s) => !s.passed));
  assert.equal(await liveScansCommand(o, ["--repo"], deps(() => "pass")), 2);
  assert.equal(await liveScansCommand(o, ["--repo", repo, "--extra"], deps(() => "pass")), 2);
});

test("E18 live-scans through the gate entry: dispatch and usage", async (t) => {
  const repo = tmp(t, "rk-sc-live2-");
  const out = []; const err = [];
  // No pnpm root: nothing live to re-run, an empty list the parser accepts with exit 0.
  assert.equal(await main(["live-scans", "--repo", repo], { cwd: repo, out: (m) => out.push(m), err: (m) => err.push(m) }), 0, err.join("\n"));
  assert.deepEqual(parseLiveScans(0, out.join("\n")), []);
  assert.equal(await main(["live-scans"], { cwd: repo, out: (m) => out.push(m), err: (m) => err.push(m) }), 2);
});
