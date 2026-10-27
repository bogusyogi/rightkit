import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import zlib from "node:zlib";
import { GATES_REQUIRED_FROM, contentStatus, scratchRoot, admissionCodeOf, detectGates, extractTarball, gatesRequired, normalize, parseDoctor, pool, prepareLive, prepareSide, readTarball, runDoctor, runLeg, treeDigest } from "./rk-release-canary-leg.mjs";

const tmp = (t, prefix = "rk-leg-") => { const d = fs.mkdtempSync(path.join(os.tmpdir(), prefix)); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");
const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8" }); assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`); return r.stdout.trim(); };

/** A minimal ustar .tgz, `package/` prefixed like npm pack. */
function tgz(files, { prefix = "package/" } = {}) {
  const blocks = [];
  for (const [name, content] of Object.entries(files)) {
    const data = Buffer.from(content);
    const h = Buffer.alloc(512);
    h.write(`${prefix}${name}`, 0, 100, "utf8");
    h.write("0000755\0", 100); h.write("0000000\0", 108); h.write("0000000\0", 116);
    h.write(`${data.length.toString(8).padStart(11, "0")}\0`, 124);
    h.write("00000000000\0", 136);
    h.write("        ", 148);
    h.write("0", 156); h.write("ustar\0", 257); h.write("00", 263);
    let sum = 0; for (const b of h) sum += b;
    h.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148);
    blocks.push(h, data, Buffer.alloc((512 - (data.length % 512)) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  return zlib.gzipSync(Buffer.concat(blocks));
}

// ---------- parseDoctor ----------
const PASS = "right-release 0.6.3\nconfig: /x/right-release.config.mjs\napp: x\nplatform: mac\nrightkit-registry: ok\npreflight:\n  [ok  ] compiler-cache: broker-managed; compiler-cache selection and status are recorded in build receipts\n  [WARN] ownership: pending approval\n";
test("parseDoctor: pass", () => {
  const r = parseDoctor({ stdout: PASS, exit: 0 });
  assert.equal(r.status, "pass");
  assert.equal(r.compilerCache, true);
  assert.deepEqual(r.failNames, []);
});
test("parseDoctor: [FAIL] rows are a fail, named and sorted, whatever the trailing fatal line says", () => {
  const stdout = `${PASS}  [FAIL] ownership: 2 own copies\n         fix: run: right-release ownership\n  [FAIL] cargo-profile: panic = abort\n`;
  const r = parseDoctor({ stdout, stderr: "right-release: preflight found 2 blocking problem(s); the build would fail on these\n", exit: 1 });
  assert.equal(r.status, "fail");
  assert.deepEqual(r.failNames, ["cargo-profile", "ownership"]);
  assert.equal(r.fatal, null);
});
test("parseDoctor: a right-release: line with exit 1 and no rows is a fatal with a normalized signature", () => {
  const r = parseDoctor({ stdout: "right-release 0.6.3\n", stderr: "right-release: heardright dirty files can change the packaged app; resolve them before release:\n- /Volumes/D/x/y.rs\n", exit: 1 });
  assert.equal(r.status, "fatal");
  assert.equal(r.fatal.signature, "heardright dirty files can change the packaged app; resolve them before release:");
  assert.equal(r.fatal.recoverable, false);
  const informational = parseDoctor({ stdout: "right-release: newer RightKit version (informational): rightkit-qa 0.8.0\n", stderr: "right-release: boom at /a/b\n", exit: 1 });
  assert.equal(informational.fatal.signature, "boom at <path>");
});
test("parseDoctor: a command-scoped refusal (right-release doctor: the app check found problems…) is a fatal, not a crash", () => {
  const stderr = "FAIL eol-policy (G04): 1 file(s) committed with CRLF\n     fix: git add --renormalize\ncheck: 42 problem(s) in 95151b7ce02e (12 rules, 10.6s). Each would refuse later in CI or publication; fix and commit, then push.\nright-release doctor: the app check found problems in /Volumes/D/claude/heardright; nothing was started. Fix the above and commit, then run it again.\n";
  const r = parseDoctor({ stdout: "", stderr, exit: 1 });
  assert.equal(r.status, "fatal");
  assert.deepEqual(r.failNames, []);
  assert.equal(r.fatal.signature, "the app check found problems in <path>; nothing was started. Fix the above and commit, then run it again.");
  const cannot = parseDoctor({ stdout: "", stderr: "right-release doctor: the app check could not run in D:/claude/pulse; nothing was started. Fix the above and commit, then run it again.\n", exit: 1 });
  assert.equal(cannot.status, "fatal");
  assert.equal(cannot.fatal.signature, "the app check could not run in <path>; nothing was started. Fix the above and commit, then run it again.");
  assert.equal(parseDoctor({ stdout: "right-release dev: [RK_X] boom\n", exit: 0 }).status, "pass");
});
test("parseDoctor: crashes are non-1 exits, signals, spawn errors and exit 1 with nothing recognizable", () => {
  assert.equal(parseDoctor({ stderr: "TypeError: x is not a function\n    at file:///a.mjs:1:1\n\nNode.js v26.8.1\n", exit: 1 }).status, "crash");
  assert.equal(parseDoctor({ exit: 139 }).status, "crash");
  assert.equal(parseDoctor({ exit: null, signal: "SIGSEGV" }).status, "crash");
  assert.equal(parseDoctor({ exit: null, spawnError: "spawn ENOENT" }).status, "crash");
});
test("parseDoctor: RECOVERABLE registry outages and timeouts are infrastructure, not verdicts", () => {
  const r = parseDoctor({ stderr: "right-release: heardright RECOVERABLE RightKit registry unreachable (fetch failed)\n", exit: 1 });
  assert.equal(r.status, "recoverable");
  assert.equal(r.fatal.recoverable, true);
  assert.equal(parseDoctor({ stdout: PASS, exit: null, signal: "SIGKILL", timedOut: true }).status, "timeout");
});
test("parseDoctor: gate rows are [FAIL] gate:<name> rows (real output of doctor --gates, packages/release test fixture), kept apart from ordinary rows", () => {
  const real = {"stdout": "right-release 0.6.3\nconfig: /private/var/folders/xp/8dtgvyms3bb1yndrvmxhdg0h0000gn/T/right-release-test-lvEGdi/right-release.config.mjs\napp: fixture\nplatform: mac\ntier: <required for release/publish>\npackageManager: pnpm\nrightkit-registry: 4 exact pins verified against the registries (cargo 0, npm 4); resolved snapshot 157f79f152f3 observed 2026-10-23T15:28:44.192Z; policy a4b231bed90b2697\nworkdir: /private/var/folders/xp/8dtgvyms3bb1yndrvmxhdg0h0000gn/T/right-release-test-lvEGdi\nhardeningscan: /Volumes/D/scratch/rk-gates.MbL1/packages/release/hardeningscan.mjs\nartifact-storage: no installer-copy scripts detected\npreflight:\n  [ok  ] target-bridge: src-tauri/target/ is ready for the shared cache bridge\n  [ok  ] compiler-cache: broker-managed; compiler-cache selection and status are recorded in build receipts\n  [ok  ] disk: 17.3GB free\n  [FAIL] ownership: ownership evidence failed: repository identity is required\n         fix: restore protected RightKit ownership trust and catalog\n  [FAIL] gate:build-features: release/update build refused: it enables a test-automation or debugging feature (test-mode, qa-native, dev-native, qa-control, dev-inspect, testing, test-seams, isolated-test-keychain, demo, dev-overrides, qa-probes, dev-worker-override, qa-shot, qa-tools, dev-control, bench-native, devtools, content-devtools); none may ship:\n- target.package: enables feature `test-mode`\nRemove the feature from the release build command, the app's default features and its tauri dependency; dev builds (`right-release dev`) may keep it. No app may ship devtools unless it is on the allow-list in devtools-policy.json and declares `devtools: \"content-only\"` (rightkit-tauri-shell DevToolsPolicy::ContentOnly).\n", "stderr": "right-release: preflight found 2 blocking problem(s); the build would fail on these\n", "status": 1};
  const r = parseDoctor({ stdout: real.stdout, stderr: real.stderr, exit: real.status });
  assert.equal(r.status, "fail");
  assert.deepEqual(r.failNames, ["ownership"]);
  assert.deepEqual(r.gateFailNames, ["gate:build-features"]);
  assert.equal(r.compilerCache, true);
  assert.equal(r.rows.find((x) => x.name === "gate:build-features").gate, true);
  assert.equal(r.rows.find((x) => x.name === "ownership").gate, false);
  // Only the first failing gate prints per run; a gate-only failure is still a fail with no ordinary rows.
  const gateOnly = parseDoctor({ stdout: "preflight:\n  [ok  ] disk: 1GB free\n  [FAIL] gate:switch-inventory-missing: switches.json is missing\n", stderr: "right-release: preflight found 1 blocking problem(s); the build would fail on these\n", exit: 1 });
  assert.equal(gateOnly.status, "fail");
  assert.deepEqual(gateOnly.failNames, []);
  assert.deepEqual(gateOnly.gateFailNames, ["gate:switch-inventory-missing"]);
  for (const name of ["switch-inputs", "build-features", "production-mode", "release-dependencies", "feature-closure", "release-switches", "switch-inventory-missing"]) {
    assert.deepEqual(parseDoctor({ stdout: `  [FAIL] gate:${name}: x\n`, exit: 1 }).gateFailNames, [`gate:${name}`], name);
  }
  // the earlier guesses (a gates: header, gate-* names) are ordinary rows
  const old = parseDoctor({ stdout: "gates:\n  [FAIL] publication-gate: x\n  [FAIL] gate-seal: y\n", exit: 1 });
  assert.deepEqual(old.gateFailNames, []);
  assert.deepEqual(old.failNames, ["gate-seal", "publication-gate"]);
});

test("normalize drops the version header, registry summary, newer-version lines and paths", () => {
  const a = normalize("right-release 0.6.2\nconfig: /Volumes/D/scratch/rk-canary/baseline/x/right-release.config.mjs\nrightkit-registry: 3 pins ok (latest read live)\nright-release: newer RightKit version (informational): rightkit-qa 0.8.0\n  [FAIL] ownership: copy at D:\\scratch\\x\\a.rs:3\n");
  const b = normalize("right-release 0.6.3\nconfig: /Volumes/D/scratch/rk-canary/candidate/x/right-release.config.mjs\nrightkit-registry: 4 pins ok\n  [FAIL] ownership: copy at D:\\scratch\\y\\a.rs:3\n");
  assert.equal(a, b);
  assert.equal(a, "config: <path>\n  [FAIL] ownership: copy at <path>");
  assert.match(normalize("see https://example.com/a/b and node_modules/x"), /https:\/\/example\.com\/a\/b and node_modules\/x/);
});

// ---------- tarballs ----------
test("extractTarball strips package/, refuses escapes, and prepareSide checks digests and the manifest identity", (t) => {
  const dir = tmp(t);
  const release = path.join(dir, "r.tgz"); fs.writeFileSync(release, tgz({ "package.json": JSON.stringify({ name: "@rightkit/release", version: "1.2.3" }), "cli/right-release.mjs": "x", "release.mjs": 'else if (arg === "--gates") opts.gates = true;' }));
  const dep = path.join(dir, "d.tgz"); fs.writeFileSync(dep, tgz({ "package.json": "{}", "dist/index.js": "1" }));
  const side = path.join(dir, "side");
  const prep = prepareSide({ sideDir: side, release: { file: release, sha256: sha256(fs.readFileSync(release)), version: "1.2.3" }, dependency: { file: dep, sha256: sha256(fs.readFileSync(dep)) } });
  assert.equal(prep.supportsGates, true);
  assert.ok(fs.existsSync(path.join(side, "node_modules", "@rightkit", "release", "cli", "right-release.mjs")));
  assert.ok(fs.existsSync(path.join(side, "node_modules", "smol-toml", "dist", "index.js")));
  assert.throws(() => prepareSide({ sideDir: side, release: { file: release, sha256: "0".repeat(64), version: "1.2.3" }, dependency: { file: dep, sha256: sha256(fs.readFileSync(dep)) } }), /differs from the manifest/);
  assert.throws(() => prepareSide({ sideDir: side, release: { file: release, sha256: sha256(fs.readFileSync(release)), version: "9.9.9" }, dependency: { file: dep, sha256: sha256(fs.readFileSync(dep)) } }), /extracted @rightkit\/release is 1\.2\.3/);
  const evil = path.join(dir, "evil.tgz"); fs.writeFileSync(evil, tgz({ "../../escape.txt": "x" }));
  assert.throws(() => extractTarball(evil, path.join(dir, "out")), /escapes/);
  assert.equal(readTarball(fs.readFileSync(release)).length, 3);
});

// ---------- exports ----------
function liveRepo(t, files = { "right-release.config.mjs": "export default {}\n", "mode.txt": "pass\n" }) {
  const dir = tmp(t, "rk-live-");
  const repo = path.join(dir, "app");
  fs.mkdirSync(repo);
  git(repo, "init", "-q", "-b", "trunk");
  git(repo, "config", "user.email", "t@example.com"); git(repo, "config", "user.name", "t");
  // Hermetic line endings: a global core.autocrlf=true (the Windows runners) would store the CRLF fixtures as LF.
  git(repo, "config", "core.autocrlf", "false");
  for (const [f, c] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, f)), { recursive: true }); fs.writeFileSync(path.join(repo, f), c); }
  git(repo, "add", "-A"); git(repo, "commit", "-qm", "one");
  git(repo, "remote", "add", "origin", "/somewhere/local/mirror.git");
  return { dir, repo, sha: git(repo, "rev-parse", "HEAD") };
}

test("prepareLive: reads the live checkout where it stands (HEAD, pin match, dirt counted, never refused) and writes nothing", (t) => {
  const { repo, sha } = liveRepo(t);
  const clean = prepareLive({ live: repo, sha });
  assert.equal(clean.head, sha);
  assert.equal(clean.headMatchesPin, true);
  assert.equal(clean.dirty, 0);
  assert.deepEqual(clean.unnormalizedBlobs, []);
  fs.writeFileSync(path.join(repo, "later.txt"), "a commit after the pin\n"); git(repo, "add", "-A"); git(repo, "commit", "-qm", "two");
  fs.writeFileSync(path.join(repo, "wip.txt"), "uncommitted work of another chat\n");
  const before = { refs: git(repo, "for-each-ref"), status: git(repo, "status", "--porcelain") };
  const moved = prepareLive({ live: repo, sha });
  assert.equal(moved.head, git(repo, "rev-parse", "HEAD"));
  assert.equal(moved.headMatchesPin, false, "the live HEAD is not the pinned commit: reported, not refused");
  assert.equal(moved.dirty, 1);
  assert.equal(prepareLive({ live: repo }).headMatchesPin, null, "no pin: nothing to compare");
  assert.deepEqual({ refs: git(repo, "for-each-ref"), status: git(repo, "status", "--porcelain") }, before, "the live checkout is untouched");
  assert.throws(() => prepareLive({ live: path.join(repo, "nope"), sha }), (e) => e.infra === true && /not a git checkout/.test(e.message));
});

test("prepareLive: a CRLF blob under a text attribute is tolerated and listed; other dirt is counted, not refused", (t) => {
  const crlf = liveRepo(t, { "doc.md": "a\r\nb\r\n" });
  fs.writeFileSync(path.join(crlf.repo, ".gitattributes"), "*.md text\n");
  git(crlf.repo, "add", ".gitattributes"); git(crlf.repo, "commit", "-qm", "attributes");
  const ok = prepareLive({ live: crlf.repo, sha: git(crlf.repo, "rev-parse", "HEAD") });
  assert.deepEqual(ok.unnormalizedBlobs, ["doc.md"]);
  assert.equal(ok.dirty, 0, "a tolerated blob is not dirt");
  assert.deepEqual(treeDigest(crlf.repo).lines, [" M doc.md"]);
  const dirt = liveRepo(t, { "id.txt": "x $Id: old $\n" });
  fs.writeFileSync(path.join(dirt.repo, ".gitattributes"), "id.txt ident\n");
  git(dirt.repo, "add", ".gitattributes"); git(dirt.repo, "commit", "-qm", "attributes");
  // Deterministic: move id.txt's mtime past the index's stat entry so git re-reads its content (the ident attribute was added after
  // the file was committed, so its content IS dirty). Without this, whether git trusts its cached stat and reports it clean depends
  // on timing (racy-git): under load the row was 0 (fullA-10, 2026-10-28).
  const later = Date.now() / 1000 + 5; fs.utimesSync(path.join(dirt.repo, "id.txt"), later, later);
  const counted = prepareLive({ live: dirt.repo, sha: git(dirt.repo, "rev-parse", "HEAD") });
  assert.equal(counted.dirty, 1);
  assert.deepEqual(counted.unnormalizedBlobs, []);
});

test("prepareLive: a file whose cleaned content differs from its index blob is dirt even when its stat says unchanged (stale-stat; ident added after the commit)", (t) => {
  const dirt = liveRepo(t, { "id.txt": "x $Id: old $\n" });
  // mtime OLDER than the index entry, and not racy: cache the old stat first, then rewrite the index (the .gitattributes add),
  // so plain `git status --porcelain` trusts id.txt's cached stat and reads it clean although its cleaned content is not its blob.
  const old = Date.now() / 1000 - 3600; fs.utimesSync(path.join(dirt.repo, "id.txt"), old, old);
  git(dirt.repo, "update-index", "--refresh");
  fs.writeFileSync(path.join(dirt.repo, ".gitattributes"), "id.txt ident\n");
  git(dirt.repo, "add", ".gitattributes"); git(dirt.repo, "commit", "-qm", "attributes");
  assert.equal(git(dirt.repo, "status", "--porcelain"), "", "precondition: plain git status trusts the cached stat and reads the tree clean");
  const indexBefore = sha256(fs.readFileSync(path.join(dirt.repo, ".git", "index")));
  assert.equal(prepareLive({ live: dirt.repo, sha: dirt.sha }).dirty, 1);
  assert.equal(sha256(fs.readFileSync(path.join(dirt.repo, ".git", "index"))), indexBefore, "the real index is not rewritten");
  const clean = liveRepo(t, { "id.txt": "x\n" });
  fs.utimesSync(path.join(clean.repo, "id.txt"), old, old);
  assert.equal(prepareLive({ live: clean.repo, sha: clean.sha }).dirty, 0, "an unchanged file stays clean");
});

test("treeDigest: any byte change to an already-modified (or untracked) file changes the digest though its status line does not", (t) => {
  const { repo } = liveRepo(t, { "a.txt": "a\n" });
  fs.writeFileSync(path.join(repo, "a.txt"), "first edit\n");
  fs.writeFileSync(path.join(repo, "new.txt"), "untracked 1\n");
  const one = treeDigest(repo);
  assert.deepEqual(one.lines, [" M a.txt", "?? new.txt"]);
  fs.writeFileSync(path.join(repo, "a.txt"), "second edit\n");
  const two = treeDigest(repo);
  assert.deepEqual(two.lines, one.lines, "same status text");
  assert.notEqual(two.digest, one.digest, "tracked, already dirty: content changed");
  fs.writeFileSync(path.join(repo, "new.txt"), "untracked 2\n");
  assert.notEqual(treeDigest(repo).digest, two.digest, "untracked: content changed");
  assert.equal(treeDigest(repo).digest, treeDigest(repo).digest, "stable when nothing changes");
});

test("contentStatus: symlinks are compared by link text (never followed), staged and unmerged entries survive, ignored output stays out of dirt, the real index is untouched", { skip: process.platform === "win32" && "symlinks need privileges on Windows" }, (t) => {
  const { repo, sha } = liveRepo(t, { ".gitignore": "out/\n", "target.txt": "t\n", "a.txt": "a\n" });
  fs.symlinkSync("target.txt", path.join(repo, "ln")); git(repo, "add", "ln"); git(repo, "commit", "-qm", "link");
  const indexOf = () => sha256(fs.readFileSync(path.join(repo, ".git", "index")));
  const head = git(repo, "rev-parse", "HEAD");
  assert.equal(prepareLive({ live: repo, sha: head }).dirty, 0, "a clean symlink is clean");
  fs.writeFileSync(path.join(repo, "target.txt"), "changed target\n");
  assert.deepEqual(contentStatus(repo).map((e) => e.line), [" M target.txt"], "the link is not followed: only the target file is dirty");
  fs.rmSync(path.join(repo, "ln")); fs.symlinkSync("a.txt", path.join(repo, "ln"));
  assert.deepEqual(contentStatus(repo).map((e) => e.line), [" M ln", " M target.txt"], "a retargeted link is dirty by its text");
  git(repo, "checkout", "-q", "--", "ln", "target.txt");
  fs.mkdirSync(path.join(repo, "out")); fs.writeFileSync(path.join(repo, "out", "x"), "1");
  assert.equal(prepareLive({ live: repo, sha: head }).dirty, 0, "ignored output is not dirt");
  assert.deepEqual(contentStatus(repo, { ignored: true }).map((e) => e.line), ["!! out/"]);
  fs.writeFileSync(path.join(repo, "a.txt"), "staged\n"); git(repo, "add", "a.txt");
  fs.writeFileSync(path.join(repo, "a.txt"), "staged then edited\n");
  assert.deepEqual(contentStatus(repo).map((e) => e.line), ["MM a.txt"], "staged and unstaged changes are both visible");
  git(repo, "checkout", "-q", "--", "a.txt"); git(repo, "reset", "-q", "--hard");
  const base = git(repo, "rev-parse", "--abbrev-ref", "HEAD");
  git(repo, "checkout", "-qb", "side"); fs.writeFileSync(path.join(repo, "a.txt"), "side\n"); git(repo, "commit", "-qam", "side");
  git(repo, "checkout", "-q", base); fs.writeFileSync(path.join(repo, "a.txt"), "main\n"); git(repo, "commit", "-qam", "main");
  spawnSync("git", ["merge", "side"], { cwd: repo, encoding: "utf8" });
  const before = indexOf();
  assert.deepEqual(contentStatus(repo).map((e) => e.line), ["UU a.txt"], "an unmerged path is reported");
  assert.equal(prepareLive({ live: repo }).dirty, 1);
  assert.equal(indexOf(), before, "the real index is not rewritten");
  void sha;
});

test("contentStatus: a renamed staged file is one entry, and the throwaway index lives under the scratch root, removed afterwards", (t) => {
  const { repo } = liveRepo(t, { "a.txt": "aaaaaaaaaaaaaaaaaaaa\nbbbbbbbbbbbbbbbbbbbbbb\n" });
  const root = tmp(t, "rk-scratch-");
  const saved = process.env.RIGHTKIT_SCRATCH_ROOT; process.env.RIGHTKIT_SCRATCH_ROOT = path.join(root, "nested");
  t.after(() => { if (saved === undefined) delete process.env.RIGHTKIT_SCRATCH_ROOT; else process.env.RIGHTKIT_SCRATCH_ROOT = saved; });
  git(repo, "mv", "a.txt", "b.txt");
  assert.deepEqual(contentStatus(repo).map((e) => e.line), ["R  a.txt -> b.txt"]);
  assert.deepEqual(fs.readdirSync(path.join(root, "nested")), [], "no leftover index");
  assert.equal(scratchRoot({ RIGHTKIT_SCRATCH_ROOT: "/x" }, "linux"), "/x");
  assert.equal(scratchRoot({}, "darwin"), "/Volumes/D/scratch");
  assert.equal(scratchRoot({}, "win32"), "D:\\scratch");
});

test("treeDigest fails closed on a path it cannot read (not 'missing', not clean)", { skip: (process.platform === "win32" || process.getuid?.() === 0) && "needs POSIX permissions as non-root" }, (t) => {
  const { repo } = liveRepo(t, { "a.txt": "a\n" });
  const f = path.join(repo, "secret.txt"); fs.writeFileSync(f, "x\n"); fs.chmodSync(f, 0o000);
  t.after(() => { try { fs.chmodSync(f, 0o644); } catch { /* removed */ } });
  assert.throws(() => treeDigest(repo), (e) => e.infra === true && /secret\.txt/.test(e.message) && /EACCES/.test(e.message));
  fs.chmodSync(f, 0o644); fs.rmSync(f, { force: true });
  assert.doesNotThrow(() => treeDigest(repo));
});

test("admissionCodeOf finds the three broker admission codes and nothing else", () => {
  assert.equal(admissionCodeOf("x RK_REPOSITORY_BUSY y"), "RK_REPOSITORY_BUSY");
  assert.equal(admissionCodeOf("RK_BROKER_UNAVAILABLE"), "RK_BROKER_UNAVAILABLE");
  assert.equal(admissionCodeOf("rustright: RK_NOT_PRIMARY_CHECKOUT: <path> is not a primary checkout"), "RK_NOT_PRIMARY_CHECKOUT");
  assert.equal(admissionCodeOf("RK_SOMETHING_ELSE"), null);
  assert.equal(admissionCodeOf(undefined), null);
});

test("treeDigest sees untracked, modified and ignored output", (t) => {
  const { dir, repo } = liveRepo(t, { ".gitignore": "out/\n", "a.txt": "a\n" });
  const before = treeDigest(repo);
  assert.deepEqual(before.lines, []);
  fs.mkdirSync(path.join(repo, "out")); fs.writeFileSync(path.join(repo, "out", "x"), "1");
  fs.writeFileSync(path.join(repo, "a.txt"), "b\n");
  const after = treeDigest(repo);
  assert.notEqual(after.digest, before.digest);
  assert.deepEqual(after.lines, [" M a.txt", "!! out/"]);
  void dir;
});

// ---------- the doctor, end to end with a fake package ----------
const cli = (body) => `import fs from "node:fs";\nconst args = process.argv.slice(2);\nconst mode = fs.readFileSync("mode.txt", "utf8").trim();\nconsole.log("right-release 9.9.9\\nargs: " + args.join(" ") + "\\nplatform: " + args[args.indexOf("--platform") + 1] + "\\npreflight:");\n${body}\n`;
const PASS_BODY = `console.log("  [ok  ] compiler-cache: broker-managed; recorded");\nif (mode === "write") fs.writeFileSync("stray.txt", "x");\nif (mode === "fail") { console.log("  [FAIL] ownership: copy"); console.error("right-release: preflight found 1 blocking problem(s)"); process.exit(1); }\nif (mode === "fatal") { console.error("right-release: app boom at " + process.cwd()); process.exit(1); }\nif (mode === "crash") { throw new Error("kaput"); }\nif (mode === "hang") setInterval(() => {}, 1000);`;
function sidePkg(dir, name, { body = PASS_BODY, gates = false, version = "9.9.9", extra = {} } = {}) {
  const file = path.join(dir, `${name}.tgz`);
  fs.writeFileSync(file, tgz({ "package.json": JSON.stringify({ name: "@rightkit/release", version }), "cli/right-release.mjs": cli(body), "release.mjs": gates ? 'else if (arg === "--gates") opts.gates = true;' : "", ...extra }));
  return { file, sha256: sha256(fs.readFileSync(file)), version };
}
/** `files` are added to every app's repository (in one commit with the cell files); `attributes`, when given, is committed
 * alone afterwards, so a committed CRLF blob stays CRLF in the index while its path gets a text attribute. */
function manifestFor(t, modes, { candidateBody, baselineBody, hang, files = {}, attributes = null, candidateFiles = {} } = {}) {
  const dir = tmp(t, "rk-leg-run-");
  const live = path.join(dir, "ws");
  fs.mkdirSync(live);
  const repos = {};
  const host = process.platform;
  const platform = host === "win32" ? "win" : "mac";
  const apps = [];
  for (const [id, mode] of Object.entries(modes)) {
    const repoDir = `repo-${id}`;
    const r = liveRepo(t, { "sub/right-release.config.mjs": "export default {}\n", "sub/mode.txt": `${mode}\n`, ...files });
    if (attributes !== null) {
      fs.writeFileSync(path.join(r.repo, ".gitattributes"), attributes);
      git(r.repo, "add", ".gitattributes"); git(r.repo, "commit", "-qm", "attributes");
    }
    fs.renameSync(r.repo, path.join(live, repoDir));
    repos[repoDir] = { origin: `https://example.com/${id}.git`, sha: git(path.join(live, repoDir), "rev-parse", "HEAD") };
    apps.push({ id, repoDir, configPath: "sub/right-release.config.mjs", platforms: [platform], blocking: true });
  }
  const bundle = path.join(dir, "bundle"); fs.mkdirSync(bundle);
  const candidate = sidePkg(bundle, "candidate", { body: candidateBody ?? PASS_BODY, gates: true, version: "9.9.10", extra: candidateFiles });
  const baseline = sidePkg(bundle, "baseline", { body: baselineBody ?? PASS_BODY, version: "9.9.9" });
  const dep = path.join(bundle, "smol-toml.tgz"); fs.writeFileSync(dep, tgz({ "package.json": "{}" }));
  const manifest = {
    sha: "c".repeat(40), version: "9.9.10", limits: { totalMinutes: 5, cellMinutes: hang ? 0.01 : 1 },
    hosts: { [host]: { workspaceRoot: live, scratchRoot: path.join(dir, "scratch"), parallel: 2 } }, apps, repos,
    artifacts: { candidate: { ...candidate, file: "candidate.tgz" }, baseline: { ...baseline, file: "baseline.tgz" }, dependency: { file: "smol-toml.tgz", sha256: sha256(fs.readFileSync(dep)) } },
  };
  return { manifest, bundle, platform, dir };
}
const cellOf = (receipt, id, side) => receipt.cells.find((c) => c.id === id && c.side === side);

test("runLeg: both sides run in place on the live checkout; candidate-only --gates; --admission never; classification and tree writes", async (t) => {
  const { manifest, bundle, platform } = manifestFor(t, { clean: "pass", failing: "fail", fatal: "fatal", boom: "crash", writer: "write" });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  assert.equal(receipt.cells.length, 10);
  for (const side of ["baseline", "candidate"]) {
    assert.equal(cellOf(receipt, "clean", side).status, "pass");
    assert.equal(cellOf(receipt, "failing", side).status, "fail");
    assert.deepEqual(cellOf(receipt, "failing", side).failNames, ["ownership"]);
    assert.equal(cellOf(receipt, "fatal", side).status, "fatal");
    assert.equal(cellOf(receipt, "fatal", side).fatal.signature, "app boom at <path>");
    assert.equal(cellOf(receipt, "boom", side).status, "crash");
    assert.equal(cellOf(receipt, "clean", side).compilerCache, true);
    assert.equal(cellOf(receipt, "clean", side).treeChanged, false);
    assert.equal(cellOf(receipt, "clean", side).seconds >= 0, true);
  }
  assert.equal(cellOf(receipt, "clean", "candidate").gatesRun, true);
  assert.equal(cellOf(receipt, "clean", "baseline").gatesRun, false);
  assert.match(cellOf(receipt, "clean", "candidate").normalized, /args: doctor --config right-release\.config\.mjs --platform \w+ --gates/);
  assert.doesNotMatch(cellOf(receipt, "clean", "baseline").normalized, /--gates/);
  for (const c of receipt.cells) assert.doesNotMatch(c.normalized, /--admission/);
  for (const c of receipt.cells) {
    assert.equal(c.head, manifest.repos[c.repoDir].sha, "the live HEAD is recorded");
    assert.equal(c.headMatchesPin, true);
  }
  // The baseline cell runs first in the live tree and its write stays there (never cleaned): the baseline cell carries
  // the finding, and the candidate cell, which rewrites the same bytes, sees no change.
  const writer = cellOf(receipt, "writer", "baseline");
  assert.equal(writer.status, "pass");
  assert.equal(writer.treeChanged, true);
  assert.deepEqual(writer.treeDelta.filter((l) => !l.startsWith("+content ")), ["+?? sub/stray.txt"]);
  assert.ok(writer.treeDelta.some((l) => /^\+content sub\/stray\.txt file /.test(l)), "the new file's bytes are in the delta");
  assert.equal(writer.dirty, 0, "read before the doctor ran");
  assert.equal(cellOf(receipt, "writer", "candidate").dirty, 1, "the earlier cell's write is now the checkout's dirt");
  assert.equal(cellOf(receipt, "writer", "candidate").treeChanged, false);
  assert.equal(receipt.sides.candidate.supportsGates, true);
  assert.equal(receipt.sides.baseline.supportsGates, false);
  void platform;
});

test("runLeg: a hanging doctor is killed at the cell limit and reported as a timeout, after one retry", async (t) => {
  const { manifest, bundle } = manifestFor(t, { slow: "hang" }, { hang: true });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  for (const side of ["baseline", "candidate"]) {
    const c = cellOf(receipt, "slow", side);
    assert.equal(c.status, "timeout");
    assert.equal(c.attempts, 2);
  }
});

test("runLeg: a pin the live checkout does not stand at is reported, not refused; a missing checkout is infrastructure; a spent budget stops starting cells", async (t) => {
  const moved = manifestFor(t, { gone: "pass" });
  moved.manifest.repos["repo-gone"].sha = "e".repeat(40);
  const r1 = await runLeg(moved.manifest, { manifestDir: moved.bundle });
  assert.equal(cellOf(r1, "gone", "candidate").status, "pass");
  assert.equal(cellOf(r1, "gone", "candidate").headMatchesPin, false);
  assert.equal(cellOf(r1, "gone", "candidate").pinned, "e".repeat(40));
  const absent = manifestFor(t, { nowhere: "pass" });
  fs.rmSync(path.join(absent.manifest.hosts[process.platform].workspaceRoot, "repo-nowhere"), { recursive: true, force: true });
  const r0 = await runLeg(absent.manifest, { manifestDir: absent.bundle });
  assert.equal(cellOf(r0, "nowhere", "candidate").status, "infra");
  assert.match(cellOf(r0, "nowhere", "candidate").detail, /not a git checkout/);
  const spent = manifestFor(t, { late: "pass" });
  let clock = 0;
  const r2 = await runLeg(spent.manifest, { manifestDir: spent.bundle, now: () => (clock += 10 * 60_000) });
  assert.equal(cellOf(r2, "late", "candidate").status, "budget");
});

test("runLeg: a side whose tarball digest is wrong makes every cell on it infrastructure, not a verdict", async (t) => {
  const { manifest, bundle } = manifestFor(t, { clean: "pass" });
  manifest.artifacts.candidate.sha256 = "0".repeat(64);
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  assert.equal(cellOf(receipt, "clean", "candidate").status, "infra");
  assert.equal(cellOf(receipt, "clean", "baseline").status, "pass");
  assert.match(receipt.sideErrors.candidate, /differs from the manifest/);
});

test("runDoctor refuses --admission by construction and bounds output", async (t) => {
  const dir = tmp(t);
  fs.writeFileSync(path.join(dir, "bin.mjs"), 'console.log(process.argv.slice(2).join(" "));');
  const r = await runDoctor({ bin: path.join(dir, "bin.mjs"), appRoot: dir, configRel: "c.mjs", platform: "mac", sideDir: dir, tmpDir: path.join(dir, "tmp") });
  assert.equal(r.stdout.trim(), "doctor --config c.mjs --platform mac");
  assert.equal(r.args.includes("--admission"), false);
});

test("pool never runs more than the limit at once", async () => {
  let live = 0, peak = 0;
  await pool([1, 2, 3, 4, 5, 6], 4, async () => { live += 1; peak = Math.max(peak, live); await new Promise((r) => setTimeout(r, 5)); live -= 1; });
  assert.equal(peak, 4);
});

test("gates support is asked of the CLI's own --help (doctor ... [--gates]); the source text is only a fallback", (t) => {
  const dir = tmp(t);
  const dep = path.join(dir, "d.tgz"); fs.writeFileSync(dep, tgz({ "package.json": "{}" }));
  const mkSide = (name, cliSource, extra = {}) => {
    const file = path.join(dir, `${name}.tgz`);
    fs.writeFileSync(file, tgz({ "package.json": JSON.stringify({ name: "@rightkit/release", version: "0.6.4" }), "cli/right-release.mjs": cliSource, ...extra }));
    return prepareSide({ sideDir: path.join(dir, name), release: { file, sha256: sha256(fs.readFileSync(file)), version: "0.6.4" }, dependency: { file: dep, sha256: sha256(fs.readFileSync(dep)) } });
  };
  const helpWith = 'console.log("Usage\\n  doctor [--platform mac|win] [--gates]   Inspect one app");';
  const helpWithout = 'console.log("Usage\\n  doctor [--platform mac|win]   Inspect one app");';
  assert.deepEqual([mkSide("a", helpWith).supportsGates, mkSide("a", helpWith).gatesVia], [true, "help"], "found through --help although release.mjs never mentions the flag");
  assert.deepEqual([mkSide("b", helpWithout).supportsGates, mkSide("b", helpWithout).gatesVia], [false, null]);
  assert.equal(mkSide("c", helpWithout, { "release.mjs": 'else if (arg === "--gates") x();' }).gatesVia, "source");
  assert.equal(mkSide("d", "process.exit(3)", {}).supportsGates, false, "a CLI that cannot even print help supports nothing");
  assert.equal(typeof detectGates, "function");
});

test("gatesRequired: candidates from 0.6.4 on must run --gates", () => {
  assert.equal(GATES_REQUIRED_FROM, "0.6.4");
  for (const [v, want] of [["0.6.3", false], ["0.6.4", true], ["0.6.10", true], ["0.7.0", true], ["1.0.0", true], ["0.5.9", false], ["junk", false]]) assert.equal(gatesRequired(v), want, v);
});

test("runLeg: a candidate >= 0.6.4 that cannot run --gates makes every candidate cell incomplete (infra), not green", async (t) => {
  const { manifest, bundle } = manifestFor(t, { clean: "pass" });
  const bodyless = path.join(bundle, "candidate.tgz");
  fs.writeFileSync(bodyless, tgz({ "package.json": JSON.stringify({ name: "@rightkit/release", version: "0.6.4" }), "cli/right-release.mjs": cli(PASS_BODY), "release.mjs": "// the flag moved somewhere unrecognisable" }));
  manifest.artifacts.candidate = { file: "candidate.tgz", sha256: sha256(fs.readFileSync(bodyless)), version: "0.6.4" };
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  const c = cellOf(receipt, "clean", "candidate");
  assert.equal(c.status, "infra");
  assert.match(c.detail, /cannot run doctor --gates/);
  assert.equal(c.attempts, 2);
  assert.equal(cellOf(receipt, "clean", "baseline").status, "pass");
  assert.equal(receipt.sides.candidate.supportsGates, false);
});

test("runLeg: each cell records whether the checkout has no node_modules", async (t) => {
  const { manifest, bundle } = manifestFor(t, { clean: "pass" });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  assert.equal(cellOf(receipt, "clean", "candidate").noNodeModules, true);
});

const CRLF_DOC = { "sub/doc.md": "a\r\nb\r\n" };
test("runLeg: a doctor write to a tolerated CRLF blob flips treeChanged (hashed around the doctor); an untouched one does not", async (t) => {
  const { manifest, bundle } = manifestFor(t, { crlf: "touchdoc", quiet: "pass" }, {
    candidateBody: 'if (mode === "touchdoc") fs.writeFileSync("doc.md", "changed\\r\\n");',
    files: CRLF_DOC, attributes: "*.md text\n",
  });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  const touched = cellOf(receipt, "crlf", "candidate");
  assert.equal(touched.status, "pass");
  assert.equal(touched.treeChanged, true);
  assert.deepEqual(touched.unnormalizedBlobs, ["sub/doc.md"]);
  assert.ok(touched.treeDelta.some((l) => /^[-+]hash sub\/doc\.md /.test(l)), touched.treeDelta.join(";"));
  assert.equal(cellOf(receipt, "crlf", "baseline").treeChanged, false);
  assert.equal(cellOf(receipt, "quiet", "candidate").treeChanged, false);
  assert.deepEqual(cellOf(receipt, "quiet", "candidate").unnormalizedBlobs, ["sub/doc.md"]);
});

test("runLeg: a crash or fail carries its broker admission code; a clean cell carries none", async (t) => {
  const { manifest, bundle } = manifestFor(t, { refused: "admit", clean: "pass" }, {
    candidateBody: 'if (mode === "admit") { console.error("Error: cargo metadata: RK_NOT_PRIMARY_CHECKOUT: <path>"); process.exit(1); }',
  });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  const refused = cellOf(receipt, "refused", "candidate");
  assert.equal(refused.status, "crash");
  assert.equal(refused.admissionCode, "RK_NOT_PRIMARY_CHECKOUT");
  assert.equal(cellOf(receipt, "refused", "baseline").status, "pass");
  assert.equal(cellOf(receipt, "refused", "baseline").admissionCode, null);
  assert.equal(cellOf(receipt, "clean", "candidate").admissionCode, null);
  assert.equal(receipt.admissionRefused, null, "the probe did not run here, so no leg refusal is recorded");
});

test("runLeg: a probe the broker refuses makes every cell of the leg infrastructure, and no cell runs", async (t) => {
  const { manifest, bundle } = manifestFor(t, { clean: "pass", other: "pass" }, {
    candidateFiles: { "managed-cargo.mjs": 'export function runCargoSync() { return { status: 1, stdout: "", stderr: "RK_NOT_PRIMARY_CHECKOUT: <path> is not a primary checkout" }; }\n' },
    files: { "sub/Cargo.toml": '[package]\nname = "x"\nversion = "0.1.0"\n' },
  });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  assert.equal(receipt.admissionProbe.ran, true);
  assert.equal(receipt.admissionRefused, "RK_NOT_PRIMARY_CHECKOUT");
  assert.equal(receipt.cells.length, 4);
  for (const c of receipt.cells) {
    assert.equal(c.status, "infra");
    assert.equal(c.detail, "broker admission: RK_NOT_PRIMARY_CHECKOUT");
    assert.equal(c.attempts, undefined, "no cell ran, so none was retried");
  }
});

test("runLeg: a probe that cannot run (no managed-cargo in the candidate) records why and refuses nothing", async (t) => {
  const { manifest, bundle } = manifestFor(t, { clean: "pass" }, { files: { "sub/Cargo.toml": '[package]\nname = "x"\nversion = "0.1.0"\n' } });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  assert.equal(receipt.admissionProbe.ran, false);
  assert.match(receipt.admissionProbe.detail, /managed-cargo/);
  assert.equal(receipt.admissionRefused, null);
  assert.equal(cellOf(receipt, "clean", "candidate").status, "pass");
});

test("runLeg: deleting or chmod +x on a tolerated CRLF blob flips treeChanged", { skip: process.platform === "win32" && "no execute bit on Windows" }, async (t) => {
  const { manifest, bundle } = manifestFor(t, { chmoddoc: "chmoddoc", rmdoc: "rmdoc" }, {
    candidateBody: 'if (mode === "chmoddoc") fs.chmodSync("doc.md", 0o755); if (mode === "rmdoc") fs.rmSync("doc.md");',
    files: CRLF_DOC, attributes: "*.md text\n",
  });
  const receipt = await runLeg(manifest, { manifestDir: bundle });
  for (const id of ["chmoddoc", "rmdoc"]) {
    assert.equal(cellOf(receipt, id, "candidate").treeChanged, true, id);
    assert.equal(cellOf(receipt, id, "baseline").treeChanged, false, id);
  }
});

test("removeTree removes a tree holding read-only files (git objects on Windows answer EPERM to rmSync; #1219)", async () => {
  const { removeTree } = await import("./rk-release-canary-leg.mjs");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-leg-rm-"));
  const nested = path.join(root, "objects", "ab");
  fs.mkdirSync(nested, { recursive: true });
  fs.writeFileSync(path.join(nested, "cd"), "x");
  fs.chmodSync(path.join(nested, "cd"), 0o444);
  fs.chmodSync(nested, 0o555);
  removeTree(root);
  assert.equal(fs.existsSync(root), false);
  removeTree(root); // absent: no throw
});

test("treeDigest: every tracked file's raw bytes are hashed (defence in depth for changes git's conversion could hide); ignored paths are named as not covered", (t) => {
  const r = liveRepo(t, { "notes.txt": "a\nb\n", "keep.txt": "k\n" });
  fs.writeFileSync(path.join(r.repo, ".gitattributes"), "* text=auto\n");
  fs.writeFileSync(path.join(r.repo, ".gitignore"), "build/\n");
  git(r.repo, "add", "-A"); git(r.repo, "commit", "-qm", "attributes");
  fs.mkdirSync(path.join(r.repo, "build")); fs.writeFileSync(path.join(r.repo, "build", "out.bin"), "1");
  const before = treeDigest(r.repo);
  assert.ok(before.contentLines.some((l) => l.startsWith("raw keep.txt file ")), "a clean tracked file is hashed by its raw bytes");
  fs.writeFileSync(path.join(r.repo, "notes.txt"), "a\r\nb\r\n");
  const after = treeDigest(r.repo);
  assert.notEqual(after.digest, before.digest, "a CRLF rewrite changes the digest whatever git's status says");
  // A write inside an existing ignored directory is not detected, and the digest names that directory as not covered.
  fs.writeFileSync(path.join(r.repo, "build", "out.bin"), "2");
  const ignoredWrite = treeDigest(r.repo);
  assert.equal(ignoredWrite.digest, after.digest, "ignored output is outside the digest");
  assert.deepEqual(ignoredWrite.ignoredNotCovered, ["build/"], "and the result says so");
});
