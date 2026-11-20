import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { test } from "node:test";
import { blobOid, createChangeLedger, entriesToRecheck, treeStandsAfterChanges } from "./snapshot-changes.mjs";

test("blobOid is git's own blob id", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-blob-"));
  try {
    const f = path.join(dir, "f.bin"); const bytes = Buffer.from("hello\r\nworld\n\0tail");
    fs.writeFileSync(f, bytes);
    const git = execFileSync("git", ["hash-object", "--no-filters", f], { encoding: "utf8" }).trim();
    assert.equal(blobOid(bytes), git);
    assert.equal(blobOid(Buffer.alloc(0)), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    assert.match(blobOid(bytes, "sha256"), /^[0-9a-f]{64}$/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("the ledger keeps changes and renames apart, by path relative to the snapshot", () => {
  const l = createChangeLedger();
  l.touch("change", "src\\a.rs"); l.touch("rename", "target/debug"); l.touch("change", "src/a.rs"); l.touch("change", "");
  assert.deepEqual(l.list(), { changed: ["src/a.rs"], renamed: ["target/debug"] });
});

test("a rename of the snapshot directory itself, and too many prefixes, leave nothing known", () => {
  const a = createChangeLedger(); a.touch("rename", "");
  assert.equal(a.list(), null); assert.equal(a.unknown, true);
  a.touch("change", "x"); assert.equal(a.list(), null);
  const b = createChangeLedger({ maxPaths: 3, maxPrefixes: 2 });
  for (const p of ["a/1/x", "a/1/y", "a/2/x"]) b.touch("change", p);
  assert.deepEqual(b.list(), { changed: ["a/1/x", "a/1/y", "a/2/x"], renamed: [] });
  b.touch("change", "a/2/y"); // past maxPaths: collapsed to two components, kept as renamed (everything under them is checked)
  assert.deepEqual(b.list(), { changed: [], renamed: ["a/1", "a/2"] });
  b.touch("change", "a/1/z/deep"); assert.deepEqual(b.list(), { changed: [], renamed: ["a/1", "a/2"] });
  b.touch("rename", "b/1/q"); assert.equal(b.list(), null);
});

test("a commit file is rechecked when it changed, or lies under something renamed; a changed directory alone touches no file", () => {
  const entries = [{ path: "src/a.rs" }, { path: "src/deep/b.rs" }, { path: "docs/c.md" }, { path: "Cargo.toml" }];
  const pick = (changes, platform = "linux") => entriesToRecheck(entries, changes, { platform }).map((e) => e.path);
  assert.deepEqual(pick({ changed: ["src/a.rs", "src", "node_modules/x/y.js"], renamed: [] }), ["src/a.rs"]);
  assert.deepEqual(pick({ changed: [], renamed: ["src"] }), ["src/a.rs", "src/deep/b.rs"]);
  assert.deepEqual(pick({ changed: [], renamed: ["src/deep", "target"] }), ["src/deep/b.rs"]);
  assert.deepEqual(pick({ changed: [], renamed: ["Cargo.toml", "app/node_modules"] }), ["Cargo.toml"]);
  assert.deepEqual(pick({ changed: ["SRC/A.rs"], renamed: ["DOCS"] }), []);
  assert.deepEqual(pick({ changed: ["SRC/A.rs"], renamed: ["DOCS"] }, "win32"), ["src/a.rs", "docs/c.md"]);
  assert.deepEqual(pick({ changed: [], renamed: [] }), []);
});

function snapshotOf(files) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-changes-")));
  const entries = [];
  for (const [rel, text] of Object.entries(files)) {
    const f = path.join(dir, ...rel.split("/")); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, text);
    entries.push({ path: rel, mode: "100644", oid: blobOid(Buffer.from(text)) });
  }
  return { dir, entries, done: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

test("outputs written beside the commit's files leave the proven tree standing, and no commit file is read", () => {
  const s = snapshotOf({ "src/a.rs": "a", "src/b.rs": "b", "Cargo.toml": "c" });
  try {
    let reads = 0;
    const r = treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes: { changed: ["target/debug/x.o", "src"], renamed: ["target", "app/node_modules"] }, read: (f) => { reads += 1; return fs.readFileSync(f); } });
    assert.deepEqual(r, { ok: true, checked: 0 }); assert.equal(reads, 0);
  } finally { s.done(); }
});

test("a touched commit file is compared with the commit's blob: same bytes stand, other bytes, a missing file and a directory in its place do not", () => {
  const s = snapshotOf({ "src/a.rs": "a", "src/b.rs": "b", "Cargo.toml": "c" });
  try {
    const run = (changes) => treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes });
    assert.deepEqual(run({ changed: ["src/a.rs"], renamed: [] }), { ok: true, checked: 1 }); // rewritten with the same bytes
    fs.writeFileSync(path.join(s.dir, "src", "a.rs"), "edited");
    assert.deepEqual(run({ changed: ["src/a.rs"], renamed: [] }), { ok: false, why: "src/a.rs is not the commit's bytes" });
    assert.deepEqual(run({ changed: ["src/b.rs"], renamed: [] }), { ok: true, checked: 1 }); // the edit is elsewhere: this proof is only as good as the change record
    fs.rmSync(path.join(s.dir, "src", "b.rs"));
    assert.deepEqual(run({ changed: [], renamed: ["src/b.rs"] }), { ok: false, why: "src/b.rs: ENOENT" });
    fs.mkdirSync(path.join(s.dir, "src", "b.rs"));
    assert.deepEqual(run({ changed: [], renamed: ["src/b.rs"] }), { ok: false, why: "src/b.rs is not a regular file" });
  } finally { s.done(); }
});

test("nothing known, every file touched, and a touched link are never answered here", () => {
  const s = snapshotOf({ "src/a.rs": "a", "link": "src/a.rs" });
  try {
    assert.equal(treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes: null }).ok, false);
    assert.equal(treeStandsAfterChanges({ dir: s.dir, entries: [], changes: { changed: [], renamed: [] } }).ok, false);
    assert.deepEqual(treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes: { changed: [], renamed: ["src", "link"] } }), { ok: false, why: "every file of the commit may have changed: that is a whole-tree hash" });
    const linked = s.entries.map((e) => (e.path === "link" ? { ...e, mode: "120000" } : e));
    assert.deepEqual(treeStandsAfterChanges({ dir: s.dir, entries: linked, changes: { changed: ["link"], renamed: [] } }), { ok: false, why: "link is a link" });
  } finally { s.done(); }
});

test("a commit file with a second name is compared whatever the change record says: a write through the other name is caught", () => {
  const s = snapshotOf({ "pkg/lib.js": "lib", "src/a.rs": "a", "Cargo.toml": "c" });
  try {
    const other = path.join(s.dir, "node_modules", "pkg"); fs.mkdirSync(other, { recursive: true });
    fs.linkSync(path.join(s.dir, "pkg", "lib.js"), path.join(other, "lib.js"));
    const changes = { changed: ["node_modules/pkg/lib.js"], renamed: ["node_modules"] }; // the watcher names the path that was written through
    assert.deepEqual(treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes }), { ok: true, checked: 1 });
    fs.writeFileSync(path.join(other, "lib.js"), "changed through the link");
    assert.deepEqual(treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes }), { ok: false, why: "pkg/lib.js is not the commit's bytes" });
  } finally { s.done(); }
});

test("a commit file that is gone is caught even when no change names it; the hash is chosen by the blob id's length", () => {
  const s = snapshotOf({ "src/a.rs": "a", "src/b.rs": "b", "Cargo.toml": "c" });
  try {
    fs.rmSync(path.join(s.dir, "src", "b.rs"));
    assert.deepEqual(treeStandsAfterChanges({ dir: s.dir, entries: s.entries, changes: { changed: [], renamed: [] } }), { ok: false, why: "src/b.rs: ENOENT" });
    fs.writeFileSync(path.join(s.dir, "src", "b.rs"), "b");
    const sha256Entries = s.entries.map((e) => (e.path === "src/a.rs" ? { ...e, oid: blobOid(Buffer.from("a"), "sha256") } : e));
    assert.deepEqual(treeStandsAfterChanges({ dir: s.dir, entries: sha256Entries, changes: { changed: ["src/a.rs"], renamed: [] } }), { ok: true, checked: 1 });
  } finally { s.done(); }
});
