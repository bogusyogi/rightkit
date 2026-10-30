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
  CheckError, DETERMINISTIC_MARKER, commitFiles, commitOf, describeInstall, probeSkippedBuilds, LIVE_SCANS_SCHEMA, allowedBuilds, deterministicMarker, installSnapshotDependencies, liveScansCommand,
  modulesStoreDir, pinnedPnpm, resolvePrimaryStore, snapshotOption, verifySnapshotDir,
} from "./snapshot-check.mjs";
import { main, discoverPnpmRoots, discoverNpmUnits } from "./bin/rightkit-gate.mjs";
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

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
function scenario(t, extra = {}) {
  const repo = tmp(t, "rk-sc-repo-");
  git(repo, "init", "-q", "-b", "main");
  write(repo, { ".gitignore": "*.log\nnode_modules/\n", "README.md": "commit A\n", "src/a.txt": "a\n", "kept.log": "committed despite *.log\n", ...extra });
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
function io(t, s, { scans = [], onScans = null, fakeInstall = false } = {}) {
  const calls = [];
  // The environment binding walks every file of the gate package; the live package can be written by other suites of a full run while
  // this nested gate runs. The fixture is a private stand-in package, so only this test's own writes can move the binding.
  const gatePackage = tmp(t, "rk-sc-gatepkg-");
  write(gatePackage, { "rightkit-gate.stub.mjs": "// stand-in gate package for the environment binding\n" });
  const store = fakeInstall ? tmp(t, "rk-sc-fake-store-") : null;
  const spawn = (cmd, args, opts = {}) => {
    if (cmd === "git") calls.push(args.join(" "));
    if (cmd === "pnpm" && args[0] === "--version") return { status: 0, stdout: "11.25.0\n", stderr: "" };
    // A root with a lockfile is installed into the snapshot; the install itself is faked (no network, no store) so the test owns node_modules.
    if (fakeInstall && cmd === "pnpm" && args[0] === "store") return { status: 0, stdout: `${store}\n`, stderr: "" };
    if (fakeInstall && cmd === "pnpm" && args[0] === "install") return { status: 0, stdout: "", stderr: "" };
    return defaultSpawn(cmd, args, opts);
  };
  const out = []; const err = [];
  const plans = [];
  return {
    calls, out, err, plans,
    io: {
      // The host-wide ownership record lives beside the injected root, in a fixture directory of its own (never the host's).
      cwd: s.repo, gatePackage, env: { ...process.env, RK_GATE_CACHE: "0" }, snapshotRoot: path.join(tmp(t, "rk-sc-own-"), "snapshots"), spawn, out: (m) => out.push(m), err: (m) => err.push(m),
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
  assert.ok(run.err.some((l) => /RK_CHECK_ERROR: snapshot .* is not exactly commit [0-9a-f]+ before the check: planted\.txt is not in the commit's tree/.test(l)), run.err.join("\n"));
  assert.equal(run.plans.length, 0, "no scan planned");
});

// Review item5-r1 finding 1 (E6, GATE-CONTRACT §1): the seeded tree id is blind to ignored files and to links, so the stage refuses on its own walk.
test("E6: an ignored file planted before the check, or while class A runs, refuses the stage (RK_CHECK_ERROR, exit 2)", async (t) => {
  const s = scenario(t);
  write(s.dir, { "target/debug/evil.rs": "ignored? no: target/ is not even an ignore rule here\n", "stale.log": "ignored by *.log\n" });
  const before = io(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], before.io), 2);
  assert.ok(before.err.some((l) => /RK_CHECK_ERROR: snapshot .* before the check: (target|stale\.log)/.test(l)), before.err.join("\n"));
  assert.equal(before.plans.length, 0, "no scan planned");
  fs.rmSync(path.join(s.dir, "target"), { recursive: true }); fs.rmSync(path.join(s.dir, "stale.log"));
  const during = io(t, s, { onScans: () => write(s.dir, { "build.log": "planted while class A ran; *.log is ignored\n" }) });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], during.io), 2);
  assert.ok(during.err.some((l) => /RK_CHECK_ERROR: snapshot .* after class A: build\.log is not in the commit's tree/.test(l)), during.err.join("\n"));
});

test("E6 verifySnapshotDir: a planted ignored file, a link or junction into the checkout or out of the snapshot, refuse naming the path; the commit's own dir passes", (t) => {
  const s = scenario(t);
  const check = () => verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit: s.a, tree: s.tree, when: "unit" });
  assert.equal(check(), s.tree);
  const refuses = (name, why) => assert.throws(check, (e) => e instanceof CheckError && e.message.includes(name) && (!why || why.test(e.message)), name);
  write(s.dir, { "stale.log": "ignored\n" });
  refuses("stale.log");
  fs.rmSync(path.join(s.dir, "stale.log"));
  const linkType = process.platform === "win32" ? "junction" : "dir";
  const outside = tmp(t, "rk-sc-outside-");
  fs.mkdirSync(path.join(s.repo, "outputs"));
  for (const [to, why] of [[path.join(s.repo, "outputs"), /into the editable checkout/], [outside, /outside the snapshot/]]) {
    fs.symlinkSync(to, path.join(s.dir, "out"), linkType);
    refuses("out");
    refuses("out", why);
    fs.rmSync(path.join(s.dir, "out"), { recursive: true, force: true });
  }
  assert.equal(check(), s.tree);
});

test("E6 verifySnapshotDir: only the named pnpm dependency directories and the config-only .git/config are allowed extras", (t) => {
  const s = scenario(t, { "package.json": "{\"name\":\"fixture\",\"private\":true}\n", "pnpm-lock.yaml": "lockfileVersion: '9.0'\n" });
  const check = () => verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit: s.a, tree: s.tree, when: "unit" });
  // The only .git/config allowed is byte-equal to what the gate writes for THIS checkout's origin (item 5 row 5.09).
  git(s.repo, "remote", "add", "origin", "git@example.com:o/r.git");
  write(s.dir, { "node_modules/.pnpm/dep@1.0.0/node_modules/dep/index.js": "x\n", ".git/config": '[remote "origin"]\n\turl = git@example.com:o/r.git\n' });
  assert.equal(check(), s.tree);
  write(s.dir, { ".git/config": '[remote "origin"]\n\turl = ext::sh -c id\n' });
  assert.throws(check, (e) => e instanceof CheckError && /\.git\/config/.test(e.message));
  write(s.dir, { ".git/config": '[remote "origin"]\n\turl = git@example.com:o/r.git\n' });
  write(s.dir, { "node_modules/dep/index.js": "x\n" });
  assert.equal(check(), s.tree);
  write(s.dir, { "dist/bundle.js": "a build output inside the snapshot\n" });
  assert.throws(check, (e) => e instanceof CheckError && /dist/.test(e.message));
  fs.rmSync(path.join(s.dir, "dist"), { recursive: true });
  const outside = tmp(t, "rk-sc-outside-");
  fs.symlinkSync(outside, path.join(s.dir, "node_modules", "leak"), process.platform === "win32" ? "junction" : "dir");
  assert.throws(check, (e) => e instanceof CheckError && /node_modules\/leak is a link resolving outside the snapshot/.test(e.message));
});

test("snapshot option: --commit required (--full-a optional), absolute real directory, no overlap, never with --worktree", (t) => {
  const s = scenario(t);
  const opt = (args) => () => snapshotOption(args, { checkout: s.repo });
  assert.throws(opt(["check", "--full-a", "--snapshot", s.dir]), CheckError);
  assert.equal(opt(["check", "--commit", s.a, "--snapshot", s.dir])(), fs.realpathSync.native(s.dir), "the content checks of a snapshot (plain check --commit) need no --full-a");
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", "relative/dir"]), /absolute/);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", path.join(s.root, "missing")]), /does not exist/);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", path.join(s.repo, "src")]), /overlap/);
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--worktree", "--snapshot", s.dir]), /working tree/);
  if (process.platform !== "win32") {
    const link = path.join(s.root, "link"); fs.symlinkSync(s.dir, link);
    assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", link]), /not a directory/);
  }
  assert.equal(opt(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir])(), fs.realpathSync.native(s.dir));
  // A directory of the checkout whose name starts with `..` is inside the checkout, not a way out of it (review item5-r2 #4).
  fs.mkdirSync(path.join(s.repo, "..x"));
  assert.throws(opt(["check", "--commit", s.a, "--full-a", "--snapshot", path.join(s.repo, "..x")]), /overlap/);
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
  for (const code of ["SPAWN_ERROR", "TOOL_VERSION", "ADVISORY_DB", "FETCH", "QUIET_LEASE_UNAVAILABLE", "SCAN_ERROR", "DENY_ERROR", "CLASS_A_REQUIRED", "AUX_METADATA"]) {
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
  // Dell 2026-10-29: pnpm records a Windows store as an escaped JSON string.
  assert.equal(modulesStoreDir('  "storeDir": "D:\\\\.dev-cache\\\\pnpm-store\\\\v11",\n  "virtualStoreDir": "D:\\\\Claude\\\\rightkit\\\\node_modules\\\\.pnpm",'), "D:\\.dev-cache\\pnpm-store\\v11");
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
  assert.deepEqual(install.a, ["install", "--offline", "--frozen-lockfile", "--ignore-scripts", "--ignore-pnpmfile", "--store-dir", store, "--virtual-store-dir", "node_modules/.pnpm", "--config.enable-global-virtual-store=false", "--reporter", "append-only"]);
  assert.equal(install.cwd, path.join(snapshot, "."));
  assert.equal(rec.storeDir, store);
  assert.equal(rec.pnpm, "11.25.0");
  assert.equal(rec.lockfileSha256, crypto.createHash("sha256").update("lockfileVersion: '9.0'\n").digest("hex"));
  assert.equal(rec.ms, 250);
  assert.deepEqual(rec.allowedBuildsNotRun, [], "esbuild is allowed to build but not installed here, so nothing is required of it");
  // The version is the one pnpm runs in the root (it switches to the packageManager pin there), not a neutral directory's.
  const switching = (c, a, o) => (a[0] === "--version" ? { status: 0, stdout: o.cwd === path.join(snapshot, ".") ? "11.25.0\n" : "11.24.0\n" } : { status: 0, stdout: "Done\n" });
  assert.equal(installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: switching })[0].pnpm, "11.25.0");
  // A pnpm that is not the pinned one refuses.
  assert.throws(() => installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: (c, a) => (a[0] === "--version" ? { status: 0, stdout: "10.0.0\n" } : { status: 0 }) }), /pins pnpm@11\.25\.0/);
  // A store miss names the package and the root, never a skip.
  const miss = (c, a) => (a[0] === "--version" ? { status: 0, stdout: "11.25.0\n" } : { status: 1, stdout: "", stderr: "[ERR_PNPM_NO_OFFLINE_TARBALL] A package is missing from the store but cannot download it in offline mode. The missing package may be downloaded from https://registry.npmjs.org/katex/-/katex-0.18.11.tgz." });
  assert.throws(() => installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: miss }), (e) => e instanceof CheckError && /root \.: katex-0\.18\.11 is missing from the store/.test(e.message));
  // No lockfile: nothing to install from; recorded as skipped (the pnpm-lockfile scan judges it), never an install.
  fs.rmSync(path.join(snapshot, "pnpm-lock.yaml"));
  calls.length = 0;
  const [none] = installSnapshotDependencies({ checkout, snapshot, roots: ["."], spawn: ok });
  assert.match(none.skipped, /no pnpm-lock\.yaml/);
  assert.equal(calls.length, 0);
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

test("rightkit#1215 in snapshot mode: a config-only .git naming the origin exists while class A runs and is gone after", async (t) => {
  const s = scenario(t);
  git(s.repo, "remote", "add", "origin", "git@github.com:example/repo.git");
  let during = null;
  const run = io(t, s, { onScans: () => { try { during = fs.readFileSync(path.join(s.dir, ".git", "config"), "utf8"); } catch { during = null; } } });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io), 0, run.err.join("\n"));
  assert.equal(during, '[remote "origin"]\n\turl = git@github.com:example/repo.git\n');
  assert.equal(fs.existsSync(path.join(s.dir, ".git")), false, "removed when the check ends");
});

test("skipped build scripts: an installed allowed-build package must prove itself usable (real node, under the class A launch guard), else RK_CHECK_ERROR", (t) => {
  const dir = tmp(t, "rk-sc-probe-");
  const pkg = (entry, name, files) => write(dir, Object.fromEntries(Object.entries(files).map(([f, b]) => [`node_modules/.pnpm/${entry}/node_modules/${name}/${f}`, b])));
  pkg("esbuild@0.28.2", "esbuild", { "package.json": JSON.stringify({ name: "esbuild", version: "0.28.2", bin: { esbuild: "bin/esbuild" } }), "bin/esbuild": "throw new Error('a compile tool must never be launched by the probe');\n" });
  pkg("nativelib@3.1.0", "nativelib", { "package.json": JSON.stringify({ name: "nativelib", version: "3.1.0", bin: { nativelib: "cli.js" } }), "cli.js": "console.log('3.1.0');\n" });
  pkg("@scope+native@1.0.0", "@scope/native", { "package.json": JSON.stringify({ name: "@scope/native", version: "1.0.0", main: "index.js" }), "index.js": "module.exports = 1;\n" });
  const ok = probeSkippedBuilds({ dir, root: ".", names: ["esbuild", "nativelib", "@scope/native", "not-installed"] });
  assert.deepEqual(ok.map(({ name, probe }) => [name, probe]), [["esbuild", "none"], ["nativelib", "nativelib --version"], ["@scope/native", "require"]]);
  assert.match(ok[0].result, /^compile tool \(esbuild\): class A never launches it/);
  const line = describeInstall({ root: ".", pnpm: "11.25.0", pinned: "11.25.0", storeDir: "/s", storeSource: "x", lockfileSha256: "0", ms: 1, allowedBuildsNotRun: ok.slice(0, 2) });
  assert.match(line, /build-scripts-not-run=esbuild@0\.28\.2\(deferred, not proven here: compile tool/);
  assert.match(line, /nativelib@3\.1\.0\(usable: nativelib --version -> 3\.1\.0\)/);
  assert.equal(ok[1].result, "3.1.0");
  pkg("broken@2.0.0", "broken", { "package.json": JSON.stringify({ name: "broken", version: "2.0.0", bin: "cli.js" }), "cli.js": "throw new Error('native binary missing: run the install script');\n" });
  assert.throws(() => probeSkippedBuilds({ dir, root: "apps/web", names: ["broken"] }), (e) => e instanceof CheckError && /root apps\/web: broken@2\.0\.0 needs its install script .* not usable without it: broken --version failed/.test(e.message));
});

// Coordinator review of 9799b15a: the probe runs before class A is green, so a dependency bin that launches cargo must be refused by the
// class A launch guard (real subprocesses, a fake cargo that would write a sentinel), and the probe fails with an actionable error.
test("skipped build scripts: a probed bin that launches cargo is refused by the launch guard; no compile starts; RK_CHECK_ERROR", (t) => {
  const dir = tmp(t, "rk-sc-guard-");
  const bin = tmp(t, "rk-sc-fakebin-");
  const sentinel = path.join(bin, "cargo-ran");
  if (process.platform === "win32") write(bin, { "cargo.cmd": `@echo ran> "${sentinel}"\r\n` });
  else { write(bin, { cargo: `#!/bin/sh\necho ran > '${sentinel}'\n` }); fs.chmodSync(path.join(bin, "cargo"), 0o755); }
  write(dir, {
    "node_modules/.pnpm/builder@1.0.0/node_modules/builder/package.json": JSON.stringify({ name: "builder", version: "1.0.0", bin: { builder: "cli.js" } }),
    "node_modules/.pnpm/builder@1.0.0/node_modules/builder/cli.js": "const r = require('node:child' + '_process').spawnSync('cargo', ['build'], { shell: true, stdio: 'inherit' }); process.exit(r.status ?? 0);\n",
  });
  // The test harness's broker guard (inherited through NODE_OPTIONS) is dropped for these two children: the control must really launch
  // the fake cargo, and the probe must be stopped by the class A launch guard alone. The sentinel proves what ran.
  const env = { ...process.env, NODE_OPTIONS: "", PATH: `${bin}${path.delimiter}${process.env.PATH}` };
  // Control: without the guard the same bin really launches the fake cargo.
  const control = spawnSync(process.execPath, [path.join(dir, "node_modules/.pnpm/builder@1.0.0/node_modules/builder/cli.js"), "--version"], { env, encoding: "utf8" });
  assert.equal(control.status, 0, control.stderr);
  assert.equal(fs.existsSync(sentinel), true, "the control run launched cargo");
  fs.rmSync(sentinel);
  assert.throws(() => probeSkippedBuilds({ dir, root: ".", names: ["builder"], env }), (e) => e instanceof CheckError && /builder@1\.0\.0 needs its install script .* builder --version failed: .*class A refuses a compile\/emit launch \(cargo\)/s.test(e.message));
  assert.equal(fs.existsSync(sentinel), false, "under the guard no cargo ran");
});

// Coordinator review of 4cda1824: a red's marker must bind only the environment it was judged in. A dependency that changes while
// class A runs (installed bytes under the snapshot's node_modules) voids the marker; the same red with nothing moved keeps it.
test("rightkit#1257: a dependency that changes while class A runs voids the deterministic marker (red) and the verdict (green)", async (t) => {
  const s = scenario(t, { "package.json": JSON.stringify({ name: "fixture", private: true }), "pnpm-lock.yaml": "lockfileVersion: '9.0'\n" });
  write(s.dir, { "node_modules/dep/package.json": JSON.stringify({ name: "dep", version: "1.0.0" }), "node_modules/dep/index.js": "module.exports = 1;\n" });
  const eligible = [{ id: "node-check:a.mjs", class: "A", status: "fail", code: "EXIT", detail: "x" }];
  const bump = () => write(s.dir, { "node_modules/dep/package.json": JSON.stringify({ name: "dep", version: "2.0.0" }), "node_modules/dep/index.js": "module.exports = 2;\n" });
  const drifted = io(t, s, { scans: eligible, onScans: bump, fakeInstall: true });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], drifted.io), 1);
  assert.equal(drifted.out.find((l) => l.startsWith(DETERMINISTIC_MARKER)), undefined, "no marker for a red judged across a dependency change");
  assert.ok(drifted.err.some((l) => /environment \(gate, tools or installed dependencies\) changed while class A ran.*No deterministic verdict is named/.test(l)), drifted.err.join("\n"));
  // The same red with nothing moving keeps its marker, bound to the environment it was judged in (dependency now at 2.0.0).
  const steady = io(t, s, { scans: eligible, fakeInstall: true });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], steady.io), 1);
  assert.match(steady.out.find((l) => l.startsWith(DETERMINISTIC_MARKER)) ?? "", /failed=node-check:a\.mjs env=[0-9a-f]{64}$/);
  // A green judged across a dependency change is refused.
  const green = io(t, s, { fakeInstall: true, onScans: () => write(s.dir, { "node_modules/dep/index.js": "module.exports = 3;\n" }) });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], green.io), 1);
  assert.equal(bytesLine(green.out), undefined, "no bytes/env line for a green whose environment moved");
});

test("rightkit#1257: a snapshot changed during a red run is RK_CHECK_ERROR with no marker", async (t) => {
  const s = scenario(t);
  const run = io(t, s, { scans: [{ id: "node-check:a.mjs", class: "A", status: "fail", code: "EXIT" }], onScans: () => fs.appendFileSync(path.join(s.dir, "README.md"), "x\n") });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io), 2);
  assert.equal(run.out.find((l) => l.startsWith(DETERMINISTIC_MARKER)), undefined);
  assert.ok(run.err.some((l) => /RK_CHECK_ERROR: snapshot .* after class A.*No deterministic verdict is named/.test(l)), run.err.join("\n"));
});

// Coordinator review of 4cda1824 (E18): the live-scans listing of a gate-owned snapshot (no git) dropped committed symlinked files, so a
// linked package.json's root got no audit and the answer was an empty, passing list.
test("E18 live-scans: a committed linked manifest's root is audited, its failure surfaces; an escaping link or a lockfile with no planned scan refuses", { skip: process.platform === "win32" ? "POSIX links" : false }, async (t) => {
  const root = tmp(t, "rk-sc-livelink-");
  write(root, { "data/web-manifest.json": JSON.stringify({ name: "web", private: true }), "apps/web/pnpm-lock.yaml": "lockfileVersion: '9.0'\n" });
  fs.symlinkSync("../../data/web-manifest.json", path.join(root, "apps", "web", "package.json"));
  const out = []; const err = [];
  const o = { cwd: root, spawn: defaultSpawn, readFile: (f) => fs.readFileSync(f, "utf8"), exists: fs.existsSync, platform: process.platform, out: (m) => out.push(m), err: (m) => err.push(m) };
  const run = async (plan) => ({ scans: plan.scans.map((s) => ({ id: s.id, status: "fail", detail: "GHSA-test advisory" })) });
  assert.equal(await liveScansCommand(o, ["--repo", root], { discoverPnpmRoots, discoverNpmUnits, run }), 1);
  assert.deepEqual(parseLiveScans(1, out.join("\n")), [{ id: "pnpm-audit:apps/web", passed: false }]);
  // A link out of the root stops the listing (exit 2), never an empty answer.
  const outside = tmp(t, "rk-sc-outside-");
  write(outside, { "x.json": "{}" });
  fs.symlinkSync(path.join(outside, "x.json"), path.join(root, "escape.json"));
  out.length = 0;
  assert.equal(await liveScansCommand(o, ["--repo", root], { discoverPnpmRoots, discoverNpmUnits, run }), 2);
  assert.equal(out.length, 0, "no JSON answer");
  fs.rmSync(path.join(root, "escape.json"));
  // A lockfile whose root cannot be discovered (no package.json beside it) is a refusal, not scans:[].
  const bare = tmp(t, "rk-sc-bare-live-");
  write(bare, { "tools/pnpm-lock.yaml": "lockfileVersion: '9.0'\n" });
  assert.equal(await liveScansCommand({ ...o, cwd: bare }, ["--repo", bare], { discoverPnpmRoots, discoverNpmUnits, run }), 2);
});

// Real subprocesses, real snapshot content (no stubbed scans or install): an npm-only repository whose selected commit is clean, while
// HEAD is ahead with a syntax-broken file committed and the checkout holds an uncommitted syntax error. node --check, the pnpm scans
// and the offline install run against the snapshot; a leak of HEAD or of the checkout would turn the check red.
const pnpmUsable = (() => { const r = defaultSpawn("pnpm", ["--version"], { cwd: os.tmpdir() }); return !r.error && r.status === 0; })();
test("E17 real subprocesses: npm-only dirty checkout with HEAD ahead passes on the selected commit's snapshot", { skip: pnpmUsable ? false : "pnpm is not on PATH" }, async (t) => {
  const repo = tmp(t, "rk-sc-real-");
  git(repo, "init", "-q", "-b", "main");
  const pnpmVersion = String(defaultSpawn("pnpm", ["--version"], { cwd: os.tmpdir() }).stdout).trim();
  write(repo, {
    ".gitignore": "node_modules/\n",
    "package.json": JSON.stringify({ name: "real-fixture", private: true, packageManager: `pnpm@${pnpmVersion}` }, null, 2),
    "pnpm-lock.yaml": "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\n\nimporters:\n\n  .: {}\n",
    "src/ok.mjs": "export const ok = 1;\n",
  });
  const a = commitAll(repo, "A");
  write(repo, { "src/broken.mjs": "export const = ;\n" });
  commitAll(repo, "B: committed syntax error, ahead of A");
  write(repo, { "src/ok.mjs": "export const ok = ;\n", "src/untracked.mjs": "not javascript at all (\n" });
  // The checkout's own install recorded a store (the primary environment's existing store); this one exists and stays empty.
  const store = tmp(t, "rk-sc-real-store-");
  write(repo, { "node_modules/.modules.yaml": `{\n  "storeDir": ${JSON.stringify(store)}\n}\n` });
  const snap = syncSnapshot({ checkout: repo, rev: a, root: tmp(t, "rk-sc-real-snaps-") });
  const out = []; const err = [];
  const code = await main(["check", "--commit", a, "--full-a", "--snapshot", snap.path], {
    cwd: repo, env: { ...process.env, RK_GATE_CACHE: "0", RIGHTKIT_SCRATCH_ROOT: tmp(t, "rk-sc-real-logs-") }, snapshotRoot: path.join(tmp(t, "rk-sc-real-own-"), "snapshots"), out: (m) => out.push(m), err: (m) => err.push(m),
    appCheck: async () => 0, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }),
  });
  const all = [...out, ...err].join("\n");
  assert.equal(code, 0, all);
  for (const id of ["node-check:src/ok.mjs", "pnpm-lockfile", "pnpm-audit"]) assert.match(all, new RegExp(`\\] pass +${id.replace(/[./]/g, "\\$&")} \\(`), `${id} ran and passed`);
  assert.doesNotMatch(all, /broken\.mjs|untracked\.mjs/, "neither HEAD's nor the checkout's files were scanned");
  assert.match(all, /snapshot dependencies root=\. pnpm=/);
  assert.match(all, /class A full pass/);
  assert.match(all, /is still tree [0-9a-f]+ after the check/);
  // The inverse: selecting B (its committed broken file) is red on that file, with a deterministic marker; the checkout's dirt still never shows.
  const b = git(repo, "rev-parse", "HEAD");
  const snapB = syncSnapshot({ checkout: repo, rev: b, root: path.dirname(snap.path) });
  const outB = []; const errB = [];
  const codeB = await main(["check", "--commit", b, "--full-a", "--snapshot", snapB.path], {
    cwd: repo, env: { ...process.env, RK_GATE_CACHE: "0", RIGHTKIT_SCRATCH_ROOT: tmp(t, "rk-sc-real-logs-b-") }, snapshotRoot: path.join(tmp(t, "rk-sc-real-own-b-"), "snapshots"), out: (m) => outB.push(m), err: (m) => errB.push(m),
    appCheck: async () => 0, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }),
  });
  const allB = [...outB, ...errB].join("\n");
  assert.equal(codeB, 1, allB);
  assert.match(allB, /\] fail +node-check:src\/broken\.mjs \(/);
  assert.match(allB, /rightkit-gate: verdict deterministic failed=node-check:src\/broken\.mjs env=[0-9a-f]{64}/);
  assert.doesNotMatch(allB, /untracked\.mjs/);
});

// Oracle review of 79c4427b, two findings still open at 5450f01d:
// (1) pnpm-lockfile re-verifies entries against the registry when pnpm's cache is cold, so an outage fails it: never a reusable red.
test("rightkit#1257: a pnpm-lockfile failure (registry-dependent) never yields a deterministic marker", async (t) => {
  const env = "e".repeat(64);
  for (const id of ["pnpm-lockfile", "pnpm-lockfile:apps/web"]) {
    assert.equal(deterministicMarker({ scans: [{ id, status: "fail", code: "EXIT" }] }, env), null, id);
    assert.equal(deterministicMarker({ scans: [{ id, status: "fail", code: "EXIT" }, { id: "node-check:a.mjs", status: "fail", code: "EXIT" }] }, env), null, `${id} with a deterministic failure beside it`);
  }
  const s = scenario(t);
  const run = io(t, s, { scans: [{ id: "pnpm-lockfile", class: "A", status: "fail", code: "EXIT", detail: "ERR_PNPM_META_FETCH_FAIL fetch failed" }] });
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io), 1);
  assert.equal(run.out.find((l) => l.startsWith(DETERMINISTIC_MARKER)), undefined);
});

// (2) On a broker host the cargo scans read the SNAPSHOT: what the checkout's workspace holds (an untracked member, a stray directory a
// members glob matches) is never read, so it neither refuses the check nor reaches its verdict.
function cargoScenario(t) {
  const repo = tmp(t, "rk-sc-cargo-");
  git(repo, "init", "-q", "-b", "main");
  write(repo, { ".gitignore": "target/\n", "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n', "Cargo.lock": "version = 4\n", "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\nedition = "2021"\n', "crates/a/src/lib.rs": "\n" });
  const a = commitAll(repo, "A");
  const snap = syncSnapshot({ checkout: repo, rev: a, root: tmp(t, "rk-sc-cargo-snaps-") });
  // The gate reads the SNAPSHOT's members as a carried read-only request (rightkit#1311: a root request inside a managed snapshot is refused):
  // a fixture broker serves it, with the snapshot and the checkout this scenario made.
  const broker = makeFakeBroker(t, { readOnlySnapshots: true });
  broker.rebase({ checkout: fs.realpathSync.native(repo), snapshotsRoot: path.dirname(snap.path) });
  const bin = tmp(t, "rk-sc-fakecargo-");
  const sentinel = path.join(bin, "non-metadata-cargo-ran");
  const fake = path.join(bin, "fake-cargo.mjs");
  // cargo's rule for `members = ["crates/*"]`: every directory under crates/ is a member; one without a Cargo.toml is an error.
  fs.writeFileSync(fake, `import fs from "node:fs"; import path from "node:path";
const args = process.argv.slice(2);
if (args[0] !== "metadata") { fs.writeFileSync(${JSON.stringify(sentinel)}, args.join(" ")); process.exit(0); }
const root = process.cwd(); const crates = path.join(root, "crates");
const packages = [];
for (const d of fs.existsSync(crates) ? fs.readdirSync(crates, { withFileTypes: true }).filter((e) => e.isDirectory()) : []) {
  const m = path.join(crates, d.name, "Cargo.toml");
  if (!fs.existsSync(m)) { console.error("error: failed to load manifest for workspace member \`" + path.join(crates, d.name) + "\`"); process.exit(101); }
  packages.push({ name: d.name, manifest_path: m, publish: null, id: d.name, targets: [] });
}
console.log(JSON.stringify({ packages, workspace_members: packages.map((p) => p.id), workspace_root: root, target_directory: path.join(root, "target"), version: 1 }));
`);
  return { repo, a, dir: snap.path, fake, sentinel, broker };
}
function brokerIo(t, s) {
  const out = []; const err = []; const plans = [];
  return { out, err, plans, io: {
    cwd: s.repo, env: { ...process.env, RK_GATE_CACHE: "0", RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" }, snapshotRoot: path.join(tmp(t, "rk-sc-own-"), "snapshots"), out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0,
    snapshotBroker: { ...s.broker.sb, checkout: fs.realpathSync.native(s.repo), markBrokerSubmit: subHandleStub(s.broker.sb.handle), get snapshot() { return { path: s.dir, expectedTree: s.broker.sb.snapshot.expectedTree }; } },
    // `cargo` is answered by the fake (never a real cargo, never the broker): metadata per cargo's member rule, anything else a sentinel.
    spawn: (cmd, args, opts = {}) => (cmd === "pnpm" && args[0] === "--version" ? { status: 0, stdout: "11.25.0\n", stderr: "" }
      : cmd === "cargo" ? defaultSpawn(process.execPath, [s.fake, ...args], opts) : defaultSpawn(cmd, args, opts)),
    resolveExecutor: () => ({ kind: "broker", cargo: ["cargo"] }),
    runScans: async (plan) => { plans.push(plan); return { scans: [], notes: [], cacheHits: [] }; },
  } };
}

test("Oracle (79c4427b), reversed by the contract (no dirty-tree condition): an untracked member and a stray members-glob directory in the checkout are ignored; the check runs on the commit's own workspace", async (t) => {
  const s = cargoScenario(t);
  const ok = brokerIo(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], ok.io), 0, ok.err.join("\n"));
  assert.equal(fs.realpathSync.native(ok.plans[0].roots.cargoLiveRoot), fs.realpathSync.native(s.repo), "the checkout is only where the client starts");
  assert.ok(ok.plans[0].scans.filter((x) => x.tool === "cargo" && x.class === "A").every((x) => x.root === "live"), "class A cargo scans read the snapshot");
  write(s.repo, { "crates/b/Cargo.toml": '[package]\nname = "b"\nversion = "0.1.0"\nedition = "2021"\n' });
  fs.mkdirSync(path.join(s.repo, "crates", "scratch-dir"), { recursive: true });
  write(s.repo, { "crates/scratch-dir/notes.txt": "agent scratch\n" });
  const run = brokerIo(t, s);
  assert.equal(await main(["check", "--commit", s.a, "--full-a", "--snapshot", s.dir], run.io), 0, run.err.join("\n"));
  assert.equal(run.plans.length, 1, "the scans were planned from the commit's members");
  assert.deepEqual(run.plans[0].scans.filter((x) => x.id.startsWith("cargo-metadata")).map((x) => x.cwd ?? "."), ["."], "crates/b is not in the commit, so no scan names it");
  assert.equal(fs.existsSync(s.sentinel), false, "only cargo metadata ran");
});

// ============================================================================================================================
// Row 5.09d through verifySnapshotDir (the door's compile verifies call it with the ledger before and admitNew after the compile): the
// same walk as snapshot.mjs, one implementation, and the result carries the admitted entries.
// ============================================================================================================================
const APP_IGNORE = "target/\nnode_modules/\n.right-release/\nsrc-tauri/binaries/\nsrc-tauri/gen/\n*.log\n";
const appScenario = (t) => scenario(t, { ".gitignore": APP_IGNORE, "src-tauri/Cargo.toml": "[package]\nname = \"app\"\n", "src-tauri/src/main.rs": "fn main() {}\n" });
const stageOutputs = (dir) => {
  const tool = path.join(dir, "src-tauri", "binaries", "tool-x86_64");
  fs.mkdirSync(path.dirname(tool), { recursive: true });
  fs.writeFileSync(tool, "sidecar bytes\n", { mode: 0o755 });
  if (process.platform !== "win32") fs.chmodSync(tool, 0o755);
  write(dir, { "src-tauri/binaries/stage-sidecar.receipt.json": "{\"staged\":1}\n", "src-tauri/gen/schemas/capabilities.json": "{}\n" });
  return tool;
};
const refusesWith = (fn, re, name) => assert.throws(fn, (e) => e instanceof CheckError && re.test(e.message), name);

test("5.09d verifySnapshotDir: the end-of-stage verify admits the stage's ignored outputs and returns them; the next start verify holds the ledger exactly; without options it stays strict", (t) => {
  const s = appScenario(t);
  const verify = (extra = {}) => verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit: s.a, tree: s.tree, when: "unit", ...extra });
  assert.equal(verify(), s.tree, "no options: the old return, the tree id");
  const tool = stageOutputs(s.dir);
  refusesWith(() => verify(), /src-tauri\/binaries is not in the commit's tree/, "the plain verify stays strict (the strict class A after-check)");
  const end = verify({ admitNew: true });
  assert.equal(end.tree, s.tree, "an ignored output does not change the tree id");
  const byPath = Object.fromEntries(end.ledger.map((e) => [e.path, e]));
  assert.deepEqual(byPath["src-tauri/binaries/tool-x86_64"], { path: "src-tauri/binaries/tool-x86_64", type: "file", size: 14, sha256: crypto.createHash("sha256").update("sidecar bytes\n").digest("hex"), execBit: process.platform !== "win32" });
  assert.deepEqual(Object.keys(byPath).sort(), ["src-tauri/binaries", "src-tauri/binaries/stage-sidecar.receipt.json", "src-tauri/binaries/tool-x86_64", "src-tauri/gen", "src-tauri/gen/schemas", "src-tauri/gen/schemas/capabilities.json"]);
  // Before the compile (the exact ledger): passes; a changed byte, a missing entry and a new entry refuse.
  assert.deepEqual(verify({ ledger: end.ledger }).ledger, end.ledger);
  fs.appendFileSync(tool, "tampered\n");
  refusesWith(() => verify({ ledger: end.ledger }), /ledger entry and changed/, "a changed staged byte");
  fs.writeFileSync(tool, "sidecar bytes\n", { mode: 0o755 });
  if (process.platform !== "win32") fs.chmodSync(tool, 0o755);
  fs.rmSync(path.join(s.dir, "src-tauri", "gen", "schemas", "capabilities.json"));
  refusesWith(() => verify({ ledger: end.ledger }), /capabilities\.json is a ledger entry .* and is missing/, "a missing entry");
  write(s.dir, { "src-tauri/gen/schemas/capabilities.json": "{}\n", "src-tauri/gen/schemas/new.json": "{}\n" });
  refusesWith(() => verify({ ledger: end.ledger }), /new\.json is not in the commit's tree and not in the stage-output ledger/, "a new entry before the compile");
  // After the compile: the ledger plus admitNew admits the new output.
  assert.ok(verify({ ledger: end.ledger, admitNew: true }).ledger.some((e) => e.path.endsWith("new.json")));
  // A staged entry changed by the compile is refused even at the end.
  fs.appendFileSync(tool, "rewritten by the compile\n");
  refusesWith(() => verify({ ledger: end.ledger, admitNew: true }), /ledger entry and changed/, "a staged sidecar changed before the end verify");
});

test("5.09d verifySnapshotDir refuses what must never be admitted: a non-ignored output, one only the checkout's info/exclude ignores, a link, target/ and .right-release/, a planted .gitignore, a hard link; a rewritten tracked file refuses by the tree id", (t) => {
  const s = appScenario(t);
  const verify = () => verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit: s.a, tree: s.tree, when: "unit", admitNew: true });
  const plant = (files) => write(s.dir, files);
  const clean = (name) => fs.rmSync(path.join(s.dir, ...name.split("/")), { recursive: true, force: true });
  plant({ "src-tauri/notes.txt": "x\n" });
  refusesWith(verify, /notes\.txt is not in the commit's tree and is not ignored by the commit's own ignore rules/, "not ignored");
  clean("src-tauri/notes.txt");
  fs.mkdirSync(path.join(s.repo, ".git", "info"), { recursive: true });
  fs.appendFileSync(path.join(s.repo, ".git", "info", "exclude"), "local-only.rs\n");
  plant({ "local-only.rs": "x\n" });
  refusesWith(verify, /local-only\.rs is not in the commit's tree and is not ignored/, "info/exclude does not count");
  clean("local-only.rs");
  for (const [name, re] of [["src-tauri/target/debug/x", /under "target"/], [".right-release/state.json", /under "\.right-release"/], ["src-tauri/binaries/.gitignore", /ignore file the commit does not have/]]) {
    plant({ [name]: "x\n" });
    refusesWith(verify, re, name);
    clean(name.split("/")[0] === "src-tauri" ? `src-tauri/${name.split("/")[1]}` : name.split("/")[0]);
  }
  fs.mkdirSync(path.join(s.dir, "src-tauri", "binaries"), { recursive: true });
  let linked = false;
  try { fs.symlinkSync(path.join(s.dir, "src-tauri", "Cargo.toml"), path.join(s.dir, "src-tauri", "binaries", "link")); linked = true; } catch { /* no link privilege: the link refusal is asserted in snapshot.test.mjs on hosts that can */ }
  if (linked) refusesWith(verify, /binaries\/link is a link/, "a link");
  clean("src-tauri/binaries");
  write(s.repo, { "payload.txt": "bytes\n" });
  fs.mkdirSync(path.join(s.dir, "src-tauri", "binaries"), { recursive: true });
  fs.linkSync(path.join(s.repo, "payload.txt"), path.join(s.dir, "src-tauri", "binaries", "hard"));
  refusesWith(verify, /binaries\/hard has \d+ hard links/, "a hard link");
  clean("src-tauri/binaries");
  assert.deepEqual(verify().ledger, [], "clean again");
  fs.writeFileSync(path.join(s.dir, "src-tauri", "src", "main.rs"), "fn main() { rewritten_by_a_generator(); }\n");
  refusesWith(verify, /is tree [0-9a-f]+ unit, not tree/, "a rewritten tracked file");
});

// ============================================================================================================================
// Item 5 D3 through the check path: commitOf, commitFiles and verifySnapshotDir use the sealed runner (snapshot-git.mjs).
// ============================================================================================================================
test("D3 sealed identity in the check path: a checkout armed with a filter driver, autocrlf, info/attributes and a hook runs nothing in commitOf, commitFiles or verifySnapshotDir; a masked tamper refuses; a replace ref changes nothing", (t) => {
  const s = scenario(t);
  const work = tmp(t, "rk-sc-armed-");
  const marker = path.join(work, "marker");
  const script = path.join(work, "filter.mjs");
  fs.writeFileSync(script, `import fs from "node:fs";\nfs.appendFileSync(${JSON.stringify(marker)}, "filter ran\\n");\nlet d = "";\nprocess.stdin.setEncoding("utf8");\nprocess.stdin.on("data", (c) => { d += c; });\nprocess.stdin.on("end", () => process.stdout.write(d.replace(/commit A/g, "commit A")));\n`);
  const fwd = (p) => p.split(path.sep).join("/");
  git(s.repo, "config", "filter.evil.clean", `"${fwd(process.execPath)}" "${fwd(script)}"`);
  git(s.repo, "config", "filter.evil.required", "false");
  git(s.repo, "config", "core.autocrlf", "true");
  fs.mkdirSync(path.join(s.repo, ".git", "info"), { recursive: true });
  fs.appendFileSync(path.join(s.repo, ".git", "info", "attributes"), "* filter=evil\n* text eol=crlf\n");
  const hook = path.join(s.repo, ".git", "hooks", "post-index-change");
  fs.mkdirSync(path.dirname(hook), { recursive: true });
  fs.writeFileSync(hook, `#!/bin/sh\necho hook >> '${fwd(marker)}'\n`, { mode: 0o755 });
  git(s.repo, "config", "core.hooksPath", fwd(path.dirname(hook)));
  // The scenario's snapshot was synced before the checkout was armed; the identity checks run now.
  const { commit, tree } = commitOf(s.repo, s.a);
  assert.deepEqual({ commit, tree }, { commit: s.a, tree: s.tree });
  assert.ok(commitFiles(s.repo, s.a).files.includes("README.md"));
  assert.equal(verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit, tree, when: "armed" }), s.tree);
  assert.equal(fs.existsSync(marker), false, "nothing the checkout configured ever ran");
  // A one-byte tamper that a rewriting filter or eol conversion could hide: README.md becomes CRLF.
  fs.writeFileSync(path.join(s.dir, "README.md"), "commit A\r\n");
  refusesWith(() => verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit, tree, when: "armed" }), /is tree [0-9a-f]+ armed, not tree/, "the CRLF tamper");
  assert.equal(fs.existsSync(marker), false);
  fs.writeFileSync(path.join(s.dir, "README.md"), "commit A\n");
  // Replace refs: plain git reads the replacement's tree, the check path the true one.
  git(s.repo, "replace", s.a, git(s.repo, "rev-parse", "HEAD"));
  assert.notEqual(git(s.repo, "rev-parse", `${s.a}^{tree}`), s.tree, "plain git is fooled by the replace ref");
  assert.deepEqual(commitOf(s.repo, s.a), { commit: s.a, tree: s.tree });
  assert.equal(verifySnapshotDir({ checkout: s.repo, dir: s.dir, commit, tree, when: "replaced" }), s.tree);
});
