import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CATALOG, ortRuntimeEnv, pickMember, provision, recordsFor } from "./provision-ort.mjs";

const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");
const ARCHIVE = Buffer.from("archive bytes");
const DLL = "the x64 runtime";

function record(overrides = {}) {
  return {
    id: "onnxruntime-9.9.9-windows-x64", library: "onnxruntime", os: "windows", arch: "x86_64", status: "pinned",
    artifactFileName: "onnxruntime.dll", sha256: sha(DLL),
    archive: { url: "https://example.invalid/runtime.nupkg", sha256: sha(ARCHIVE) },
    ...overrides,
  };
}
const response = (bytes, ok = true) => ({ ok, status: ok ? 200 : 404, arrayBuffer: async () => bytes });
/** Stands in for the unzip: writes an x64 and an arm64 build under the same file name. */
function fakeExtract(_zip, out) {
  for (const [folder, text] of [["win-arm64", "the arm64 runtime"], ["win-x64", DLL]]) {
    fs.mkdirSync(path.join(out, "runtimes", folder, "native"), { recursive: true });
    fs.writeFileSync(path.join(out, "runtimes", folder, "native", "onnxruntime.dll"), text);
  }
}
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-ort-prov-"));

test("the repository catalog has exactly one resolvable Windows x64 runtime record, with an archive", () => {
  const [runtime] = recordsFor(JSON.parse(fs.readFileSync(CATALOG, "utf8")));
  assert.equal(runtime.library, "onnxruntime");
  assert.equal(runtime.artifactFileName, "onnxruntime.dll");
  assert.match(runtime.archive.url, /^https:\/\//);
});

test("only approved or pinned records for the platform count, and the runtime record must be single", () => {
  const others = [record({ id: "mac", os: "macos", arch: "aarch64" }), record({ id: "planned", status: "planned" }), record({ id: "stale", status: "stale-unverified" })];
  assert.deepEqual(recordsFor({ libraries: [...others, record()] }).map((r) => r.id), ["onnxruntime-9.9.9-windows-x64"]);
  assert.throws(() => recordsFor({ libraries: others }), /has 0 approved or pinned onnxruntime records/);
  assert.throws(() => recordsFor({ libraries: [record(), record({ id: "second" })] }), /has 2 approved or pinned/);
});

test("a directml record for the platform is provisioned after the runtime", () => {
  const dml = record({ id: "directml-1", library: "directml", artifactFileName: "DirectML.dll" });
  assert.deepEqual(recordsFor({ libraries: [dml, record()] }).map((r) => r.id), ["onnxruntime-9.9.9-windows-x64", "directml-1"]);
});

test("a record without an https archive digest, or with a path as its file name, is refused", () => {
  assert.throws(() => recordsFor({ libraries: [record({ archive: undefined })] }), /no https archive/);
  assert.throws(() => recordsFor({ libraries: [record({ archive: { url: "http://example.invalid/a", sha256: sha("a") } })] }), /no https archive/);
  assert.throws(() => recordsFor({ libraries: [record({ artifactFileName: "..\\onnxruntime.dll" })] }), /no file name and sha256/);
  assert.throws(() => recordsFor({ libraries: [record({ sha256: "" })] }), /no file name and sha256/);
});

test("the member is chosen by name and digest together", () => {
  const members = [{ path: "a/arm64/onnxruntime.dll", sha256: sha("arm") }, { path: "a/x64/onnxruntime.dll", sha256: sha(DLL) }, { path: "a/x64/other.dll", sha256: sha(DLL) }];
  assert.equal(pickMember(members, record()), "a/x64/onnxruntime.dll");
  assert.throws(() => pickMember(members.slice(0, 1), record()), /holds 1 file\(s\) named onnxruntime.dll, none with the pinned sha256/);
});

test("provision verifies the archive, keeps only the pinned file, and does not download twice", async () => {
  const root = tmp();
  try {
    let downloads = 0;
    const fetchImpl = async () => { downloads += 1; return response(ARCHIVE); };
    const dir = await provision({ root, catalog: { libraries: [record()] }, fetchImpl, extract: fakeExtract });
    assert.equal(dir, path.join(root, "onnxruntime-9.9.9-windows-x64"));
    assert.deepEqual(fs.readdirSync(dir), ["onnxruntime.dll"]);
    assert.equal(fs.readFileSync(path.join(dir, "onnxruntime.dll"), "utf8"), DLL);
    assert.deepEqual(fs.readdirSync(root), ["onnxruntime-9.9.9-windows-x64"], "no staging directory is left behind");
    await provision({ root, catalog: { libraries: [record()] }, fetchImpl, extract: fakeExtract });
    assert.equal(downloads, 1);
    // A file that no longer carries the pinned digest is replaced.
    fs.writeFileSync(path.join(dir, "onnxruntime.dll"), "tampered");
    await provision({ root, catalog: { libraries: [record()] }, fetchImpl, extract: fakeExtract });
    assert.equal(downloads, 2);
    assert.equal(fs.readFileSync(path.join(dir, "onnxruntime.dll"), "utf8"), DLL);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("a wrong archive digest, a failed download and an archive without the pinned file all refuse and leave nothing", async () => {
  for (const [fetchImpl, extract, message] of [
    [async () => response(Buffer.from("other bytes")), fakeExtract, /is not the pinned/],
    [async () => response(ARCHIVE, false), fakeExtract, /download failed: HTTP 404/],
    [async () => response(ARCHIVE), (_zip, out) => fs.writeFileSync(path.join(out, "onnxruntime.dll"), "wrong build"), /none with the pinned sha256/],
  ]) {
    const root = tmp();
    try {
      await assert.rejects(provision({ root, catalog: { libraries: [record()] }, fetchImpl, extract }), message);
      assert.deepEqual(fs.readdirSync(path.join(root, "onnxruntime-9.9.9-windows-x64")), []);
      assert.deepEqual(fs.readdirSync(root), ["onnxruntime-9.9.9-windows-x64"]);
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  }
});

test("hosts other than Windows x64 provision nothing", async () => {
  assert.deepEqual(await ortRuntimeEnv({ platform: "darwin", arch: "arm64" }), {});
  assert.deepEqual(await ortRuntimeEnv({ platform: "win32", arch: "arm64" }), {});
});
