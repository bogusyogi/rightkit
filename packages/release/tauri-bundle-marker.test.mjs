import assert from "node:assert/strict";
import test from "node:test";
import { BUNDLE_MARKERS, patchTauriBundleType, patchTauriBundleTypeForNsis, readTauriBundleMarkers } from "./tauri-bundle-marker.mjs";

/** A stand-in binary: filler, one marker, filler. */
function fakeBinary(code = BUNDLE_MARKERS.unknown, { copies = 1 } = {}) {
  const parts = [Buffer.alloc(64, 0x41)];
  for (let index = 0; index < copies; index += 1) {
    parts.push(Buffer.from(`__TAURI_BUNDLE_TYPE_VAR_${code}`, "ascii"), Buffer.alloc(32, 0x42));
  }
  return Buffer.concat(parts);
}

function memoryFs(initial) {
  const store = new Map([["binary.exe", Buffer.from(initial)]]);
  return {
    store,
    read: (file) => {
      const key = file.endsWith("binary.exe") ? "binary.exe" : file;
      const value = store.get(key);
      if (!value) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
      return Buffer.from(value);
    },
    write: (file, bytes) => {
      store.set(file.endsWith("binary.exe") ? "binary.exe" : file, Buffer.from(bytes));
    },
  };
}

test("rewrites the single unknown marker to the NSIS marker and nothing else", () => {
  const fs = memoryFs(fakeBinary());
  const before = fs.read("binary.exe");
  const receipt = patchTauriBundleTypeForNsis("binary.exe", { read: fs.read, write: fs.write });
  const after = fs.read("binary.exe");

  assert.equal(receipt.alreadyPatched, false);
  assert.equal(receipt.bundle, "nsis");
  assert.equal(receipt.changedBytes, 3, "only the 3-character bundle code may change");
  assert.equal(after.length, before.length, "patching must not resize the binary");
  assert.equal(after.includes(Buffer.from("__TAURI_BUNDLE_TYPE_VAR_NSS")), true);
  assert.equal(after.includes(Buffer.from("__TAURI_BUNDLE_TYPE_VAR_UNK")), false);
  assert.notEqual(receipt.beforeSha256, receipt.afterSha256);
});

test("is idempotent: an already-patched binary is reported, not rewritten", () => {
  const fs = memoryFs(fakeBinary(BUNDLE_MARKERS.nsis));
  const receipt = patchTauriBundleTypeForNsis("binary.exe", { read: fs.read, write: fs.write });
  assert.equal(receipt.alreadyPatched, true);
  assert.equal(receipt.changedBytes, 0);
  assert.equal(receipt.beforeSha256, receipt.afterSha256, "an already-patched binary keeps its signed bytes");
});

test("fails closed when the binary carries more than one unknown marker", () => {
  const fs = memoryFs(fakeBinary(BUNDLE_MARKERS.unknown, { copies: 2 }));
  assert.throws(
    () => patchTauriBundleTypeForNsis("binary.exe", { read: fs.read, write: fs.write }),
    /found 2/,
  );
});

test("fails closed when no marker of any kind is present", () => {
  const fs = memoryFs(Buffer.alloc(128, 0x41));
  assert.throws(
    () => patchTauriBundleTypeForNsis("binary.exe", { read: fs.read, write: fs.write }),
    /refusing to guess the bundle identity/,
  );
});

test("rejects an unsupported bundle kind before touching the file", () => {
  const fs = memoryFs(fakeBinary());
  assert.throws(() => patchTauriBundleType("binary.exe", { bundle: "zip", read: fs.read, write: fs.write }), /unsupported Tauri bundle kind/);
  assert.equal(fs.read("binary.exe").includes(Buffer.from("__TAURI_BUNDLE_TYPE_VAR_UNK")), true);
});

test("readTauriBundleMarkers reports offsets without writing", () => {
  const fs = memoryFs(fakeBinary(BUNDLE_MARKERS.nsis));
  const report = readTauriBundleMarkers("binary.exe", { read: fs.read });
  assert.deepEqual(report.markers.nsis, [64]);
  assert.equal(report.markers.unknown, undefined);
  assert.match(report.sha256, /^[0-9a-f]{64}$/);
});
