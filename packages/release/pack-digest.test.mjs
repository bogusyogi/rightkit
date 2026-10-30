import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import zlib from "node:zlib";
import { tarOf, tgzOf } from "./test-support/tgz.mjs";
import { SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT, exclusionsFor, npmContentDigest, parseTar } from "./pack-digest.mjs";

const BASE = [
  { path: "package/package.json", data: '{"name":"@rightkit/x"}' },
  { path: "package/bin/cli.mjs", data: "#!/usr/bin/env node\n", mode: 0o755 },
  { path: "package/lib/a.mjs", data: "export const a = 1;\n" },
];

test("the content digest ignores entry order, mtimes and gzip, and reads exactly what a real tar writes", () => {
  const base = npmContentDigest(tgzOf(BASE));
  assert.match(base.contentSha256, /^[0-9a-f]{64}$/);
  assert.equal(base.entries, 3);
  assert.equal(npmContentDigest(tgzOf([...BASE].reverse())).contentSha256, base.contentSha256, "entry order");
  assert.equal(npmContentDigest(tgzOf(BASE.map((e) => ({ ...e, mtime: 1_700_000_000 })))).contentSha256, base.contentSha256, "mtime");
  assert.equal(npmContentDigest(tgzOf(BASE, 1)).contentSha256, base.contentSha256, "gzip level");
  assert.notEqual(zlib.gzipSync(tarOf(BASE), { level: 1 }).toString("hex"), zlib.gzipSync(tarOf(BASE), { level: 9 }).toString("hex"));
  // A real tar (bsdtar or GNU tar), in another order, through the system gzip: the same digest.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pack-digest-"));
  try {
    for (const e of BASE) { fs.mkdirSync(path.dirname(path.join(dir, e.path)), { recursive: true }); fs.writeFileSync(path.join(dir, e.path), e.data, { mode: e.mode ?? 0o644 }); fs.chmodSync(path.join(dir, e.path), e.mode ?? 0o644); }
    const out = path.join(dir, "real.tgz");
    const made = spawnSync("tar", ["-czf", out, "-C", dir, "package/lib/a.mjs", "package/bin/cli.mjs", "package/package.json"], { encoding: "utf8" });
    if (made.status === 0) assert.equal(npmContentDigest(out).contentSha256, base.contentSha256, "a tarball written by the system tar");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("the content digest changes with the exec bit, a path, a content byte, a type, an added or removed entry", () => {
  const base = npmContentDigest(tgzOf(BASE)).contentSha256;
  const differ = (name, entries) => assert.notEqual(npmContentDigest(tgzOf(entries)).contentSha256, base, name);
  differ("exec bit cleared", BASE.map((e) => (e.path.endsWith("cli.mjs") ? { ...e, mode: 0o644 } : e)));
  differ("exec bit set", BASE.map((e) => (e.path.endsWith("a.mjs") ? { ...e, mode: 0o755 } : e)));
  differ("renamed path", BASE.map((e) => (e.path.endsWith("a.mjs") ? { ...e, path: "package/lib/b.mjs" } : e)));
  differ("one content byte", BASE.map((e) => (e.path.endsWith("a.mjs") ? { ...e, data: "export const a = 2;\n" } : e)));
  differ("a symlink instead of a file", BASE.map((e) => (e.path.endsWith("a.mjs") ? { ...e, type: "2", data: "" } : e)));
  differ("an extra file", [...BASE, { path: "package/extra.txt", data: "x" }]);
  differ("a missing file", BASE.slice(1));
  // Path and content are not interchangeable: swapping which file holds which bytes changes the digest.
  differ("contents swapped between paths", [{ ...BASE[0], data: BASE[2].data }, BASE[1], { ...BASE[2], data: BASE[0].data }]);
});

test("exclusions are code constants for @rightkit/release alone; an excluded entry is reported, never digested", () => {
  assert.deepEqual(exclusionsFor("@rightkit/release"), [SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT]);
  assert.equal(SWAP_HELPER_ARCHIVE_BIN, "package/bin/darwin/rightkit-swap");
  assert.equal(SWAP_HELPER_ARCHIVE_RECEIPT, "package/bin/darwin/rightkit-swap.sign.json");
  for (const other of ["@rightkit/ci", "@rightkit/qa", "rightkit-fs", "@rightkit/release-extra", ""]) assert.deepEqual(exclusionsFor(other), [], other);
  const withHelper = (helper, receipt) => [...BASE, { path: SWAP_HELPER_ARCHIVE_BIN, data: helper, mode: 0o755 }, ...(receipt ? [{ path: SWAP_HELPER_ARCHIVE_RECEIPT, data: receipt }] : [])];
  const exclude = exclusionsFor("@rightkit/release");
  const a = npmContentDigest(tgzOf(withHelper("signed-1", '{"t":1}')), { exclude });
  const b = npmContentDigest(tgzOf(withHelper("signed-2", '{"t":2}')), { exclude });
  assert.equal(a.contentSha256, b.contentSha256, "a re-signed helper and its receipt do not move the digest");
  assert.equal(a.contentSha256, npmContentDigest(tgzOf(BASE)).contentSha256, "and the digest is that of the rest of the archive");
  assert.deepEqual(Object.keys(a.excluded).sort(), [SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT].sort());
  assert.equal(a.excluded[SWAP_HELPER_ARCHIVE_BIN].exec, true);
  assert.deepEqual(Object.keys(npmContentDigest(tgzOf(withHelper("signed-1")), { exclude }).excluded), [SWAP_HELPER_ARCHIVE_BIN], "no receipt, no receipt reported");
  // The same archive for any other unit digests the helper like any entry.
  assert.notEqual(npmContentDigest(tgzOf(withHelper("signed-1", '{"t":1}'))).contentSha256, npmContentDigest(tgzOf(withHelper("signed-2", '{"t":2}'))).contentSha256);
  // Any other differing entry still moves the digest of the exclusion-aware reading.
  const other = tgzOf([...withHelper("signed-1", '{"t":1}').map((e) => (e.path.endsWith("a.mjs") ? { ...e, data: "changed" } : e))]);
  assert.notEqual(npmContentDigest(other, { exclude }).contentSha256, a.contentSha256);
});

test("an archive that reads two ways or not at all has no digest", () => {
  assert.throws(() => npmContentDigest(Buffer.from("not gzip")), /not a readable gzip/);
  assert.throws(() => npmContentDigest(tgzOf([BASE[0], BASE[0]])), /holds package\/package.json twice/);
  const bad = tarOf(BASE); bad[10] ^= 0xff;
  assert.throws(() => parseTar(bad), /bad checksum/);
  assert.throws(() => parseTar(tarOf(BASE).subarray(0, 600)), /truncated/);
  assert.throws(() => npmContentDigest(tgzOf([{ path: "package/dev", type: "3" }])), /unsupported type/);
  assert.deepEqual(parseTar(tarOf([{ path: "package/", type: "5", mode: 0o755 }])).map((e) => [e.path, e.type]), [["package", "directory"]]);
});
