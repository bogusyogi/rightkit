// Item 7 decision 2, question 1 (rows 7.05, 7.05b, 7.07, 7.08): the stage-output ledger. A real build writes into the directory it builds
// from (Tauri's gen/schemas, staged sidecars); those writes may remain at a stage's end only as ledger entries recorded by the door's own
// walk. Evidence label: real git, real snapshot sync into the test's own root, on a simulated Linux host; the child processes (the staging
// step, the compile) are doubles that write what the real tools write (probed in decision 2: tauri-build 2.7.4 acl.rs, genright/mailright
// layouts). `verifySnapshot` is a spy: its ledger/admitNew side is item 5 row 5.09d (lane L5S), which the door only calls.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { closeSnapshotStage, main, openSnapshotStage, stageSidecarsOnce } from "./bin/rightkit-gate.mjs";
import { attributionProblems, declaredInputProblems, diffEntries, neutralIgnored, receiptLedger, reportedPaths, walkSnapshot } from "./stage-ledger.mjs";
import { stageSidecars } from "./stage-sidecar.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;
const git = (cwd, ...a) => spawnSync("git", a, { cwd, encoding: "utf8" });
const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const triple = "x86_64-unknown-linux-gnu";

function tmp(t, prefix) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** The realistic app shape decision 2 asks for: committed src-tauri, a .gitignore over binaries/ and gen/, a build.rs that needs the sidecar and writes gen/schemas. */
const APP_FILES = {
  ".gitignore": "src-tauri/binaries/\nsrc-tauri/gen/\ndist/\n",
  "right-release.config.json": JSON.stringify({ schema: 1, app: "app", version: "1.0.0", sidecars: [{ kind: "cargo-bin", package: "eng" }] }) + "\n",
  "src-tauri/Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\nbuild = "build.rs"\n',
  "src-tauri/Cargo.lock": "version = 4\n",
  "src-tauri/tauri.conf.json": '{"bundle":{"externalBin":["binaries/eng"]}}\n',
  "src-tauri/build.rs": 'fn main() { /* requires binaries/eng-<triple>, emits rerun-if-changed for it, writes gen/schemas/x.json */ }\n',
  "src-tauri/src/main.rs": "fn main() {}\n",
  "src-tauri/gen/README.md": "x\n", // not committed below: see the genright variant
  "README.md": "committed\n",
};

async function fixture(t, files = APP_FILES, prefix = "rk-ledger-") {
  const dir = tmp(t, prefix);
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  const write = (rel, body, base = repo) => { fs.mkdirSync(path.dirname(path.join(base, rel)), { recursive: true }); fs.writeFileSync(path.join(base, rel), body); };
  for (const [rel, body] of Object.entries(files)) if (!rel.startsWith("src-tauri/gen/")) write(rel, body);
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c");
  const sha = git(repo, "rev-parse", "HEAD").stdout.trim(); const tree = git(repo, "rev-parse", `${sha}^{tree}`).stdout.trim();
  const snaps = path.join(dir, "snaps");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir };
  delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE; delete env.TAURI_CONFIG;
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : c === "pnpm" && a[0] === "--version" ? { status: 0, stdout: "11.25.0\n", stderr: "" } : { status: 1, stdout: "", stderr: "" });
  // Sync the snapshot through the door itself (a green no-op stage), then drive the ledger functions on it.
  const out = []; const err = [];
  const code = await main(["--to", "check", "--commit", sha], { cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: () => {}, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }) });
  assert.equal(code, 0, [...out, ...err].join("\n"));
  const dirSnap = path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".") && fs.statSync(path.join(snaps, n)).isDirectory()));
  const verifyCalls = [];
  const lines = [];
  const o = { platform: "linux", env, spawn, out: (m) => lines.push(m), err: (m) => lines.push(m), verifySnapshot: (a) => { verifyCalls.push(a); return {}; }, readFile: (f) => fs.readFileSync(f, "utf8") };
  const ds = { own: { held: () => true, handle: "own-test" }, path: dirSnap, commit: sha, tree, root: snaps, ledger: { entries: [] } };
  const rm = (rel) => fs.rmSync(path.join(dirSnap, rel), { recursive: true, force: true });
  return { dir, repo, sha, tree, snaps, env, o, ds, lines, verifyCalls, snapshot: dirSnap, write: (rel, body) => write(rel, body, dirSnap), writeRepo: (rel, body) => write(rel, body), rm, reset: () => { ds.ledger = { entries: [] }; lines.length = 0; verifyCalls.length = 0; } };
}

/** A staging step double writing what `stage-sidecar` writes: the sidecar, its receipt, and the JSON it prints. */
const stagingDouble = (fx, { extra = [], skipReported = [] } = {}) => (cmd, args, opts) => {
  const staged = `src-tauri/binaries/eng-${triple}`;
  const receipt = "src-tauri/binaries/stage-sidecar.json";
  fx.write(staged, "engine-bytes");
  fx.write(receipt, '{"schema":1}\n');
  for (const f of extra) fx.write(f, "unreported");
  return { status: 0, stderr: "", stdout: JSON.stringify({ files: [{ name: "eng", path: staged, sha256: sha256("engine-bytes") }].filter((x) => !skipReported.includes(x.path)), receiptPath: path.join(fx.snapshot, receipt) }) };
};

const stageArgs = (fx, spawnCapture) => ({ o: { ...fx.o, spawnCapture }, ds: fx.ds, root: fx.repo, configs: ["right-release.config.json"], appDir: ".", cli: "/x/right-release.mjs", commonDir: path.join(fx.repo, ".git"), commit: fx.sha, tree: fx.tree, stage: "compile" });

// ---- positive ---------------------------------------------------------------------------------------------------------------------

test("[Q1 positive 1] a realistic Tauri app: the staged sidecar, its receipt and gen/schemas are the ledger; the snapshot is exactly tree + dependencies + ledger; verify is called with the ledger and admitNew only at the end", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  assert.equal(await openSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" }), true, fx.lines.join("\n"));
  assert.deepEqual(fx.verifyCalls.map((c) => [c.ledger.length, c.admitNew]), [[0, false]], "the start verify is strict: an empty ledger, nothing admitted");
  assert.equal(await stageSidecarsOnce(stageArgs(fx, stagingDouble(fx))), true, fx.lines.join("\n"));
  assert.deepEqual(fx.ds.ledger.entries.map((e) => [e.path, e.kind]), [[`src-tauri/binaries/eng-${triple}`, "staged"], ["src-tauri/binaries/stage-sidecar.json", "staged"]]);
  fx.write("src-tauri/gen/schemas/x.json", '{"acl":true}\n'); // what every Tauri build writes (tauri-build acl.rs)
  const closed = await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile", step: "compile" });
  assert.equal(closed.ok, true, fx.lines.join("\n"));
  const ledger = receiptLedger(closed.entries);
  assert.deepEqual(ledger.map((e) => [e.path, e.kind, e.producer.stage]), [[`src-tauri/binaries/eng-${triple}`, "staged", "sidecar"], ["src-tauri/binaries/stage-sidecar.json", "staged", "sidecar"], ["src-tauri/gen/schemas/x.json", "generated", "compile"]]);
  assert.ok(ledger.every((e) => e.sha256 && !("abs" in e) && !JSON.stringify(e).includes(fx.snapshot)), "the receipt form names no absolute path and holds no bytes");
  const last = fx.verifyCalls.at(-1);
  assert.equal(last.admitNew, true); assert.equal(last.ledger.length, 3);
  // The next stage's start verify finds exactly the ledger; a generated entry may be rewritten or removed within a later stage.
  assert.equal(await openSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "package" }), true, fx.lines.join("\n"));
  assert.equal(fx.verifyCalls.at(-1).admitNew, false, "admitNew is end-of-stage only");
  fx.write("src-tauri/gen/schemas/x.json", '{"acl":"rewritten"}\n');
  fx.write("dist/index.html", "<html>"); // a frontend dist: ignored, generated
  const second = await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "package", step: "package" });
  assert.equal(second.ok, true, fx.lines.join("\n"));
  assert.deepEqual(second.entries.filter((e) => e.kind === "staged").map((e) => e.path), [`src-tauri/binaries/eng-${triple}`, "src-tauri/binaries/stage-sidecar.json"], "the staged entries are the same ones");
  assert.ok(second.entries.some((e) => e.path === "dist/index.html" && e.kind === "generated"));
  // A generated entry may be removed within a later stage. The generator removes the files AND their directories: an empty directory the commit
  // does not have is itself a refusal (stage-ledger.mjs walkSnapshot; snapshot.mjs snapshotProblem), so a leftover `gen/schemas/` would void the stage.
  fx.rm("src-tauri/gen"); fx.rm("dist");
  const removed = await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "package", step: "package" });
  assert.equal(removed.ok, true, `a generated entry may be removed:\n${fx.lines.join("\n")}`);
  assert.deepEqual(removed.entries.filter((e) => e.kind === "generated"), [], "and it leaves the ledger");
});

test("[7.07 positive] an ignored generator output inside the snapshot passes (the OUT_DIR-style file)", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  fx.write("src-tauri/gen/out/generated.rs", "pub const X: u8 = 1;\n");
  assert.equal((await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile", step: "compile" })).ok, true, fx.lines.join("\n"));
});

test("[Q1 positive 4] declared compile inputs: a declared path covering ledger entries must carry the digest of those very bytes in the receipt", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t, { ...APP_FILES, ".rightkit/compile-inputs.json": JSON.stringify({ inputs: ["src-tauri/binaries"] }) + "\n" }, "rk-ledger-decl-");
  assert.equal(await stageSidecarsOnce(stageArgs(fx, stagingDouble(fx))), true, fx.lines.join("\n"));
  const { judgeCompileInputs } = await import("./compile-inputs.mjs");
  const judged = judgeCompileInputs(fx.snapshot);
  const digest = judged.inputs.find((i) => i.path === "src-tauri/binaries").sha256;
  const entries = fx.ds.ledger.entries;
  const check = (receiptDigests) => declaredInputProblems({ snapshot: fx.snapshot, entries, receiptDigests, judge: judgeCompileInputs });
  assert.deepEqual(check([{ path: "src-tauri/binaries", sha256: digest, files: 2 }]), []);
  assert.match(check([{ path: "src-tauri/binaries", sha256: "0".repeat(64), files: 2 }])[0], /differs from the gate's digest/);
  assert.match(check([])[0], /carries no digest for it/);
  assert.deepEqual(declaredInputProblems({ snapshot: fx.snapshot, entries: [], receiptDigests: [], judge: judgeCompileInputs }), [], "an input covering no ledger entry needs no digest check");
});

// ---- negatives (each RK_CHECK_ERROR, the stage voided) -----------------------------------------------------------------------------

const refused = (fx, re) => assert.match(fx.lines.join("\n"), re);

test("[Q1 negative] a staged sidecar changed between staging and the end verify; a ledger byte changed between stages", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  assert.equal(await stageSidecarsOnce(stageArgs(fx, stagingDouble(fx))), true, fx.lines.join("\n"));
  fx.write(`src-tauri/binaries/eng-${triple}`, "tampered");
  assert.equal((await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" })).ok, false);
  refused(fx, /RK_CHECK_ERROR: \[compile\] src-tauri\/binaries\/eng-.* is a staged sidecar output and changed after it was staged/);
  fx.lines.length = 0;
  // Between stages: a ledger entry's byte changed.
  fx.write(`src-tauri/binaries/eng-${triple}`, "engine-bytes");
  fx.write("src-tauri/gen/schemas/x.json", "a\n");
  assert.equal((await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" })).ok, true, fx.lines.join("\n"));
  fx.write("src-tauri/gen/schemas/x.json", "b\n");
  assert.equal(await openSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "package" }), false);
  refused(fx, /RK_CHECK_ERROR: \[package\] before the stage the snapshot differs from the ledger.*changed: src-tauri\/gen\/schemas\/x\.json/);
});

test("[Q1 negative] an output at a non-ignored path; one ignored only by the checkout's info/exclude; a link; under target/ or .right-release/; a planted .rightkit-ledger.json", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  const close = async () => (await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" })).ok;
  const clean = () => { fx.reset(); for (const rel of ["src-tauri/other.txt", "a.scratch", "link", "src-tauri/target", ".right-release", ".rightkit-ledger.json", "src-tauri/gen", "node_modules"]) fx.rm(rel); };
  fx.write("src-tauri/other.txt", "x"); assert.equal(await close(), false); refused(fx, /src-tauri\/other\.txt is not ignored by the commit's own ignore rules/); clean();
  // The checkout's own info/exclude must never widen what is ignored (P4).
  fs.mkdirSync(path.join(fx.repo, ".git", "info"), { recursive: true });
  fs.appendFileSync(path.join(fx.repo, ".git", "info", "exclude"), "*.scratch\n");
  fx.write("a.scratch", "x"); assert.equal(await close(), false); refused(fx, /a\.scratch is not ignored by the commit's own ignore rules \(the checkout's info\/exclude and global excludes never count\)/); clean();
  if (process.platform !== "win32") {
    fs.symlinkSync("README.md", path.join(fx.snapshot, "link")); assert.equal(await close(), false); refused(fx, /link is a link the commit does not have/); clean();
  }
  fx.write("src-tauri/target/debug/x", "x"); assert.equal(await close(), false); refused(fx, /src-tauri\/target is under the reserved path segment "target"/); clean();
  fx.write(".right-release/cache/x", "x"); assert.equal(await close(), false); refused(fx, /\.right-release is under the reserved path segment/); clean();
  fx.write(".rightkit-ledger.json", "{}"); assert.equal(await close(), false); refused(fx, /\.rightkit-ledger\.json is not ignored/, "a planted ledger file is just an unledgered extra"); clean();
  fx.write("node_modules/evil/index.js", "x"); assert.equal(await close(), false); refused(fx, /reserved path segment "node_modules"/); clean();
  assert.equal(await close(), true, "control: with every plant removed the stage closes");
});

test("[7.04c N9, R-T2 rule 5] a declared root output a split package writes into the snapshot (a mailright-shaped updater tarball) must be ignored by the commit itself: the end verify refuses a non-ignored one naming the declared field and the .gitignore fix, and admits it once the commit ignores it", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  const close = async () => (await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "package" })).ok;
  fx.write("MailRight.app.tar.gz", "tarball"); fx.write("MailRight.app.tar.gz.sig", "sig");
  assert.equal(await close(), false);
  refused(fx, /MailRight\.app\.tar\.gz is not ignored by the commit's own ignore rules .*updater\.artifacts\[\]\.file.*add MailRight\.app\.tar\.gz to the commit's \.gitignore/);
  // The same outputs with the commit's own .gitignore covering them (the one-line app fix) are generated ledger entries.
  const fixed = await fixture(t, { ...APP_FILES, ".gitignore": `${APP_FILES[".gitignore"]}MailRight.app.tar.gz*\n` });
  fixed.write("MailRight.app.tar.gz", "tarball"); fixed.write("MailRight.app.tar.gz.sig", "sig");
  assert.equal((await closeSnapshotStage({ o: fixed.o, ds: fixed.ds, root: fixed.repo, stage: "package" })).ok, true, fixed.lines.join("\n"));
});

test("[Q1 negative] a staging step that writes a file it did not report, or reports one it did not write", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  assert.equal(await stageSidecarsOnce(stageArgs(fx, stagingDouble(fx, { extra: ["src-tauri/binaries/unreported.bin"] }))), false);
  refused(fx, /RK_CHECK_ERROR: \[compile\] stage-sidecar right-release\.config\.json: src-tauri\/binaries\/unreported\.bin was written by the staging step but it did not report it/);
  const fy = await fixture(t, APP_FILES, "rk-ledger-b-");
  const lying = (cmd, args) => ({ status: 0, stderr: "", stdout: JSON.stringify({ files: [{ path: "src-tauri/binaries/ghost" }], receiptPath: null }) });
  assert.equal(await stageSidecarsOnce(stageArgs(fy, lying)), false);
  refused(fy, /src-tauri\/binaries\/ghost was reported by the staging step but is not in the snapshot as a ledger entry/);
  // A step that prints no result cannot be attributed.
  const fz = await fixture(t, APP_FILES, "rk-ledger-c-");
  assert.equal(await stageSidecarsOnce(stageArgs(fz, () => ({ status: 0, stdout: "not json" }))), false);
  refused(fz, /did not print its result; its writes cannot be attributed/);
});

test("[Q1 negative] the verify side is called, never reimplemented: a verify that refuses (a tracked file rewritten) voids the stage; one admitting other entries than the walk recorded is refused", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  fx.write("src-tauri/gen/schemas/x.json", "a\n");
  fx.o.verifySnapshot = () => { throw new Error("the snapshot at X is not exactly commit C: README.md differs from the commit's blob"); };
  assert.equal((await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" })).ok, false);
  refused(fx, /RK_CHECK_ERROR: \[compile\] the snapshot at X is not exactly commit C: README\.md differs/);
  fx.lines.length = 0;
  fx.o.verifySnapshot = () => ({ ledger: [{ path: "something/else" }] });
  assert.equal((await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" })).ok, false);
  refused(fx, /admitted other ledger entries than the door's own walk recorded/);
  // A tracked rewrite is the real verify's refusal (BC:133 B12): the real verifySnapshot, no spy.
  fx.lines.length = 0; fx.rm("src-tauri/gen"); delete fx.o.verifySnapshot;
  fx.write("README.md", "rewritten by a generator\n");
  assert.equal((await closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" })).ok, false);
  // A tracked-byte rewrite passes the name walk (snapshotProblem) and is caught by the tree id (snapshot.mjs verifySnapshot: "is tree X, not tree Y of
  // commit Z"): both are the exact-snapshot refusal (GC:187, BC:133 B12); which message appears depends on what changed (a name or a byte).
  refused(fx, /RK_CHECK_ERROR: \[compile\] the snapshot at .* (is not exactly commit|is tree [0-9a-f]+, not tree [0-9a-f]+ of commit)/);
});

test("[Q1 negative] an ignored file planted BEFORE a stage: the start verify stays strict", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  fx.write("src-tauri/binaries/planted", "x");
  assert.equal(await openSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile" }), false);
  refused(fx, /before the stage the snapshot differs from the ledger.*new: src-tauri\/binaries\/planted/);
  assert.deepEqual(fx.verifyCalls, [], "the verify is not even reached");
});

test("[Q1] the stage-output door run: ledger in the receipt (never in the snapshot), TAURI_CONFIG refused, an unattributed sidecar write voids the compile (exit 2 for a tamper, 1 for a staging failure)", { skip: POSIX_SIM }, async (t) => {
  const dir = tmp(t, "rk-ledger-door-");
  const repo = path.join(dir, "repo"); fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main"); git(repo, "config", "user.email", "t@t"); git(repo, "config", "user.name", "t");
  for (const [rel, body] of Object.entries(APP_FILES)) if (!rel.startsWith("src-tauri/gen/")) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "c");
  const sha = git(repo, "rev-parse", "HEAD").stdout.trim();
  const snaps = path.join(dir, "snaps");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: dir, XDG_STATE_HOME: dir }; delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE; delete env.TAURI_CONFIG;
  const spawn = (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "" });
  const receipts = {};
  const door = async (extra = {}) => {
    const out = []; const err = [];
    const code = await main(["--to", "compile", "--commit", sha], { cwd: repo, platform: "linux", snapshotRoot: snaps, env, spawn, out: (m) => out.push(m), err: (m) => err.push(m), appCheck: async () => 0, runCheck: async () => 0, readReceipt: () => null, writeReceipt: ({ receipt }) => { Object.assign(receipts, receipt.stages); }, resolveExecutor: () => ({ kind: "direct", cargo: ["cargo"] }), verifySnapshot: () => ({}), ...extra });
    return { code, all: [...out, ...err].join("\n") };
  };
  const snapshot = () => path.join(snaps, fs.readdirSync(snaps).find((n) => !n.includes(".") && fs.statSync(path.join(snaps, n)).isDirectory()));
  const staging = (extra = []) => (c, a, opts) => {
    const s = snapshot();
    fs.mkdirSync(path.join(s, "src-tauri/binaries"), { recursive: true });
    fs.writeFileSync(path.join(s, `src-tauri/binaries/eng-${triple}`), "engine-bytes"); fs.writeFileSync(path.join(s, "src-tauri/binaries/stage-sidecar.json"), "{}\n");
    for (const f of extra) fs.writeFileSync(path.join(s, f), "x");
    return { status: 0, stderr: "", stdout: JSON.stringify({ files: [{ path: `src-tauri/binaries/eng-${triple}` }], receiptPath: path.join(s, "src-tauri/binaries/stage-sidecar.json") }) };
  };
  const compileWritesGen = async () => { fs.mkdirSync(path.join(snapshot(), "src-tauri/gen/schemas"), { recursive: true }); fs.writeFileSync(path.join(snapshot(), "src-tauri/gen/schemas/x.json"), "{}\n"); return 0; };
  let r = await door({ spawnCapture: staging(), runCompile: compileWritesGen });
  assert.equal(r.code, 0, r.all);
  const ledger = receipts.compile.snapshot.ledger;
  assert.deepEqual(ledger.map((e) => [e.path, e.kind]), [[`src-tauri/binaries/eng-${triple}`, "staged"], ["src-tauri/binaries/stage-sidecar.json", "staged"], ["src-tauri/gen/schemas/x.json", "generated"]], "the ledger is in the door receipt's stage entry");
  assert.ok(!fs.existsSync(path.join(snapshot(), ".rightkit-ledger.json")), "and never inside the snapshot");
  assert.equal(receipts.compile.snapshot.intactAfter, true);
  // Sidecars once per door run: the second run starts from a fresh sync (which removes every ledger entry), then stages again.
  r = await door({ spawnCapture: staging(), runCompile: compileWritesGen });
  assert.equal(r.code, 0, r.all);
  // An unattributed write by the staging step voids the compile; nothing compiles.
  let compiled = false;
  r = await door({ spawnCapture: staging(["src-tauri/binaries/unreported.bin"]), runCompile: async () => { compiled = true; return 0; } });
  assert.equal(r.code, 1, r.all); assert.match(r.all, /RK_CHECK_ERROR: \[compile\] stage-sidecar right-release\.config\.json: src-tauri\/binaries\/unreported\.bin was written by the staging step but it did not report it/); assert.equal(compiled, false);
  // A staged sidecar changed by the compile voids the stage (exit 2).
  r = await door({ spawnCapture: staging(), runCompile: async () => { fs.writeFileSync(path.join(snapshot(), `src-tauri/binaries/eng-${triple}`), "tampered"); return 0; } });
  assert.equal(r.code, 2, r.all); assert.match(r.all, /is a staged sidecar output and changed after it was staged/);
  // TAURI_CONFIG in the environment is refused (the door never sets or passes one).
  r = await door({ env: { ...env, TAURI_CONFIG: "{}" }, spawnCapture: staging(), runCompile: async () => 0 });
  assert.equal(r.code, 1); assert.match(r.all, /RK_TAURI_CONFIG_OVERRIDE/);
});

// ---- the pure pieces -------------------------------------------------------------------------------------------------------------

test("[Q1] the neutral ignore oracle: only the snapshot's own .gitignore applies, never the checkout's info/exclude or a global excludes file (runs natively on the Dell too: the null device there is NUL, the P4 gap)", async (t) => {
  const dir = tmp(t, "rk-oracle-");
  fs.writeFileSync(path.join(dir, ".gitignore"), "built/\n");
  fs.mkdirSync(path.join(dir, "built")); fs.writeFileSync(path.join(dir, "built", "a"), "x"); fs.writeFileSync(path.join(dir, "b.local"), "x"); fs.writeFileSync(path.join(dir, "c"), "x");
  // A hostile global excludes file and an info/exclude in a surrounding repository must not widen it.
  const home = tmp(t, "rk-oracle-home-");
  fs.writeFileSync(path.join(home, "excludes"), "*.local\n");
  fs.writeFileSync(path.join(home, ".gitconfig"), `[core]\n\texcludesFile = ${path.join(home, "excludes").replaceAll("\\", "/")}\n`);
  const env = { ...process.env, HOME: home, USERPROFILE: home, XDG_CONFIG_HOME: home, GIT_DIR: path.join(dir, "nope") };
  const ignored = neutralIgnored({ dir, paths: ["built/a", "b.local", "c"], env });
  assert.deepEqual([...ignored], ["built/a"]);
  assert.deepEqual([...neutralIgnored({ dir, paths: [] })], []);
});

test("[Q1] walkSnapshot and the entry helpers: tracked files are not extras, dependency roots are skipped, hard links and empty directories are refused, attribution is exact", async (t) => {
  const dir = tmp(t, "rk-walk-");
  // No `git init`: a real snapshot has no .git (or only the gate's config-only one, which walkSnapshot allows and snapshot.mjs 5.09b pins); the ignore
  // oracle runs against an empty bare git directory and reads only the snapshot's own .gitignore.
  fs.writeFileSync(path.join(dir, ".gitignore"), "out/\n"); fs.writeFileSync(path.join(dir, "tracked.txt"), "t");
  fs.mkdirSync(path.join(dir, "out")); fs.writeFileSync(path.join(dir, "out", "f"), "1");
  fs.mkdirSync(path.join(dir, "node_modules", "p"), { recursive: true }); fs.writeFileSync(path.join(dir, "node_modules", "p", "i.js"), "x");
  fs.mkdirSync(path.join(dir, "out", "empty"));
  const tracked = new Set([".gitignore", "tracked.txt"]);
  const walked = walkSnapshot({ dir, tracked, dependencyRoots: new Set(["node_modules"]) });
  assert.deepEqual(walked.entries.map((e) => e.path), ["out/f"]);
  assert.deepEqual(walked.problems, ["out/empty is an empty directory the commit does not have"]);
  if (process.platform !== "win32") {
    fs.rmSync(path.join(dir, "out", "empty"), { recursive: true });
    fs.linkSync(path.join(dir, "out", "f"), path.join(dir, "out", "g"));
    assert.match(walkSnapshot({ dir, tracked, dependencyRoots: new Set(["node_modules"]) }).problems.join("\n"), /has 2 hard links/);
  }
  const a = [{ path: "x", type: "file", size: 1, sha256: "1", execBit: false }];
  const b = [{ path: "x", type: "file", size: 1, sha256: "2", execBit: false }, { path: "y", type: "file", size: 1, sha256: "3", execBit: false }];
  assert.deepEqual(diffEntries(a, b), { added: ["y"], changed: ["x"], removed: [] });
  assert.deepEqual(attributionProblems({ before: [], after: b, reported: ["x"], receipt: "y" }), []);
  assert.match(attributionProblems({ before: [], after: b, reported: ["x"], receipt: null })[0], /y was written by the staging step but it did not report it/);
  assert.deepEqual([...reportedPaths({ files: [{ path: "a\\b", notices: [{ path: "c" }] }] })].sort(), ["a/b", "c"]);
});

test("[7.05a/Q1] the warm-cache mtime carry: a staged file carries the mtime of the cache file it came from, so a re-stage after the sync does not bump it", async (t) => {
  const dir = tmp(t, "rk-mtime-");
  const snapshot = path.join(dir, "snapshot"); const checkout = path.join(dir, "checkout");
  fs.mkdirSync(snapshot); fs.mkdirSync(checkout);
  const bytes = Buffer.from("model-bytes");
  const entry = { kind: "remote", name: "r2", url: "https://models.example.com/r2.onnx", sha256: sha256(bytes), dest: "src-tauri/resources/models/r2.onnx" };
  const cached = path.join(checkout, ".right-release", "cache", "remote", sha256(bytes));
  fs.mkdirSync(path.dirname(cached), { recursive: true }); fs.writeFileSync(cached, bytes);
  const old = new Date("2026-01-02T03:04:05Z"); fs.utimesSync(cached, old, old);
  let fetched = 0;
  const deps = { fetch: async () => { fetched += 1; return { ok: true, status: 200, arrayBuffer: async () => bytes }; }, env: {}, log: () => {}, now: () => "2026-10-29T00:00:00.000Z" };
  const stage = () => stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest: [entry], target: triple, deps });
  await stage();
  const staged = path.join(snapshot, "src-tauri", "resources", "models", "r2.onnx");
  assert.equal(fs.statSync(staged).mtime.getTime(), old.getTime(), "the staged file carries the cache file's mtime");
  assert.equal(fetched, 0, "served from the checkout's cache (a second run reports fromCache with no fetch)");
  // The next sync removes every ledger entry; re-staging restores the bytes with the same mtime (the app crate stays fresh).
  fs.rmSync(path.join(snapshot, "src-tauri"), { recursive: true, force: true });
  await stage();
  assert.equal(fs.statSync(staged).mtime.getTime(), old.getTime());
  assert.equal(fs.readFileSync(staged).toString(), "model-bytes");
});

test("[5.09d] the compile's own verify returns the entries it admitted; the door holds them against its walk at the stage's end (a mismatch voids the stage), and a verify that returns a tree id changes nothing", { skip: POSIX_SIM }, async (t) => {
  const fx = await fixture(t);
  fx.write("src-tauri/gen/schemas/x.json", "{}\n");
  const closeCompile = () => closeSnapshotStage({ o: fx.o, ds: fx.ds, root: fx.repo, stage: "compile", step: "compile" });
  // snapshot-check returned { tree, ledger: admitted } with exactly the walked entries: the stage closes.
  fx.ds.compileAdmitted = [{ path: "src-tauri/gen/schemas/x.json" }];
  assert.equal((await closeCompile()).ok, true, fx.lines.join("\n"));
  assert.equal(fx.ds.compileAdmitted, null, "the admitted entries are consumed by the stage that received them");
  // It admitted an entry the door's walk did not find (or missed one): refused.
  fx.ds.ledger = { entries: [] }; fx.lines.length = 0;
  fx.ds.compileAdmitted = [{ path: "src-tauri/gen/schemas/x.json" }, { path: "src-tauri/binaries/ghost" }];
  assert.equal((await closeCompile()).ok, false);
  refused(fx, /the compile's snapshot verify admitted other ledger entries than the door's own walk recorded/);
  // No ledger handed to the verify (it returned the tree id): nothing is compared, the door's own walk decides.
  fx.ds.ledger = { entries: [] }; fx.lines.length = 0; fx.ds.compileAdmitted = null;
  assert.equal((await closeCompile()).ok, true, fx.lines.join("\n"));
  // The gate's main() passes the ledger on and hands the admitted entries back through onCompileLedger (source form: the seam is wired both ways).
  const src = fs.readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(src, /ledger: o\.snapshotLedger, admitNew: true/);
  assert.match(src, /o\.onCompileLedger\?\.\(Array\.isArray\(verified\?\.ledger\) \? verified\.ledger : null\)/);
  assert.match(src, /onCompileLedger: \(entries\) => \{ ds\.compileAdmitted = entries; \}/);
});
