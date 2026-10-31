import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { LINUX_ARTIFACTS_SCHEMA, glibcFloor, linuxArch, linuxArtifactsOutDir, packLinuxArtifacts, validateLinuxArchive } from "./linux-artifacts.mjs";
import { startRun, endRun } from "./gate-stages.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { CheckError, verifySnapshotDir } from "./snapshot-check.mjs";

const COMMIT = "a".repeat(40);
const fakeRun = (arch, objdump = "") => (cmd, args, opts) => {
  if (cmd === "uname") return { status: 0, stdout: `${arch}\n` };
  if (cmd === "objdump") return { status: 0, stdout: objdump };
  return spawnSync(cmd, args, { ...opts, stdio: "pipe" });
};
const ELF = Buffer.concat([Buffer.from([0x7f, 0x45, 0x4c, 0x46]), Buffer.alloc(60)]);

function app(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-linux-art-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, "dist", "bin"), { recursive: true });
  fs.writeFileSync(path.join(root, "dist", "bin", "pulse"), ELF);
  fs.writeFileSync(path.join(root, "dist", "README"), "hello\n");
  return root;
}

test("archive config is strict: name stem, non-empty relative includes, no unknown keys", () => {
  assert.equal(validateLinuxArchive(undefined), null);
  assert.deepEqual(validateLinuxArchive({ name: "pulse", include: ["dist"] }), { name: "pulse", include: ["dist"] });
  for (const bad of [{ name: "Pulse", include: ["dist"] }, { name: "pulse", include: [] }, { name: "pulse", include: ["/abs"] }, { name: "pulse", include: ["../up"] }, { name: "pulse", include: ["dist"], url: "x" }]) {
    assert.throws(() => validateLinuxArchive(bad), /archive/);
  }
});

test("architecture is uname -m and only x86_64 or aarch64", () => {
  assert.equal(linuxArch(fakeRun("x86_64")), "x86_64");
  assert.equal(linuxArch(fakeRun("aarch64")), "aarch64");
  assert.throws(() => linuxArch(fakeRun("arm64")), /unsupported/);
});

test("glibc floor is the highest GLIBC_ version any ELF file needs; non-ELF files are skipped; an objdump failure fails closed", (t) => {
  const root = app(t);
  const files = [path.join(root, "dist", "bin", "pulse"), path.join(root, "dist", "README")];
  assert.equal(glibcFloor(files, { run: fakeRun("x86_64", "x GLIBC_2.17 y GLIBC_2.34 z GLIBC_2.4\n") }), "2.34");
  assert.throws(() => glibcFloor(files, { run: () => ({ status: 1, stdout: "" }) }), /glibc floor cannot be stated/);
});

test("pack writes one tarball per arch with its sha256 and size, merges arches of the same build, and replaces a manifest of another build", (t) => {
  const root = app(t);
  const archive = { name: "pulse", include: ["dist"] };
  const x = packLinuxArtifacts({ root, archive, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {} });
  const tarPath = path.join(root, ".right-release", "linux", "pulse-0.1.0-linux-x86_64.tar.gz");
  const bytes = fs.readFileSync(tarPath);
  assert.equal(x.sha256, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(x.size, bytes.length);
  assert.ok(/^dist\/bin\/pulse$/m.test(spawnSync("tar", ["-tzf", tarPath], { encoding: "utf8" }).stdout), "a listed folder keeps its own tree under its base name");
  packLinuxArtifacts({ root, archive, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("aarch64", "GLIBC_2.39"), log: () => {} });
  let m = JSON.parse(fs.readFileSync(x.manifestPath, "utf8"));
  assert.equal(m.schema, LINUX_ARTIFACTS_SCHEMA);
  assert.deepEqual(Object.keys(m.artifacts).sort(), ["aarch64", "x86_64"]);
  assert.deepEqual(m.artifacts.x86_64, { file: "pulse-0.1.0-linux-x86_64.tar.gz", sha256: x.sha256, size: x.size, glibc: "2.35" });
  assert.equal(m.artifacts.aarch64.glibc, "2.39");
  packLinuxArtifacts({ root, archive, app: "pulse", version: "0.1.1", commit: COMMIT, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {} });
  m = JSON.parse(fs.readFileSync(x.manifestPath, "utf8"));
  assert.deepEqual(Object.keys(m.artifacts), ["x86_64"], "another version starts a new manifest, never mixed");
  assert.throws(() => packLinuxArtifacts({ root, archive, app: "pulse", version: "x", commit: COMMIT, run: fakeRun("x86_64"), log: () => {} }), /semver/);
  assert.throws(() => packLinuxArtifacts({ root, archive: { name: "pulse", include: ["missing"] }, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64"), log: () => {} }), /does not exist/);
});

test("the archive is flat: each listed file is stored under its base name; duplicate base names are refused", (t) => {
  const root = app(t);
  fs.mkdirSync(path.join(root, "scripts", "linux-guest"), { recursive: true });
  fs.writeFileSync(path.join(root, "scripts", "linux-guest", "install.sh"), "#!/bin/sh\n");
  const r = packLinuxArtifacts({ root, archive: { name: "pulse", include: ["dist/bin/pulse", "scripts/linux-guest/install.sh"] }, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {} });
  const entries = spawnSync("tar", ["-tzf", path.join(path.dirname(r.manifestPath), r.file)], { encoding: "utf8" }).stdout.trim().split("\n").sort();
  assert.deepEqual(entries, ["install.sh", "pulse"]);
  fs.mkdirSync(path.join(root, "other"), { recursive: true }); fs.writeFileSync(path.join(root, "other", "pulse"), "x");
  assert.throws(() => packLinuxArtifacts({ root, archive: { name: "pulse", include: ["dist/bin/pulse", "other/pulse"] }, app: "pulse", version: "0.1.0", commit: COMMIT, run: fakeRun("x86_64"), log: () => {} }), /share the base name pulse/);
});

// ---- a Linux dev stage's archive inside the door's source split ----------------------------------------------------------------------------------
// Real git, a real snapshot, a real live door run (startRun, read back by readSourceSplit) and the stage's own end-of-stage verify
// (verifySnapshotDir with admitNew, what closeSnapshotStage runs): nothing is mocked but uname and objdump, which a stub ELF cannot answer.
const gitIn = (cwd, ...args) => { const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout.trim(); };

/** A checkout holding `app/` (ignored `dist/`), its snapshot, the live door run and the files a linux dev stage leaves in the snapshot before it archives. */
function splitStage(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-linux-split-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const checkout = path.join(dir, "checkout"); fs.mkdirSync(path.join(checkout, "app"), { recursive: true });
  fs.writeFileSync(path.join(checkout, "app", ".gitignore"), "dist/\n.right-release/\n"); fs.writeFileSync(path.join(checkout, "app", "package.json"), JSON.stringify({ name: "pulse", version: "0.1.0" }));
  gitIn(checkout, "init", "-q", "-b", "main"); gitIn(checkout, "add", "-A"); gitIn(checkout, "commit", "-q", "-m", "one");
  const commit = gitIn(checkout, "rev-parse", "HEAD"); const tree = gitIn(checkout, "rev-parse", "HEAD^{tree}");
  const snap = syncSnapshot({ checkout, rev: commit, root: path.join(dir, "snapshots") });
  const appRoot = path.join(snap.path, "app");
  // The stage's staged binary: an ignored, untracked file under the snapshot, an admissible ledger entry.
  fs.mkdirSync(path.join(appRoot, "dist", "bin"), { recursive: true }); fs.writeFileSync(path.join(appRoot, "dist", "bin", "pulse"), ELF, { mode: 0o755 });
  const commonDir = path.join(checkout, ".git");
  const door = startRun({ commonDir, commit, tree, stage: "dev", source: { checkout, snapshot: snap.path, commit, tree, appDir: "app" } });
  t.after(() => endRun(door));
  const verify = () => verifySnapshotDir({ checkout, dir: snap.path, commit, tree, when: "after the stage", admitNew: true });
  return { dir, checkout, snapshot: snap.path, appRoot, commit, tree, env: { ...process.env, ...door.env }, verify };
}

test("B1: an archive written under the snapshot's .right-release (the old default in a split) fails the stage's end verify; the split packs into the checkout's and the verify passes", (t) => {
  const s = splitStage(t);
  const archive = { name: "pulse", include: ["dist/bin/pulse"] };
  const pack = (over) => packLinuxArtifacts({ root: s.appRoot, archive, app: "pulse", version: "0.1.0", commit: s.commit, run: fakeRun("x86_64", "GLIBC_2.35"), log: () => {}, ...over });
  // Before: the archive lands in <snapshot>/app/.right-release/linux, a reserved ledger segment, so "the result covers no known bytes" (closeSnapshotStage exit 2).
  const old = pack({ outDir: path.join(s.appRoot, ".right-release", "linux") });
  assert.ok(fs.existsSync(old.manifestPath) && old.manifestPath.startsWith(s.snapshot));
  assert.throws(() => s.verify(), (e) => e instanceof CheckError && /app\/\.right-release is under "\.right-release", a reserved path/.test(e.message));
  fs.rmSync(path.join(s.appRoot, ".right-release"), { recursive: true });
  assert.doesNotThrow(() => s.verify(), "without the archive the stage's only new file is the staged binary: an ignored ledger entry");
  // After: the default inside the split is the checkout's, found from the live door run, never from a path the caller names.
  const done = pack({ env: s.env });
  const expected = path.join(s.checkout, "app", ".right-release", "linux");
  assert.equal(path.dirname(done.manifestPath), expected);
  assert.ok(fs.existsSync(path.join(expected, "pulse-0.1.0-linux-x86_64.tar.gz")));
  assert.equal(fs.existsSync(path.join(s.appRoot, ".right-release")), false, "nothing was written into the snapshot");
  assert.doesNotThrow(() => s.verify(), "the stage verifies: the snapshot holds its commit plus the staged binary");
  const manifest = JSON.parse(fs.readFileSync(done.manifestPath, "utf8"));
  assert.equal(manifest.commit, s.commit); assert.equal(manifest.artifacts.x86_64.sha256, done.sha256);
  // The same place an unsplit run writes it, so whoever reads the manifest looks in one place.
  assert.equal(linuxArtifactsOutDir({ root: path.join(s.checkout, "app"), env: {} }), expected);
});

test("outside a split the archive stays under the app root; a split that cannot be proven is a refusal, never a fallback into the snapshot", (t) => {
  const s = splitStage(t);
  assert.equal(linuxArtifactsOutDir({ root: s.appRoot, env: {} }), path.join(s.appRoot, ".right-release", "linux"), "no RK_GATE_CHECKOUT: the checkout route, as before");
  assert.equal(linuxArtifactsOutDir({ root: s.appRoot, split: null }), path.join(s.appRoot, ".right-release", "linux"), "an explicit no-split");
  assert.equal(linuxArtifactsOutDir({ root: s.appRoot, split: { checkoutAppRoot: "/c/app" } }), path.join("/c/app", ".right-release", "linux"), "an explicit split");
  // Named but unprovable (a run id that is not live): the gate's refusal stands.
  assert.throws(() => linuxArtifactsOutDir({ root: s.appRoot, env: { ...s.env, RK_GATE_RUN: "0".repeat(32) } }), /no live door run/);
  // A process outside the snapshot its run names is refused too.
  assert.throws(() => linuxArtifactsOutDir({ root: s.checkout, env: s.env }), /outside the snapshot/);
});
