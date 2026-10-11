import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  QA_CONTROL_MARKER, assertNoQaControl, bufferHasQaControlMarker, fileHasQaControlMarker, isNativeBinaryHeader, scanForQaControl,
} from "./qa-control-scan.mjs";

const MACHO = Buffer.from([0xcf, 0xfa, 0xed, 0xfe]);
const PE = Buffer.from("MZ");
const filler = (n) => Buffer.alloc(n, 0x90);
const withMarker = (header, before = 100, after = 100) =>
  Buffer.concat([header, filler(before), Buffer.from(QA_CONTROL_MARKER), filler(after)]);
const without = (header) => Buffer.concat([header, filler(500), Buffer.from("RIGHTKIT-QA-CONTROL")]);

test("marker is the fixed string the Rust crate emits", () => {
  assert.equal(QA_CONTROL_MARKER, "RIGHTKIT-QA-CONTROL-SERVER-PRESENT");
});

test("native headers: Mach-O thin and fat, PE, ELF; not text", () => {
  for (const head of [[0xfe, 0xed, 0xfa, 0xcf], [0xcf, 0xfa, 0xed, 0xfe], [0xca, 0xfe, 0xba, 0xbe], [0x7f, 0x45, 0x4c, 0x46], [0x4d, 0x5a, 0, 0]]) {
    assert.equal(isNativeBinaryHeader(Buffer.from(head)), true);
  }
  assert.equal(isNativeBinaryHeader(Buffer.from("text")), false);
});

test("a Mach-O and a PE fixture containing the marker are findings; clean ones are not", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const bad = [join(root, "app-macho"), join(root, "app.exe")];
  const good = [join(root, "clean-macho"), join(root, "clean.exe")];
  writeFileSync(bad[0], withMarker(MACHO));
  writeFileSync(bad[1], withMarker(PE));
  writeFileSync(good[0], without(MACHO));
  writeFileSync(good[1], without(PE));
  assert.deepEqual(scanForQaControl(good), []);
  const findings = scanForQaControl(bad);
  assert.equal(findings.length, 2);
  assert.throws(() => assertNoQaControl(bad, "release"), /QA control server must never ship/);
  assert.doesNotThrow(() => assertNoQaControl(good, "release"));
});

test("a .app directory is walked recursively; a text file with the marker is not a binary", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const exe = join(root, "X.app", "Contents", "MacOS");
  mkdirSync(exe, { recursive: true });
  writeFileSync(join(exe, "x"), withMarker(MACHO));
  writeFileSync(join(root, "X.app", "Contents", "notes.txt"), QA_CONTROL_MARKER);
  const findings = scanForQaControl([join(root, "X.app")]);
  assert.deepEqual(findings.map((f) => f.file), [join(exe, "x")]);
});

test("a marker split across the chunk boundary is found, and symlinks are not followed", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const big = join(root, "big");
  const chunk = 4 * 1024 * 1024;
  writeFileSync(big, Buffer.concat([MACHO, filler(chunk - 4 - 10), Buffer.from(QA_CONTROL_MARKER), filler(20)]));
  assert.equal(fileHasQaControlMarker(big), true);
  const link = join(root, "link");
  symlinkSync(big, link);
  const dir = join(root, "d");
  mkdirSync(dir);
  symlinkSync(big, join(dir, "l"));
  assert.deepEqual(scanForQaControl([dir]), []);
});

test("an unreadable or missing path fails closed", () => {
  const findings = scanForQaControl([join(tmpdir(), "rk-qa-scan-does-not-exist")]);
  assert.equal(findings.length, 1);
  assert.match(findings[0].reason, /could not scan/);
});

test("buffer helper", () => {
  assert.equal(bufferHasQaControlMarker(withMarker(PE)), true);
  assert.equal(bufferHasQaControlMarker(without(PE)), false);
});

test("hardeningscan fails an artifact that carries the marker and passes one that does not", async () => {
  const { spawnSync } = await import("./background-process.mjs");
  const root = mkdtempSync(join(tmpdir(), "rk-qa-scan-"));
  const bad = join(root, "bad.exe");
  const good = join(root, "good.exe");
  writeFileSync(bad, withMarker(PE));
  writeFileSync(good, without(PE));
  const scanner = join(import.meta.dirname, "hardeningscan.mjs");
  const failed = spawnSync(process.execPath, [scanner, bad], { cwd: root, encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /QA control server shipped/);
  const passed = spawnSync(process.execPath, [scanner, good], { cwd: root, encoding: "utf8" });
  assert.equal(passed.status, 0, passed.stderr);
});

test("a sealed release containing the marker fails verification (every seal and upload path)", async () => {
  const { verifySealedRelease } = await import("./release-state.mjs");
  const { createHash } = await import("node:crypto");
  const make = (bytes) => {
    const dir = mkdtempSync(join(tmpdir(), "rk-qa-sealed-"));
    writeFileSync(join(dir, "App.exe"), bytes);
    writeFileSync(join(dir, "release-manifest.json"), JSON.stringify({
      schema: 1, checkpoints: ["sealed"],
      files: [{ name: "App.exe", sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: bytes.length }],
    }));
    return dir;
  };
  assert.doesNotThrow(() => verifySealedRelease(make(without(PE))));
  assert.throws(() => verifySealedRelease(make(withMarker(PE))), /QA control server/);
});
