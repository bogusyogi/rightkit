import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { gitFixture } from "./release-qa-fixtures.mjs";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const gitModule = pathToFileURL(path.join(here, "snapshot-git.mjs")).href;
const memoModule = pathToFileURL(path.join(here, "probe-memo.mjs")).href;

// One gate process: the sealed whole-tree listing exactly as snapshot.mjs treeEntries asks it (sealedRun, fresh private directory each call).
const child = (repo, storeDir, sha) => JSON.parse(execFileSync(process.execPath, ["--input-type=module", "-e", `
  const { sealedRun, setTreeListingStore } = await import(${JSON.stringify(gitModule)}); const { contentStore } = await import(${JSON.stringify(memoModule)});
  const stats = setTreeListingStore(contentStore(${JSON.stringify(storeDir)}));
  const r = sealedRun(${JSON.stringify(repo)}, ["ls-tree", "-r", "-z", "--full-tree", ${JSON.stringify(sha)}]);
  process.stdout.write(JSON.stringify({ status: r.status, stored: stats.stored, pid: r.pid, length: r.stdout.length, head: r.stdout.slice(0, 80) }));
`], { encoding: "utf8", maxBuffer: 1 << 28 }));

test("NN22: a sealed listing of a commit is kept between gate processes although each call runs in a new private directory", (t) => {
  const repo = gitFixture();
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const sha = execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim();
  const storeDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-sealed-store-"));
  try {
    const first = child(repo, storeDir, sha);
    assert.equal(first.status, 0); assert.equal(first.stored, 0); assert.notEqual(first.pid, 0);
    const second = child(repo, storeDir, sha);
    assert.equal(second.stored, 1, "the second process lists nothing"); assert.equal(second.pid, 0);
    assert.equal(second.length, first.length); assert.equal(second.head, first.head);
    // A kept file whose bytes no longer match its hash is never used: the process lists again.
    const [file] = fs.readdirSync(storeDir).filter((n) => n.endsWith(".bin"));
    const raw = fs.readFileSync(path.join(storeDir, file)); raw[raw.length - 1] ^= 1; fs.writeFileSync(path.join(storeDir, file), raw);
    const third = child(repo, storeDir, sha);
    assert.equal(third.stored, 0); assert.notEqual(third.pid, 0); assert.equal(third.head, first.head);
  } finally { fs.rmSync(storeDir, { recursive: true, force: true }); }
});

test("NN22: a stored answer is bound to its key: a valid file under another key's name is never returned (astra62 review 2)", async () => {
  const { contentStore } = await import(memoModule);
  const storeDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-content-store-"));
  try {
    const store = contentStore(storeDir);
    store.put("listing-of-a", Buffer.from("a\0")); store.put("listing-of-b", Buffer.from("b\0"));
    assert.equal(store.get("listing-of-a").toString(), "a\0");
    const name = (k) => path.join(storeDir, `${createHash("sha256").update(k).digest("hex")}.bin`);
    fs.copyFileSync(name("listing-of-a"), name("listing-of-b"));
    assert.equal(store.get("listing-of-b"), null, "a's record under b's name");
  } finally { fs.rmSync(storeDir, { recursive: true, force: true }); }
});
