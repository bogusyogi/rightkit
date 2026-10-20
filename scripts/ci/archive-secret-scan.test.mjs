import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { gzipSync, gunzipSync } from "node:zlib";
import { readArchiveEntries, scanArchiveForSecrets } from "./archive-secret-scan.mjs";

function tar(items, { terminated = true } = {}) {
  const blocks = [];
  for (const item of items) {
    const bytes = Buffer.from(item.content ?? "");
    const header = Buffer.alloc(512);
    header.write(item.path, 0, 100);
    header.write("0000600\0", 100);
    header.write("0000000\0", 108);
    header.write("0000000\0", 116);
    header.write(`${(item.size ?? bytes.length).toString(8).padStart(11, "0")}\0`, 124);
    header.write("00000000000\0", 136);
    header.fill(32, 148, 156);
    header[156] = (item.type ?? "0").charCodeAt(0);
    if (item.link) header.write(item.link, 157, 100);
    header.write("ustar\0", 257);
    header.write("00", 263);
    const checksum = header.reduce((sum, byte) => sum + byte, 0);
    header.write(`${checksum.toString(8).padStart(6, "0")}\0 `, 148);
    blocks.push(header, bytes, Buffer.alloc((512 - bytes.length % 512) % 512));
  }
  if (terminated) blocks.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(blocks));
}
function pax(key, value) {
  const payload = `${key}=${value}\n`;
  let length = Buffer.byteLength(payload) + 2;
  while (Buffer.byteLength(`${length} ${payload}`) !== length) length = Buffer.byteLength(`${length} ${payload}`);
  return `${length} ${payload}`;
}
const digest = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const fixtureOwner = {
  packageFileViolations(paths) { return paths.filter((name) => name === ".env" || name.startsWith("node_modules/")); },
  scanCrateForSecrets(dir) {
    return fs.readdirSync(dir).filter((name) => fs.readFileSync(path.join(dir, name), "utf8").includes("fixture-sensitive-value"));
  },
};
async function scan(bytes, extra = {}) {
  // Root can supply RIGHTKIT_TEST_SCRATCH=D:/scratch (or /Volumes/D/scratch).
  const parent = process.env.RIGHTKIT_TEST_SCRATCH ?? (process.platform === "win32" ? "D:/scratch" : "/Volumes/D/scratch");
  const scratchRoot = fs.mkdtempSync(path.join(parent, "archive-scan-fixture-"));
  try {
    const report = await scanArchiveForSecrets({ file: "fixture.tgz", registry: "npm", scratchRoot, scannerOwner: fixtureOwner, readArchive: () => bytes, ...extra });
    assert.deepEqual(fs.readdirSync(scratchRoot), []);
    return report;
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
}

test("npm & Cargo archive reports bind exact compressed bytes", async () => {
  for (const [registry, root] of [["npm", "package"], ["cargo", "demo-1.2.3"]]) {
    const bytes = tar([{ path: `${root}/manifest`, content: "safe" }]);
    const report = await scan(bytes, { registry, name: "demo", version: "1.2.3" });
    assert.equal(report.status, "pass");
    assert.equal(report.archiveSha256, digest(bytes));
    assert.equal(report.files, 1);
  }
});
test("shared path helper & scanner receive original paths & every file's bytes", async () => {
  const bytes = tar([
    { path: "package/.env", content: "safe" },
    { path: "package/.git/config", content: "fixture-sensitive-value" },
    { path: "package/node_modules/child/index.js", content: "safe" },
  ]);
  const report = await scan(bytes);
  assert.equal(report.status, "fail");
  assert.deepEqual(report.violations, [".env", ".git/config", "node_modules/child/index.js"]);
  assert.ok(!JSON.stringify(report).includes("fixture-sensitive-value"));
});
test("path helper checks explicit empty directories", async () => {
  const report = await scan(tar([{ path: "package/.env/", type: "5" }, { path: "package/file", content: "safe" }]));
  assert.deepEqual(report.violations, [".env"]);
  assert.equal(report.status, "fail");
});
test("unsafe archive paths, aliases, links & special entries fail before scanner", async () => {
  const cases = [
    [{ path: "package/../escape" }], [{ path: "/absolute" }], [{ path: "package/C:stream" }],
    [{ path: "package/back\\slash" }], [{ path: "package/./file" }], [{ path: "other/file" }],
    [{ path: "package/file " }], [{ path: "package/NUL" }],
    [{ path: "package/alias", type: "2", link: "../outside" }],
    [{ path: "package/alias", type: "1", link: "package/file" }],
    [{ path: "package/device", type: "3" }], [{ path: "package/sparse", type: "S" }],
    [{ path: "package/file" }, { path: "package/file" }],
    [{ path: "package/File" }, { path: "package/file" }],
    [{ path: "package/\u00e9" }, { path: "package/e\u0301" }],
    [{ path: "package/file" }, { path: "package/file/child" }],
    [{ path: "package/file/child" }, { path: "package/file" }],
  ];
  for (const items of cases) {
    let calls = 0;
    const report = await scan(tar(items), { scannerOwner: { ...fixtureOwner, scanCrateForSecrets() { calls++; return []; } } });
    assert.equal(report.status, "fail", JSON.stringify(items));
    assert.equal(calls, 0);
  }
});
test("PAX & GNU long paths are checked after effective-name normalization", async () => {
  const safe = await scan(tar([{ path: "PaxHeader", type: "x", content: pax("path", "package/long/name") }, { path: "placeholder", content: "safe" }]));
  assert.equal(safe.status, "pass");
  for (const item of [
    { path: "PaxHeader", type: "x", content: pax("path", "package/../../escape") },
    { path: "LongLink", type: "L", content: "package/../escape\0" },
    { path: "PaxHeader", type: "x", content: pax("GNU.sparse.map", "0,1") },
  ]) assert.equal((await scan(tar([item, { path: "package/file" }]))).status, "fail");
});
test("corrupt, truncated, oversized & unscannable archives fail closed without error text", async () => {
  const valid = tar([{ path: "package/file", content: "payload" }]);
  for (const bytes of [Buffer.from("invalid gzip"), valid.subarray(0, valid.length - 4), tar([{ path: "package/file" }], { terminated: false }), tar([{ path: "package/file", size: 1024 }])]) {
    assert.equal((await scan(bytes)).status, "fail");
  }
  assert.equal((await scan(valid, { limits: { compressedBytes: 1 } })).status, "fail");
  assert.equal((await scan(valid, { limits: { expandedBytes: 512 } })).status, "fail");
  assert.equal((await scan(valid, { limits: { fileBytes: 1 } })).status, "fail");
  assert.equal((await scan(valid, { limits: { entries: 0 } })).status, "fail");
  const failed = await scan(valid, { scannerOwner: { ...fixtureOwner, scanCrateForSecrets() { throw new Error("fixture-sensitive-value"); } } });
  assert.equal(failed.status, "fail");
  assert.ok(!JSON.stringify(failed).includes("fixture-sensitive-value"));
  assert.equal((await scan(valid, { readArchive() { throw new Error("unreadable"); } })).archiveSha256, null);
  assert.equal((await scan(valid, { scannerOwner: {} })).status, "fail");
});
test("tar parser rejects checksum mismatch & nonzero trailer", () => {
  const valid = tar([{ path: "package/file", content: "safe" }]);
  const checksum = gunzipSync(valid);
  checksum[0] ^= 1;
  assert.throws(() => readArchiveEntries(gzipSync(checksum), { root: "package" }));
  const trailer = gunzipSync(valid);
  trailer[trailer.length - 1] = 1;
  assert.throws(() => readArchiveEntries(gzipSync(trailer), { root: "package" }));
});
