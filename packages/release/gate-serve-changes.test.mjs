// The open gate process's record of what changed in a snapshot since its tree was proven (NN11), against a real server and a real watcher.
// The server runs in a child process on a private socket and watches a temporary root: nothing of the host's own server is touched.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { test } from "node:test";
import { isHostSnapshotRoot, serveRequest } from "./gate-serve.mjs";
import { blobOid } from "./snapshot-changes.mjs";
import { treeProvenByChanges, SNAPSHOT_PROOF_COUNTS } from "./snapshot.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

test("only the host's own snapshot root may start a server", () => {
  assert.equal(isHostSnapshotRoot(fs.mkdtempSync(path.join(os.tmpdir(), "rk-not-host-")), { platform: "win32" }), false);
  assert.equal(isHostSnapshotRoot("/anything", { platform: "linux" }), true);
});

test("after a proof the server names what changed; a later proof checks those files only; outputs beside the commit leave the tree standing", async () => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-serve-changes-")));
  const dir = path.join(root, "aaaaaaaaaaaaaaaa");
  fs.mkdirSync(path.join(dir, "src"), { recursive: true });
  fs.writeFileSync(path.join(dir, "src", "a.txt"), "a"); fs.writeFileSync(path.join(dir, "b.txt"), "b");
  const entries = [{ path: "src/a.txt", mode: "100644", oid: blobOid(Buffer.from("a")) }, { path: "b.txt", mode: "100644", oid: blobOid(Buffer.from("b")) }];
  const sock = process.platform === "win32" ? `\\\\.\\pipe\\rk-serve-changes-test-${process.pid}-${Date.now()}` : path.join(root, "test.sock");
  const script = `import(${JSON.stringify(pathToFileURL(path.join(here, "gate-serve.mjs")).href)}).then((m) => m.serve({ root: ${JSON.stringify(root)}, listenOn: ${JSON.stringify(sock)} }));`;
  const child = spawn(process.execPath, ["--input-type=module", "-e", script], { stdio: "ignore" });
  const ask = (req) => serveRequest(req, { sock, env: {} });
  try {
    let up = null;
    for (let i = 0; i < 100 && !up; i += 1) { up = ask({ op: "status" }); if (!up) await sleep(100); }
    assert.ok(up?.healthy, "the test server answers and watches");

    const begun = ask({ op: "begin", dir });
    assert.ok(Number.isInteger(begun?.generation));
    assert.equal(ask({ op: "record", dir, tree: "t1", generation: begun.generation })?.ok, true);
    const clean = ask({ op: "check", dir, tree: "t1" });
    assert.equal(clean.clean, true);
    assert.deepEqual(clean.base, { tree: "t1", changed: [], renamed: [] });
    assert.equal(ask({ op: "check", dir, tree: "another" }).base, undefined); // a proof of another tree is no base for this one

    // A stage writes outputs beside the commit's files.
    fs.mkdirSync(path.join(dir, "target", "debug"), { recursive: true });
    fs.writeFileSync(path.join(dir, "target", "debug", "x.o"), "object");
    const afterOutputs = ask({ op: "check", dir, tree: "t1" });
    assert.equal(afterOutputs.clean, false);
    assert.equal(afterOutputs.base.tree, "t1");
    assert.ok(afterOutputs.base.renamed.includes("target"), JSON.stringify(afterOutputs.base));
    assert.ok(![...afterOutputs.base.changed, ...afterOutputs.base.renamed].some((p) => p === "src/a.txt" || p === "b.txt" || p === "src"), "no commit file was touched");
    const before = SNAPSHOT_PROOF_COUNTS.changedFileProofs;
    assert.equal(treeProvenByChanges({ reply: afterOutputs, dir, tree: "t1", entries, objectFormat: "sha1" }), true);
    assert.equal(SNAPSHOT_PROOF_COUNTS.changedFileProofs, before + 1);
    assert.equal(treeProvenByChanges({ reply: afterOutputs, dir, tree: "t2", entries, objectFormat: "sha1" }), false);

    // That proof is recorded as "the commit's files are this tree": the change record starts again, and "nothing changed" is still not claimed.
    assert.equal(ask({ op: "tree-record", dir, tree: "t1", generation: afterOutputs.generation })?.ok, true);
    const afterTreeRecord = ask({ op: "check", dir, tree: "t1" });
    assert.equal(afterTreeRecord.clean, false);
    assert.deepEqual(afterTreeRecord.base, { tree: "t1", changed: [], renamed: [] });

    // A commit file is edited: it is named, compared with the commit's blob, and the proven tree no longer stands.
    fs.writeFileSync(path.join(dir, "src", "a.txt"), "edited");
    const afterEdit = ask({ op: "check", dir, tree: "t1" });
    // Windows and Linux report an in-place write as a change; macOS FSEvents may report it as a rename. Either way it is named.
    assert.ok([...afterEdit.base.changed, ...afterEdit.base.renamed].includes("src/a.txt"), JSON.stringify(afterEdit.base));
    assert.equal(treeProvenByChanges({ reply: afterEdit, dir, tree: "t1", entries, objectFormat: "sha1" }), false);
    // Restored to the commit's bytes, it stands again (the file is read and compared, not assumed).
    fs.writeFileSync(path.join(dir, "src", "a.txt"), "a");
    const restored = ask({ op: "check", dir, tree: "t1" });
    assert.equal(treeProvenByChanges({ reply: restored, dir, tree: "t1", entries, objectFormat: "sha1" }), true);

    // A record made with a stale generation is refused: something changed during the proof.
    fs.writeFileSync(path.join(dir, "b.txt"), "b");
    assert.equal(ask({ op: "tree-record", dir, tree: "t1", generation: restored.generation }), null);
  } finally {
    child.kill();
    await sleep(200);
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("one run hashes the tree once: the sync's proof is recorded, later proofs are answered or check only what changed, an edited commit file is still caught", async () => {
  const { execFileSync } = await import("node:child_process");
  const { syncSnapshot, verifySnapshot } = await import("./snapshot.mjs");
  const { verifySnapshotDir } = await import("./snapshot-check.mjs");
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-serve-run-")));
  const repo = path.join(base, "repo"); const root = path.join(base, "snapshots"); fs.mkdirSync(repo); fs.mkdirSync(root, { mode: 0o700 });
  const git = (...a) => execFileSync("git", a, { cwd: repo, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  git("init", "-q"); git("config", "user.email", "t@example.invalid"); git("config", "user.name", "t"); git("config", "core.autocrlf", "false");
  fs.mkdirSync(path.join(repo, "src")); fs.writeFileSync(path.join(repo, "src", "a.txt"), "a\n"); fs.writeFileSync(path.join(repo, "b.txt"), "b\n"); fs.writeFileSync(path.join(repo, ".gitignore"), "out/\n");
  git("add", "-A"); git("commit", "-q", "-m", "one");
  const commit = git("rev-parse", "HEAD").trim();
  const sock = process.platform === "win32" ? `\\\\.\\pipe\\rk-serve-run-test-${process.pid}-${Date.now()}` : path.join(base, "test.sock");
  const script = `import(${JSON.stringify(pathToFileURL(path.join(here, "gate-serve.mjs")).href)}).then((m) => m.serve({ root: ${JSON.stringify(root)}, listenOn: ${JSON.stringify(sock)} }));`;
  const child = spawn(process.execPath, ["--input-type=module", "-e", script], { stdio: "ignore" });
  const ask = (req) => serveRequest(req, { sock, env: {} });
  const counts = () => ({ ...SNAPSHOT_PROOF_COUNTS });
  try {
    let up = null;
    for (let i = 0; i < 100 && !up; i += 1) { up = ask({ op: "status" }); if (!up) await sleep(100); }
    assert.ok(up?.healthy, "the test server answers and watches");

    const start = counts();
    const synced = syncSnapshot({ checkout: repo, rev: commit, root, managedRoot: null, resident: ask });
    const { path: dir, tree } = synced;
    assert.equal(counts().treeHashes, start.treeHashes + 1, "the sync hashes the tree once");

    // The door's start of a stage and the check's own verify: both answered from the sync's recorded proof.
    assert.deepEqual(verifySnapshot({ checkout: repo, rev: commit, root, managedRoot: null, ledger: [], resident: ask }).ledger, []);
    assert.equal(verifySnapshotDir({ checkout: repo, dir, commit, tree, when: "before the check", resident: ask }), tree);
    assert.equal(counts().treeHashes, start.treeHashes + 1);
    assert.equal(counts().residentProofs, start.residentProofs + 2);

    // The stage writes an ignored output. Its end verify admits it; the tree is not hashed again.
    fs.mkdirSync(path.join(dir, "out")); fs.writeFileSync(path.join(dir, "out", "x.o"), "object");
    const end = verifySnapshot({ checkout: repo, rev: commit, root, managedRoot: null, ledger: [], admitNew: true, resident: ask });
    assert.deepEqual(end.ledger.map((e) => e.path).filter((p) => p.endsWith("x.o")), ["out/x.o"]);
    assert.equal(counts().treeHashes, start.treeHashes + 1, "an output beside the commit's files costs no second hash");
    assert.equal(counts().changedFileProofs, start.changedFileProofs + 1);

    // The next stage starts with that ledger: walked, and again no hash. A plain verify is not answered "nothing changed" while outputs exist.
    verifySnapshot({ checkout: repo, rev: commit, root, managedRoot: null, ledger: end.ledger, resident: ask });
    assert.equal(counts().treeHashes, start.treeHashes + 1);
    assert.equal(counts().changedFileProofs, start.changedFileProofs + 2);
    assert.equal(ask({ op: "check", dir, tree }).clean, false);

    // A commit file edited inside the snapshot: the change is named, its bytes are not the commit's, the tree is hashed and the verify refuses.
    fs.writeFileSync(path.join(dir, "src", "a.txt"), "edited\n");
    assert.throws(() => verifySnapshot({ checkout: repo, rev: commit, root, managedRoot: null, ledger: end.ledger, resident: ask }), /is tree [0-9a-f]{40}, not tree/);
    assert.equal(counts().treeHashes, start.treeHashes + 2);
  } finally {
    child.kill();
    await sleep(200);
    fs.rmSync(base, { recursive: true, force: true });
  }
});
