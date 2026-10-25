import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  StageSidecarError, builtBinaryCandidates, defaultExtractMember, defaultRun, isNestedBrokerContext, normalizeArchiveMember, parseTargetTriple, planSidecarStaging, resolveBuildMode, stageSidecars, validateSidecarManifest, validateStageSidecarReceipt,
} from "./stage-sidecar.mjs";
import { formatSidecarPlan, nodeHostTriple, parseStageSidecarArgs } from "./stage-sidecar-command.mjs";
import { contentIdentity } from "./content-digest.mjs";
import { uncoveredBinaries } from "./binary-license-notices.mjs";

const sha = (value) => createHash("sha256").update(value).digest("hex");
const APP = path.resolve("/rk-fake-app");
const at = (...parts) => path.join(APP, ...parts);
const MAC = "aarch64-apple-darwin";
const WIN = "x86_64-pc-windows-msvc";

function fakeFs(initial = {}) {
  const files = new Map(Object.entries(initial).map(([file, bytes]) => [path.resolve(file), Buffer.from(bytes)]));
  const log = { copies: [], chmods: [], writes: [], removed: [] };
  const isDir = (dir) => [...files.keys()].some((key) => key.startsWith(path.resolve(dir) + path.sep));
  const fs = {
    existsSync: (file) => files.has(path.resolve(file)) || isDir(file),
    mkdirSync: () => {},
    readdirSync: (dir) => [...new Set([...files.keys()].filter((key) => key.startsWith(path.resolve(dir) + path.sep)).map((key) => key.slice(path.resolve(dir).length + 1).split(path.sep)[0]))],
    rmSync: (target) => {
      const root = path.resolve(target);
      for (const key of [...files.keys()]) if (key === root || key.startsWith(root + path.sep)) { files.delete(key); log.removed.push(key); }
    },
    readFileSync: (file) => {
      if (!files.has(path.resolve(file))) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" });
      return files.get(path.resolve(file));
    },
    writeFileSync: (file, bytes) => { log.writes.push(path.resolve(file)); files.set(path.resolve(file), Buffer.from(bytes)); },
    copyFileSync: (from, to) => { log.copies.push(path.resolve(to)); files.set(path.resolve(to), files.get(path.resolve(from))); },
    renameSync: (from, to) => { files.set(path.resolve(to), files.get(path.resolve(from))); files.delete(path.resolve(from)); },
    chmodSync: (file, mode) => { log.chmods.push([path.resolve(file), mode]); },
    statSync: (file) => ({ isFile: () => files.has(path.resolve(file)), isDirectory: () => !files.has(path.resolve(file)) && isDir(file), size: files.get(path.resolve(file))?.length ?? 0 }),
  };
  return { fs, files, log };
}

const stream = (bin, executable) => `${JSON.stringify({ reason: "compiler-artifact", executable, target: { name: bin, kind: ["bin"] } })}\n`;

/** A fake `rightkit` runner: records calls; `onBuild` returns { stdout } and may seed the fake fs. */
function fakeRun({ onBuild, targetDir = path.resolve("/rk-fake-target") } = {}) {
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (command === "rustright" && args[0] === "cargo" && args[1] === "metadata") return { status: 0, stdout: JSON.stringify({ target_directory: targetDir }), stderr: "" };
    if (command === "rustright" && args[0] === "cargo" && args[1] === "build") return { status: 0, stderr: "", ...onBuild(args) };
    return { status: 0, stdout: "", stderr: "" };
  };
  return { run, calls };
}

const deps = (extra) => ({ now: () => "2026-10-17T00:00:00.000Z", pid: 7, log: () => {}, ...extra });

test("parseTargetTriple maps triples, accepts universal and refuses unknown", () => {
  assert.deepEqual(parseTargetTriple(WIN), { triple: WIN, platform: "win", os: "windows", arch: "x86_64" });
  assert.deepEqual(parseTargetTriple("universal-apple-darwin"), { triple: "universal-apple-darwin", platform: "mac", os: "macos", arch: "universal" });
  assert.throws(() => parseTargetTriple("riscv64-unknown-none"), { code: "STAGE_SIDECAR_TARGET_UNSUPPORTED" });
});

test("validation reports every problem at once with a typed error", () => {
  let error;
  try {
    validateSidecarManifest([
      { kind: "nope" },
      { kind: "cargo-bin", package: "a b", features: "x", extra: 1 },
      { kind: "cargo-bin", package: "dup" },
      { kind: "cargo-bin", package: "dup" },
      { kind: "resource", from: "../x", to: "/abs", sha256: "XYZ" },
      { kind: "onnxruntime", version: "1" },
      { kind: "ffmpeg", stage: { cmd: "cargo", args: [] }, notices: { dir: "n", sourceOfferContact: "a@b.co" } },
    ], { target: MAC });
  } catch (caught) { error = caught; }
  assert.ok(error instanceof StageSidecarError);
  assert.equal(error.code, "STAGE_SIDECAR_CONFIG_INVALID");
  for (const needle of ["sidecars[0].kind", "package must be a Cargo package name", "features must be a list", "unknown key \"extra\"", "already writes", "must be relative", "64-hex", "version must be x.y.z", "bare cargo"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
  assert.throws(() => validateSidecarManifest([], { target: MAC }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
  assert.throws(() => validateSidecarManifest({ sidecars: [{ kind: "resource", from: "a", to: "b" }], stageDir: "../out" }, { target: MAC }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
});

test("the retired client name is refused in a stage command and by the default runner, never resolved", () => {
  const retired = ["right", "kit"].join(""); // spelled at runtime: the retired-alias rule refuses the literal
  assert.throws(
    () => validateSidecarManifest([{ kind: "ffmpeg", tools: ["ffmpeg"], catalog: "c/artifacts.json", notices: { dir: "n", sourceOfferContact: "a@b.co" }, stage: { cmd: retired, args: ["cargo", "run"] } }], { target: MAC }),
    (error) => error.code === "STAGE_SIDECAR_CONFIG_INVALID" && /retired name of the RustRight client/.test(error.message),
  );
  assert.throws(() => defaultRun(retired, ["cargo", "metadata"], { cwd: "." }), /retired name of the RustRight client/);
});

test("builtBinaryCandidates prefers the explicit-target layout, then the plain one", () => {
  const dirs = builtBinaryCandidates({ targetDir: "/t", triple: MAC, profile: "dev", bin: "eng", windows: false });
  assert.deepEqual(dirs, [path.join("/t", MAC, "debug", "eng"), path.join("/t", "debug", "eng")]);
  assert.equal(builtBinaryCandidates({ targetDir: "/t", triple: WIN, profile: "release", bin: "eng", windows: true })[0], path.join("/t", WIN, "release", "eng.exe"));
});

test("cargo-bin builds through rustright cargo, stages <name>-<triple>, writes a receipt", async () => {
  const { fs, files, log } = fakeFs();
  const exe = path.resolve("/rk-fake-target", MAC, "release", "mailright-engine");
  const { run, calls } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("engine-bytes")); return { stdout: stream("mailright-engine", exe) }; } });
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "mailright-engine", features: ["a", "pkg/b"] }], deps: deps({ fs, run }) });
  const staged = at("src-tauri", "binaries", `mailright-engine-${MAC}`);
  assert.equal(fs.readFileSync(staged).toString(), "engine-bytes");
  assert.deepEqual(result.externalBinNames, ["mailright-engine"]);
  assert.ok(calls.every((call) => call.command === "rustright" && call.args[0] === "cargo"), "never bare cargo");
  const build = calls[0].args;
  for (const flag of ["--locked", "-p", "mailright-engine", "--target", MAC, "--profile", "release", "--features", "a,pkg/b"]) assert.ok(build.includes(flag), flag);
  assert.deepEqual(log.chmods.map(([, mode]) => mode), [0o755]);
  const receipt = JSON.parse(fs.readFileSync(result.receiptPath).toString());
  assert.equal(receipt.files[0].sha256, sha("engine-bytes"));
  assert.equal(receipt.files[0].path, `src-tauri/binaries/mailright-engine-${MAC}`);
  assert.equal(path.relative(APP, result.receiptPath).replaceAll("\\", "/"), "src-tauri/binaries/stage-sidecar.json");
  validateStageSidecarReceipt(receipt, { target: MAC, names: ["mailright-engine"] });
});

test("cargo-bin falls back to <targetDir>/<triple>/<profile>, then <targetDir>/<profile>; windows gets .exe and no chmod", async () => {
  for (const layout of ["triple", "plain"]) {
    const { fs, files, log } = fakeFs();
    const dir = layout === "triple" ? path.resolve("/rk-fake-target", WIN, "release") : path.resolve("/rk-fake-target", "release");
    const { run } = fakeRun({ onBuild: () => { files.set(path.join(dir, "eng.exe"), Buffer.from(`bytes-${layout}`)); return { stdout: "" }; } });
    await stageSidecars({ appRoot: APP, target: WIN, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run }) });
    assert.equal(fs.readFileSync(at("src-tauri", "binaries", `eng-${WIN}.exe`)).toString(), `bytes-${layout}`);
    assert.deepEqual(log.chmods, []);
  }
  const { fs } = fakeFs();
  const { run } = fakeRun({ onBuild: () => ({ stdout: "" }) });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run }) }), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
});

test("a failed build and a pinned-hash mismatch are typed errors that stage nothing", async () => {
  const { fs, files } = fakeFs();
  const failing = (command, args) => (args[1] === "build" ? { status: 101, stdout: "", stderr: "boom" } : { status: 0, stdout: "{}" });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run: failing }) }), { code: "STAGE_SIDECAR_BUILD_FAILED" });
  const exe = path.resolve("/rk-fake-target", MAC, "release", "eng");
  const { run } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("real")); return { stdout: stream("eng", exe) }; } });
  await assert.rejects(
    stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng", sha256: sha("other") }], deps: deps({ fs, run }) }),
    (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.actual === sha("real"),
  );
  assert.equal(fs.existsSync(at("src-tauri", "binaries", `eng-${MAC}`)), false);
});

test("staging is idempotent: an identical staged file is not rewritten", async () => {
  const { fs, files, log } = fakeFs();
  const exe = path.resolve("/rk-fake-target", MAC, "release", "eng");
  const { run } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("same")); return { stdout: stream("eng", exe) }; } });
  const manifest = [{ kind: "cargo-bin", package: "eng" }];
  await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run }) });
  await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run }) });
  assert.equal(log.copies.length, 1);
});

test("platforms filters entries per target and records them as skipped", async () => {
  const { fs, files } = fakeFs({ [at("models", "m.bin")]: "model" });
  const exe = path.resolve("/rk-fake-target", MAC, "release", "eng");
  const { run, calls } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("e")); return { stdout: stream("eng", exe) }; } });
  const result = await stageSidecars({
    appRoot: APP, target: MAC, deps: deps({ fs, run }),
    manifest: [{ kind: "cargo-bin", package: "eng" }, { kind: "cargo-bin", package: "dump-helper", platforms: ["win"] }, { kind: "resource", from: "models/m.bin", to: "src-tauri/resources/m.bin", sha256: sha("model") }],
  });
  assert.equal(calls.filter((call) => call.args[1] === "build").length, 1);
  assert.deepEqual(result.receipt.skipped.map((item) => item.platforms), [["win"]]);
  assert.equal(fs.readFileSync(at("src-tauri", "resources", "m.bin")).toString(), "model");
  assert.deepEqual(result.externalBinNames, ["eng"]);
});

test("resource hash mismatch and a missing source are refused", async () => {
  const { fs } = fakeFs({ [at("a.bin")]: "a" });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to: "out/a.bin", sha256: sha("b") }], deps: deps({ fs }) }), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "none.bin", to: "out/n.bin" }], deps: deps({ fs }) }), { code: "STAGE_SIDECAR_RESOURCE_MISSING" });
});

function ortCatalog(overrides = {}) {
  const library = Buffer.from("ort-library");
  const archive = Buffer.from("ort-archive");
  return {
    library, archive,
    catalog: { libraries: [{
      id: "onnxruntime-1.24.4-macos-arm64", library: "onnxruntime", version: "1.24.4", os: "macos", arch: "aarch64", status: "approved",
      artifactFileName: "libonnxruntime.1.24.4.dylib", sha256: sha(library), archive: { url: "https://example.test/ort.tgz", sha256: sha(archive) },
      licenseSpdx: "MIT", noticeFiles: ["third-party/ONNXRuntime-MIT.txt"], ...overrides,
    }] },
  };
}

test("onnxruntime downloads, verifies archive and library digests, stages the library and its notices", async () => {
  const { library, archive, catalog } = ortCatalog();
  const crateDir = path.resolve("/rk-fake-crate");
  const { fs, files } = fakeFs({ [path.join(crateDir, "third-party", "ONNXRuntime-MIT.txt")]: "MIT text" });
  let fetched = 0;
  const result = await stageSidecars({
    appRoot: APP, target: MAC,
    manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog: "cat/native-libraries.json", as: "libonnxruntime.dylib" }],
    deps: deps({
      fs,
      loadNativeCatalog: () => ({ catalog, crateDir }),
      fetchBytes: (url) => { fetched += 1; assert.equal(url, "https://example.test/ort.tgz"); return archive; },
      makeTempDir: () => path.resolve("/rk-fake-scratch"),
      removeDir: () => {},
      extractMember: ({ archive: file, fileName, destDir }) => {
        assert.equal(fileName, "libonnxruntime.1.24.4.dylib");
        assert.equal(sha(fs.readFileSync(file)), sha(archive));
        const out = path.join(destDir, fileName);
        files.set(out, library);
        return out;
      },
    }),
  });
  assert.equal(fetched, 1);
  assert.equal(fs.readFileSync(at("src-tauri", "resources", "runtime", "libonnxruntime.dylib")).toString(), "ort-library");
  assert.equal(fs.readFileSync(at("src-tauri", "resources", "runtime", "LICENSES", "ONNXRuntime-MIT.txt")).toString(), "MIT text");
  assert.equal(result.files[0].license, "MIT");
  assert.equal(result.files[0].notices[0].sha256, sha("MIT text"));
  assert.deepEqual(result.externalBinNames, []);
});

test("onnxruntime refuses a bad archive digest, a planned row, a missing notice and a missing catalog", async () => {
  const crateDir = path.resolve("/rk-fake-crate");
  const run = (extra, over, files = { [path.join(crateDir, "third-party", "ONNXRuntime-MIT.txt")]: "MIT" }) => {
    const { fs } = fakeFs(files);
    const { catalog } = ortCatalog(over);
    return stageSidecars({
      appRoot: APP, target: MAC, manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog: "c.json" }],
      deps: deps({ fs, loadNativeCatalog: () => ({ catalog, crateDir }), makeTempDir: () => path.resolve("/s"), removeDir: () => {}, ...extra }),
    });
  };
  await assert.rejects(run({ fetchBytes: () => Buffer.from("tampered") }), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  await assert.rejects(run({}, { status: "planned" }), { code: "STAGE_SIDECAR_NOT_APPROVED" });
  await assert.rejects(run({}, {}, {}), { code: "STAGE_SIDECAR_NOTICE_MISSING" });
  await assert.rejects(run({}, { noticeFiles: [] }), { code: "STAGE_SIDECAR_NOTICE_MISSING" });
  const { archive } = ortCatalog();
  await assert.rejects(run({ fetchBytes: () => archive, extractMember: ({ destDir }) => path.join(destDir, "x") }), (error) => error.code === "ENOENT" || error instanceof StageSidecarError);
  const { fs } = fakeFs();
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "onnxruntime", version: "1.24.4" }], deps: deps({ fs }) }), { code: "STAGE_SIDECAR_CATALOG_UNREADABLE" });
});

function ffmpegFixture({ stagedBytes = "ffmpeg-bytes", contentSha256 = sha("ffmpeg-bytes") } = {}) {
  const crateDir = path.resolve("/rk-fake-ffmpeg-crate");
  const row = {
    id: "ffmpeg-decode-macos-arm64", tool: "ffmpeg", variant: "decode", os: "macos", arch: "aarch64", status: "approved", version: "7.1",
    contentSha256, sourceSha256: sha("src"),
    attribution: { noticeFiles: ["notices/LGPL-2.1.txt"], sourceUrl: "https://ffmpeg.org/releases/ffmpeg-7.1.tar.xz", licenseSpdx: "LGPL-2.1-or-later", buildProvenance: "recipe" },
  };
  const { fs, files, log } = fakeFs({ [path.join(crateDir, "notices", "LGPL-2.1.txt")]: "LGPL text" });
  const run = (command, args) => {
    if (command === "rustright" && args[0] === "ffmpeg-stage") files.set(at("src-tauri", "binaries", `ffmpeg-${MAC}`), Buffer.from(stagedBytes));
    return { status: 0, stdout: "", stderr: "" };
  };
  return { fs, log, run, crateDir, catalog: { schemaVersion: 1, artifacts: [row] } };
}

const ffmpegManifest = [{ kind: "ffmpeg", tools: ["ffmpeg"], catalog: "c/artifacts.json", notices: { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.test/source" }, stage: { cmd: "rustright", args: ["ffmpeg-stage"] } }];

test("ffmpeg stages through media-sidecar, writes licence notices and verifies the catalog digest", async () => {
  const fixture = ffmpegFixture();
  const result = await stageSidecars({
    appRoot: APP, target: MAC, manifest: ffmpegManifest,
    deps: deps({ fs: fixture.fs, run: fixture.run, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir, crateVersion: "0.9.0" }) }),
  });
  const [file] = result.files;
  assert.equal(file.name, "ffmpeg");
  assert.equal(file.path, `src-tauri/binaries/ffmpeg-${MAC}`);
  assert.equal(file.license, "LGPL-2.1-or-later");
  assert.deepEqual(file.notices.map((item) => item.name).sort(), ["FFMPEG-SOURCE-OFFER.txt", "LGPL-2.1.txt"]);
  assert.deepEqual(result.externalBinNames, ["ffmpeg"]);
});

test("ffmpeg refuses staged bytes that differ from the approved catalog digest", async () => {
  const fixture = ffmpegFixture({ stagedBytes: "tampered" });
  await assert.rejects(
    stageSidecars({ appRoot: APP, target: MAC, manifest: ffmpegManifest, deps: deps({ fs: fixture.fs, run: fixture.run, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir }) }) }),
    { code: "STAGE_SIDECAR_HASH_MISMATCH" },
  );
});

// Minimal thin arm64 Mach-O, mirroring content-digest.test.mjs: __TEXT + __LINKEDIT (+ optional code signature).
function machoArm64(payload, signature) {
  const u32 = (v) => { const b = Buffer.alloc(4); b.writeUInt32LE(v); return b; };
  const u64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
  const up = (v, t) => Math.floor((v + t - 1) / t) * t;
  const sizeofcmds = 144 + (signature ? 16 : 0);
  const textOff = 0x1000, text = Buffer.alloc(64, 0xaa), linkeditOff = textOff + text.length;
  const sigOff = signature ? up(linkeditOff + payload.length, 16) : null;
  const fileLen = signature ? sigOff + signature.length : linkeditOff + payload.length;
  const filesize = fileLen - linkeditOff;
  const segment = (name, vm, vmsize, off, size) => { const n = Buffer.alloc(16); n.write(name, "latin1"); return Buffer.concat([u32(0x19), u32(72), n, u64(vm), u64(vmsize), u64(off), u64(size), Buffer.alloc(16)]); };
  const head = [u32(0xfeedfacf), u32(0x0100000c), u32(0), u32(2), u32(2 + (signature ? 1 : 0)), u32(sizeofcmds), u32(0), u32(0),
    segment("__TEXT", 0, 0x4000, textOff, text.length), segment("__LINKEDIT", 0x4000, up(filesize, 0x4000), linkeditOff, filesize)];
  if (signature) head.push(u32(0x1d), u32(16), u32(sigOff), u32(signature.length));
  const file = Buffer.alloc(fileLen);
  Buffer.concat(head).copy(file); text.copy(file, textOff); payload.copy(file, linkeditOff);
  if (signature) signature.copy(file, sigOff);
  return file;
}

test("ffmpeg staging accepts a signed Mach-O whose whole-file hash differs from the catalog content digest", async () => {
  const payload = Buffer.from("symtab-and-strtab-bytes!");
  const unsigned = machoArm64(payload, null);
  const signed = machoArm64(payload, Buffer.alloc(300, 0x11));
  const catalogDigest = contentIdentity(unsigned).slices[0].sha256;
  assert.equal(contentIdentity(signed).slices[0].sha256, catalogDigest, "signed & unsigned share one content digest");
  assert.notEqual(sha(signed), catalogDigest, "whole-file SHA-256 differs from the catalog row");
  const fixture = ffmpegFixture({ stagedBytes: signed, contentSha256: catalogDigest });
  const result = await stageSidecars({
    appRoot: APP, target: MAC, manifest: ffmpegManifest,
    deps: deps({ fs: fixture.fs, run: fixture.run, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir, crateVersion: "0.9.0" }) }),
  });
  const [file] = result.files;
  assert.equal(file.catalogContentSha256, catalogDigest);
  assert.equal(file.sha256, sha(signed), "receipt sha256 stays the whole-file pre-sign hash");
});

test("ffmpeg staging refuses a Mach-O with changed code under the same catalog content digest", async () => {
  const payload = Buffer.from("symtab-and-strtab-bytes!");
  const catalogDigest = contentIdentity(machoArm64(payload, null)).slices[0].sha256;
  const tampered = machoArm64(payload, Buffer.alloc(300, 0x11)); tampered[0x1000] ^= 1;
  const fixture = ffmpegFixture({ stagedBytes: tampered, contentSha256: catalogDigest });
  await assert.rejects(
    stageSidecars({ appRoot: APP, target: MAC, manifest: ffmpegManifest, deps: deps({ fs: fixture.fs, run: fixture.run, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir }) }) }),
    { code: "STAGE_SIDECAR_HASH_MISMATCH" },
  );
});

test("ffmpeg wraps media-sidecar failures (a pending catalog row) in a typed error", async () => {
  const fixture = ffmpegFixture();
  fixture.catalog.artifacts[0].status = "pending";
  await assert.rejects(
    stageSidecars({ appRoot: APP, target: MAC, manifest: ffmpegManifest, deps: deps({ fs: fixture.fs, run: fixture.run, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir }) }) }),
    (error) => error.code === "STAGE_SIDECAR_FFMPEG_FAILED" && error.details.causeCode === "MEDIA_SIDECAR_NOT_APPROVED",
  );
});

test("validateStageSidecarReceipt rejects malformed receipts", () => {
  assert.throws(() => validateStageSidecarReceipt({ schema: 1 }), { code: "STAGE_SIDECAR_RECEIPT_INVALID" });
  assert.throws(() => validateStageSidecarReceipt({ schema: 1, contract: "rightkit-stage-sidecar-v1", files: [{ name: "a", path: "a", sha256: "x", sizeBytes: 1 }] }), { code: "STAGE_SIDECAR_RECEIPT_INVALID" });
});

// ---- profile switch, external catalogs, command kind, universal-apple-darwin (bogusyogi/rightkit#345) ----

const profileOf = (manifest, env) => validateSidecarManifest(manifest, { target: MAC, mode: resolveBuildMode(env) }).entries[0].profile;

test("cargo-bin profile pair is chosen by RIGHTKIT_BUILD_MODE, with a top-level profiles default", () => {
  const pair = { dev: "release-iterate", release: "release" };
  const entry = [{ kind: "cargo-bin", package: "eng", profile: pair }];
  assert.equal(profileOf(entry, {}), "release");
  assert.equal(profileOf(entry, { RIGHTKIT_BUILD_MODE: "release" }), "release");
  assert.equal(profileOf(entry, { RIGHTKIT_BUILD_MODE: "dev" }), "release-iterate");
  const wrapped = { sidecars: [{ kind: "cargo-bin", package: "eng" }, { kind: "cargo-bin", package: "own", profile: "release-lto" }], profiles: pair };
  assert.equal(profileOf(wrapped, { RIGHTKIT_BUILD_MODE: "dev" }), "release-iterate");
  assert.equal(validateSidecarManifest(wrapped, { target: MAC, mode: "dev" }).entries[1].profile, "release-lto");
  assert.equal(validateSidecarManifest([{ kind: "cargo-bin", package: "eng" }], { target: MAC, mode: "dev" }).entries[0].profile, "release");
  assert.throws(() => resolveBuildMode({ RIGHTKIT_BUILD_MODE: "debug" }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
  for (const bad of [{ dev: "x" }, { dev: "A", release: "release" }, { dev: "a", release: "b", beta: "c" }, 7]) {
    assert.throws(() => validateSidecarManifest([{ kind: "cargo-bin", package: "eng", profile: bad }], { target: MAC }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
  }
  assert.throws(() => validateSidecarManifest({ sidecars: entry, profiles: "release" }, { target: MAC }), /profiles/);
});

test("the dev mode builds with the dev profile through rustright cargo", async () => {
  const { fs, files } = fakeFs();
  const exe = path.resolve("/rk-fake-target", MAC, "release-iterate", "eng");
  const { run, calls } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("dev-bytes")); return { stdout: stream("eng", exe) }; } });
  const manifest = [{ kind: "cargo-bin", package: "eng", profile: { dev: "release-iterate", release: "release" } }];
  await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run, env: { RIGHTKIT_BUILD_MODE: "dev" } }) });
  assert.equal(calls[0].args[calls[0].args.indexOf("--profile") + 1], "release-iterate");
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run, env: { RIGHTKIT_BUILD_MODE: "nope" } }) }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
});

test("catalog accepts a path, an absolute path, { crate } and { npm }; a bad catalog is reported against its entry", () => {
  const abs = path.resolve("/somewhere/artifacts.json");
  const ok = validateSidecarManifest([
    { kind: "onnxruntime", version: "1.24.4", catalog: "cat/n.json" },
    { kind: "onnxruntime", version: "1.24.4", catalog: abs, as: "a.dylib" },
    { kind: "onnxruntime", version: "1.24.4", catalog: { crate: "rightkit-ffmpeg" }, as: "b.dylib" },
    { kind: "onnxruntime", version: "1.24.4", catalog: { npm: "@orthic/catalogs/native-libraries.json" }, as: "c.dylib" },
  ], { target: MAC }).entries;
  assert.deepEqual(ok.map((entry) => entry.catalogSpec.via), ["path", "absolute", "crate", "npm"]);
  assert.equal(ok[2].catalogSpec.file, "catalog/native-libraries.json");
  assert.equal(ok[3].catalogSpec.pkg, "@orthic/catalogs");
  let error;
  try {
    validateSidecarManifest([
      { kind: "onnxruntime", version: "1.24.4", catalog: { crate: "bad name" } },
      { kind: "ffmpeg", notices: { dir: "n", sourceOfferContact: "a@b.co" }, catalog: { npm: "nofile" } },
      { kind: "onnxruntime", version: "1.24.4", catalog: { crate: "ok", extra: 1 }, as: "z.dylib" },
      { kind: "onnxruntime", version: "1.24.4", catalog: ["x"], as: "y.dylib" },
    ], { target: MAC });
  } catch (caught) { error = caught; }
  for (const needle of ["sidecars[0] (onnxruntime): catalog.crate must be a Cargo package name", "sidecars[1] (ffmpeg): catalog.npm must be", "sidecars[2] (onnxruntime): catalog: unknown key \"extra\"", "sidecars[3] (onnxruntime): catalog must be a path"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
});

const nativeCatalog = (bytes) => ({
  libraries: [{ id: "ort", library: "onnxruntime", version: "1.24.4", os: "macos", arch: "aarch64", status: "approved", sha256: sha(bytes), artifactFileName: "libonnxruntime.dylib", archive: { url: "https://example.test/ort.tgz", sha256: sha("archive") }, noticeFiles: ["NOTICE.txt"] }],
});

test("onnxruntime resolves a { crate } catalog through the metadata listing and an { npm } catalog through node_modules", async () => {
  const crateDir = path.resolve("/rk-fake-crates/rightkit-ffmpeg");
  for (const catalog of [{ crate: "rightkit-ffmpeg" }, { npm: "@orthic/catalogs/native-libraries.json" }]) {
    const { fs, files } = fakeFs({
      [path.join(crateDir, "catalog", "native-libraries.json")]: JSON.stringify(nativeCatalog("lib")),
      [path.join(crateDir, "NOTICE.txt")]: "notice",
      [path.resolve("/node_modules/@orthic/catalogs/native-libraries.json")]: JSON.stringify(nativeCatalog("lib")),
      [path.resolve("/node_modules/@orthic/NOTICE.txt")]: "notice",
    });
    const calls = [];
    const run = (command, args, options) => {
      calls.push({ command, args, options });
      if (args[1] === "metadata") return { status: 0, stderr: "", stdout: JSON.stringify({ packages: [{ name: "other", manifest_path: path.resolve("/x/Cargo.toml") }, { name: "rightkit-ffmpeg", manifest_path: path.join(crateDir, "Cargo.toml") }] }) };
      return { status: 0, stdout: "", stderr: "" };
    };
    const result = await stageSidecars({
      appRoot: path.resolve("/node_modules/.rk-app"), target: MAC, manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog, as: "libonnxruntime.dylib" }],
      deps: deps({
        fs, run, fetchBytes: async () => Buffer.from("archive"), makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {},
        extractMember: ({ destDir }) => { const file = path.join(destDir, "libonnxruntime.dylib"); files.set(file, Buffer.from("lib")); return file; },
      }),
    });
    assert.equal(result.files[0].sha256, sha("lib"));
    assert.equal(calls.some((call) => call.command === "rustright" && call.args[1] === "metadata"), "crate" in catalog);
  }
});

test("an unresolvable catalog fails that entry with a typed error", async () => {
  const { fs } = fakeFs();
  const run = () => ({ status: 0, stderr: "", stdout: JSON.stringify({ packages: [] }) });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog: { crate: "missing-crate" } }], deps: deps({ fs, run }) }), { code: "STAGE_SIDECAR_CATALOG_UNREADABLE", message: /missing-crate/ });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog: { npm: "nope/x.json" } }], deps: deps({ fs, run }) }), { code: "STAGE_SIDECAR_CATALOG_UNREADABLE", message: /nope\/x\.json/ });
});

test("command kind runs argv without a shell, stages <name>-<triple>, and records its sha256", async () => {
  const { fs, files, log } = fakeFs();
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    files.set(at("vendor", "whisper", "build", "bin", "whisper-cli"), Buffer.from("whisper-bytes"));
    return { status: 0, stdout: "", stderr: "" };
  };
  const manifest = [{ kind: "command", name: "whisper-cli", cmd: "cmake", args: ["--build", "build", "--config", "Release"], cwd: "vendor/whisper", output: "vendor/whisper/build/bin/whisper-cli", externalBin: true }];
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run }) });
  assert.deepEqual(calls.map((call) => [call.command, call.args, call.options.cwd]), [["cmake", ["--build", "build", "--config", "Release"], at("vendor", "whisper")]]);
  assert.equal(calls[0].options.shell, undefined);
  assert.equal(fs.readFileSync(at("src-tauri", "binaries", `whisper-cli-${MAC}`)).toString(), "whisper-bytes");
  assert.deepEqual(result.externalBinNames, ["whisper-cli"]);
  assert.equal(result.files[0].sha256, sha("whisper-bytes"));
  assert.equal(result.files[0].kind, "command");
  assert.deepEqual(log.chmods.map(([, mode]) => mode), [0o755]);
  const failing = await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run: () => ({ status: 3, stderr: "boom" }) }) }).catch((error) => error);
  assert.equal(failing.code, "STAGE_SIDECAR_BUILD_FAILED");
  const { fs: empty } = fakeFs();
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs: empty, run: () => ({ status: 0 }) }) }), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
});

test("command kind validates strictly", () => {
  let error;
  try {
    validateSidecarManifest([
      { kind: "command", name: "a.exe", cmd: "cmake --build", args: "x", output: "../out", externalBin: false, shell: true },
      { kind: "command", name: "b", cmd: "make", output: "/abs", externalBin: true },
    ], { target: MAC });
  } catch (caught) { error = caught; }
  for (const needle of ["name must be a bare sidecar name", "cmd must be one executable", "args must be a list of strings", "output must be relative", "externalBin must be true", "unknown key \"shell\""]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
});

const UNIVERSAL = "universal-apple-darwin";

test("universal-apple-darwin builds both slices and lipo-creates <name>-universal-apple-darwin", async () => {
  const { fs, files, log } = fakeFs();
  const exeFor = (triple) => path.resolve("/rk-fake-target", triple, "release", "eng");
  const seen = [];
  const run = (command, args, options) => {
    seen.push({ command, args, options });
    if (command === "rustright" && args[1] === "build") {
      const triple = args[args.indexOf("--target") + 1];
      files.set(exeFor(triple), Buffer.from(`slice-${triple}`));
      return { status: 0, stderr: "", stdout: stream("eng", exeFor(triple)) };
    }
    if (command === "lipo" && args[0] === "-create") {
      files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-bytes"));
      return { status: 0, stdout: "", stderr: "" };
    }
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  const result = await stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run }) });
  const targets = seen.filter((call) => call.command === "rustright").map((call) => call.args[call.args.indexOf("--target") + 1]);
  assert.deepEqual(targets, ["aarch64-apple-darwin", "x86_64-apple-darwin"]);
  const create = seen.find((call) => call.command === "lipo" && call.args[0] === "-create");
  assert.deepEqual(create.args.filter((arg) => arg.startsWith(path.resolve("/rk-fake-target"))), [exeFor("aarch64-apple-darwin"), exeFor("x86_64-apple-darwin")]);
  assert.equal(fs.readFileSync(at("src-tauri", "binaries", `eng-${UNIVERSAL}`)).toString(), "fat-bytes");
  assert.equal(result.files[0].sha256, sha("fat-bytes"));
  assert.deepEqual(result.files[0].slices.map((slice) => slice.triple), ["aarch64-apple-darwin", "x86_64-apple-darwin"]);
  assert.deepEqual(log.chmods.map(([, mode]) => mode), [0o755]);
  assert.equal(result.receipt.target, UNIVERSAL);
  validateStageSidecarReceipt(result.receipt, { target: UNIVERSAL, names: ["eng"] });
});

test("universal: a thin lipo result and a failing lipo are refused", async () => {
  const build = (files, command, args) => {
    if (command !== "rustright") return undefined;
    const exe = path.resolve("/rk-fake-target", args[args.indexOf("--target") + 1], "eng");
    files.set(exe, Buffer.from("s"));
    return { status: 0, stderr: "", stdout: stream("eng", exe) };
  };
  const thin = fakeFs();
  const thinRun = (command, args) => build(thin.files, command, args)
    ?? (args[0] === "-create" ? (thin.files.set(args[args.indexOf("-output") + 1], Buffer.from("x")), { status: 0 }) : { status: 0, stdout: "arm64\n" });
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs: thin.fs, run: thinRun }) }), { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
  const bad = fakeFs();
  const badRun = (command, args) => build(bad.files, command, args) ?? { status: 1, stdout: "", stderr: "no" };
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs: bad.fs, run: badRun }) }), { code: "STAGE_SIDECAR_BUILD_FAILED" });
});

test("universal: command output and machO resources must already carry both archs", async () => {
  for (const [archs, ok] of [["x86_64 arm64", true], ["arm64", false]]) {
    const { fs, files } = fakeFs({ [at("out", "tool")]: "fat", [at("res", "lib.dylib")]: "fat" });
    const run = (command) => (command === "lipo" ? { status: 0, stdout: `${archs}\n` } : { status: 0, stdout: "", stderr: "" });
    const manifest = [
      { kind: "command", name: "tool", cmd: "make", output: "out/tool", externalBin: true },
      { kind: "resource", from: "res/lib.dylib", to: "src-tauri/resources/lib.dylib", machO: true },
    ];
    const attempt = stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest, deps: deps({ fs, run }) });
    if (ok) {
      await attempt;
      assert.ok(files.has(at("src-tauri", "binaries", `tool-${UNIVERSAL}`)));
    } else await assert.rejects(attempt, { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
  }
  // ffmpeg and onnxruntime are lipo-merged from their per-arch catalog rows (#492); see the slice tests below.
  assert.doesNotThrow(() => validateSidecarManifest([{ kind: "onnxruntime", version: "1.24.4" }], { target: UNIVERSAL }));
  assert.doesNotThrow(() => validateSidecarManifest([{ kind: "ffmpeg", notices: { dir: "n", sourceOfferContact: "a@b.co" } }], { target: UNIVERSAL }));
});

// ---- dest, clean, wipeStageDir, remote, directory resources, nested broker, target roots, --check (bogusyogi/rightkit#345) ----

test("dest places a cargo-bin at a contained path, is not an externalBin, sets the exec bit and cleans a stale file first", async () => {
  const dest = "src-tauri/resources/coderight-engine/coderight.exe";
  const { fs, files, log } = fakeFs({ [at(...dest.split("/"))]: "stale" });
  const exe = path.resolve("/rk-fake-target", MAC, "release", "coderight");
  const { run } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("engine-v2")); return { stdout: stream("coderight", exe) }; } });
  const manifest = [{ kind: "cargo-bin", package: "coderight", dest, clean: true }];
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run }) });
  assert.equal(fs.readFileSync(at(...dest.split("/"))).toString(), "engine-v2");
  assert.equal(fs.existsSync(at("src-tauri", "binaries", `coderight-${MAC}`)), false);
  assert.ok(log.removed.includes(at(...dest.split("/"))));
  assert.deepEqual(result.externalBinNames, []);
  assert.equal(result.files[0].externalBin, false);
  assert.equal(result.files[0].path, dest);
  assert.deepEqual(log.chmods.map(([, mode]) => mode), [0o755]);
  // An unchanged file still gets its exec bit re-applied on POSIX.
  await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "coderight", dest }], deps: deps({ fs, run }) });
  assert.equal(log.chmods.length, 2);
  for (const bad of ["../x/y", "/abs/y", "C:/y"]) assert.throws(() => validateSidecarManifest([{ kind: "cargo-bin", package: "e", dest: bad }], { target: MAC }), /dest must be relative/);
  assert.throws(() => validateSidecarManifest([{ kind: "cargo-bin", package: "e", dest: "a/b" }, { kind: "cargo-bin", package: "f", dest: "a/b" }], { target: MAC }), /already writes/);
  assert.throws(() => validateSidecarManifest([{ kind: "cargo-bin", package: "e", clean: "yes" }], { target: MAC }), /clean must be a boolean/);
});

test("command with dest allows externalBin false, refuses true, and stages at dest", async () => {
  const { fs, files } = fakeFs();
  const run = () => { files.set(at("out", "tool"), Buffer.from("t")); return { status: 0, stdout: "", stderr: "" }; };
  const manifest = [{ kind: "command", name: "tool", cmd: "make", output: "out/tool", externalBin: false, dest: "resources/tool" }];
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run }) });
  assert.equal(fs.readFileSync(at("resources", "tool")).toString(), "t");
  assert.deepEqual(result.externalBinNames, []);
  assert.throws(() => validateSidecarManifest([{ ...manifest[0], externalBin: true }], { target: MAC }), /externalBin must be false or omitted with dest/);
});

test("F6: wipeStageDir removes only <name>-<current triple>[.exe] files of declared names this run did not produce", async () => {
  const stale = [`gone-${MAC}`, `gone-${MAC}.exe`];
  const keepers = ["README.md", "unrelated.bin", `eng-${WIN}.exe`, `gone-${WIN}.exe`, "eng-old.tmp-9", `undeclared-${MAC}`, "notices/LICENSE.txt", `eng-${MAC}.bak`];
  const initial = Object.fromEntries([...stale, ...keepers].map((name) => [at("src-tauri", "binaries", ...name.split("/")), "x"]));
  const { fs, files } = fakeFs(initial);
  const exe = path.resolve("/rk-fake-target", MAC, "release", "eng");
  const { run } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("eng")); return { stdout: stream("eng", exe) }; } });
  // `gone` is declared but skipped on this platform, so its staged name for the current triple is stale.
  const manifest = { sidecars: [{ kind: "cargo-bin", package: "eng" }, { kind: "cargo-bin", package: "gone", platforms: ["win"] }], wipeStageDir: true };
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run }) });
  for (const name of stale) assert.equal(fs.existsSync(at("src-tauri", "binaries", name)), false, name);
  assert.equal(fs.readFileSync(at("src-tauri", "binaries", `eng-${MAC}`)).toString(), "eng", "the file produced this run survives");
  assert.equal(fs.existsSync(result.receiptPath), true, "the receipt survives");
  for (const name of keepers) assert.equal(fs.existsSync(at("src-tauri", "binaries", ...name.split("/"))), true, name);
  assert.deepEqual([...result.receipt.wiped].sort(), stale.map((name) => `src-tauri/binaries/${name}`).sort());
  assert.throws(() => validateSidecarManifest({ sidecars: [{ kind: "cargo-bin", package: "eng" }], wipeStageDir: "yes" }, { target: MAC }), /wipeStageDir must be a boolean/);
});

test("F3: a dest or to naming the app root, or a path that is or contains a protected path, is refused at validation", () => {
  const refuse = (item) => assert.throws(() => validateSidecarManifest([{ kind: "cargo-bin", package: "e", ...item }], { target: MAC }), /at least one normal segment|is or contains/, JSON.stringify(item));
  for (const dest of [".", "./", ".//", "./.", "src-tauri", "src-tauri/", "src-tauri/binaries", "src-tauri/binaries/stage-sidecar.json", ".right-release", ".right-release/cache", ".right-release/cache/remote", ".git", "SRC", "node_modules", "package.json"]) refuse({ dest });
  assert.throws(() => validateSidecarManifest([{ kind: "resource", from: "a", to: "." }], { target: MAC }), /at least one normal segment/);
  assert.throws(() => validateSidecarManifest([{ kind: "remote", name: "r", url: "https://x.example.com/a", sha256: sha("a"), to: "./" }], { target: MAC }), /at least one normal segment/);
  assert.throws(() => validateSidecarManifest({ sidecars: [{ kind: "resource", from: "a", to: "b" }], stageDir: "." }, { target: MAC }), /at least one normal segment/);
  // The cache directory an onnxruntime entry declares is protected too.
  assert.throws(() => validateSidecarManifest([{ kind: "onnxruntime", version: "1.2.3", catalog: "c.json", cacheDir: "cache/ort" }, { kind: "resource", from: "a", to: "cache" }], { target: MAC }), /is or contains/);
  // A command's cwd may still be the app root, and a normal nested destination is accepted.
  validateSidecarManifest([{ kind: "command", name: "t", cmd: "make", cwd: ".", output: "o", externalBin: true }, { kind: "cargo-bin", package: "e", dest: "resources/e/e.exe" }], { target: MAC });
});

test("F3: two entries whose outputs are nested inside each other are refused", () => {
  const manifest = (a, b) => [{ kind: "resource", from: "a", to: a }, { kind: "cargo-bin", package: "e", dest: b }];
  for (const [a, b] of [["res", "res/e"], ["res/e", "res"], ["Res/Models", "res/models/x.bin"]]) assert.throws(() => validateSidecarManifest(manifest(a, b), { target: MAC }), /overlaps/, `${a} ${b}`);
  assert.throws(() => validateSidecarManifest(manifest("res/a", "./res/a"), { target: MAC }), /already writes/);
  validateSidecarManifest(manifest("res/a", "res/b"), { target: MAC });
});

test("F3: clean never reaches the app root or a protected path, removes a file non-recursively, and refuses a symlinked parent", async () => {
  const run = () => ({ status: 0, stdout: "", stderr: "" });
  const { fs, files } = fakeFs({ [at("package.json")]: "{}", [at("a.bin")]: "a" });
  const removals = [];
  const rmSync = fs.rmSync;
  fs.rmSync = (target, options) => { removals.push({ target: path.resolve(target), options }); return rmSync(target, options); };
  for (const to of [".", "./", "package.json"]) {
    await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to, clean: true }], deps: deps({ fs, run }) }), { code: "STAGE_SIDECAR_CONFIG_INVALID" }, to);
  }
  assert.deepEqual(removals, []);
  assert.ok(files.has(at("package.json")));
  // A file destination is removed with recursive: false.
  const fileRun = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to: "out/a.bin", clean: true }], deps: deps({ fs, run }) });
  assert.equal(fileRun.files.length, 1);
  assert.deepEqual(removals.map((call) => call.options.recursive), [false]);
  // A destination that is a directory is not removed by a file entry (non-recursive rm; the fake refuses it like Node).
  const { fs: dirFs } = fakeFs({ [at("a.bin")]: "a", [at("out", "a.bin", "nested.txt")]: "n" });
  dirFs.rmSync = (target, options) => { if (options?.recursive !== true) throw Object.assign(new Error("EISDIR"), { code: "ERR_FS_EISDIR" }); };
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to: "out/a.bin", clean: true }], deps: deps({ fs: dirFs, run }) }), { code: "STAGE_SIDECAR_CLEAN_REFUSED" });
  // A parent that resolves outside the app through a symlink is refused before any delete.
  const outside = path.resolve("/rk-outside-the-app");
  const deleted = [];
  const { fs: linkFs } = fakeFs({ [at("a.bin")]: "a" });
  linkFs.rmSync = (target) => { deleted.push(target); };
  linkFs.realpathSync = (target) => { const resolved = path.resolve(target); return resolved === at("out") ? path.join(outside, "out") : resolved; };
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to: "out/a.bin", clean: true }], deps: deps({ fs: linkFs, run }) }), { code: "STAGE_SIDECAR_CLEAN_REFUSED", message: /outside the app/ });
  assert.deepEqual(deleted, []);
  // A symlinked parent that lands on a protected path inside the app is refused too.
  const { fs: aliasFs } = fakeFs({ [at("a.bin")]: "a" });
  aliasFs.rmSync = (target) => { deleted.push(target); };
  aliasFs.realpathSync = (target) => { const resolved = path.resolve(target); return resolved === at("out") ? at("src-tauri") : resolved; };
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to: "out/binaries", clean: true }], deps: deps({ fs: aliasFs, run }) }), { code: "STAGE_SIDECAR_CLEAN_REFUSED", message: /protected path/ });
  assert.deepEqual(deleted, []);
});

const REMOTE = { kind: "remote", name: "r2-model", url: "https://models.example.com/r2.onnx", sha256: sha("model-bytes"), dest: "src-tauri/resources/models/r2.onnx" };
const fetchOf = (body, extra = {}) => {
  const calls = [];
  const fetch = async (url) => { calls.push(url); return { ok: true, status: 200, arrayBuffer: async () => Buffer.from(body), ...extra }; };
  return { fetch, calls };
};

test("remote downloads over https, verifies sha256 before placing, caches by content address and reuses the cache", async () => {
  const { fs, files } = fakeFs();
  const { fetch, calls } = fetchOf("model-bytes");
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...REMOTE, size: 11 }], deps: deps({ fs, fetch }) });
  assert.equal(fs.readFileSync(at("src-tauri", "resources", "models", "r2.onnx")).toString(), "model-bytes");
  assert.ok(files.has(at(".right-release", "cache", "remote", sha("model-bytes"))), "content-addressed cache entry");
  assert.equal(result.files[0].kind, "remote");
  assert.equal(result.files[0].cached, false);
  assert.equal(result.files[0].sha256, sha("model-bytes"));
  assert.deepEqual(calls, [REMOTE.url]);
  // Destination removed, then staged again with `to` instead of `dest`: served from the cache, no second fetch.
  files.delete(at("src-tauri", "resources", "models", "r2.onnx"));
  const { dest, ...rest } = REMOTE;
  const reused = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...rest, to: dest }], deps: deps({ fs, fetch }) });
  assert.equal(calls.length, 1);
  assert.equal(reused.files[0].cached, true);
});

test("remote fails closed: bad digest, wrong size, http failure and a non-https redirect stage nothing; bad config is refused", async () => {
  const attempt = async (entry, body, extra) => {
    const { fs, files } = fakeFs();
    const { fetch } = fetchOf(body, extra);
    const error = await stageSidecars({ appRoot: APP, target: MAC, manifest: [entry], deps: deps({ fs, fetch }) }).catch((caught) => caught);
    assert.equal(fs.existsSync(at("src-tauri", "resources", "models", "r2.onnx")), false, "nothing placed");
    assert.equal(files.has(at(".right-release", "cache", "remote", REMOTE.sha256)), false, "nothing cached");
    return error;
  };
  const mismatch = await attempt(REMOTE, "tampered");
  assert.equal(mismatch.code, "STAGE_SIDECAR_HASH_MISMATCH");
  assert.equal(mismatch.details.actual, sha("tampered"));
  assert.equal((await attempt({ ...REMOTE, size: 3 }, "model-bytes")).code, "STAGE_SIDECAR_HASH_MISMATCH");
  assert.equal((await attempt(REMOTE, "x", { ok: false, status: 404 })).code, "STAGE_SIDECAR_DOWNLOAD_FAILED");
  assert.equal((await attempt(REMOTE, "model-bytes", { url: "http://evil.example.com/r2.onnx" })).code, "STAGE_SIDECAR_DOWNLOAD_FAILED");
  for (const bad of [{ url: "http://x.example.com/a" }, { url: "file:///etc/passwd" }, { sha256: undefined }, { sha256: "abc" }, { dest: undefined }, { to: "also/dest" }, { size: -1 }]) {
    assert.throws(() => validateSidecarManifest([{ ...REMOTE, ...bad }], { target: MAC }), { code: "STAGE_SIDECAR_CONFIG_INVALID" }, JSON.stringify(bad));
  }
});

test("a corrupt cache entry is ignored and re-downloaded", async () => {
  const { fs, files } = fakeFs({ [at(".right-release", "cache", "remote", sha("model-bytes"))]: "corrupt" });
  const { fetch, calls } = fetchOf("model-bytes");
  await stageSidecars({ appRoot: APP, target: MAC, manifest: [REMOTE], deps: deps({ fs, fetch }) });
  assert.equal(calls.length, 1);
  assert.equal(files.get(at(".right-release", "cache", "remote", sha("model-bytes"))).toString(), "model-bytes");
});

test("a resource directory is copied recursively and every file is hashed into the receipt", async () => {
  const { fs } = fakeFs({ [at("native", "win", "onnxruntime.dll")]: "ort", [at("native", "win", "sub", "DirectML.dll")]: "dml" });
  const result = await stageSidecars({ appRoot: APP, target: WIN, manifest: [{ kind: "resource", from: "native/win", to: "src-tauri/resources/runtime", clean: true }], deps: deps({ fs }) });
  assert.equal(fs.readFileSync(at("src-tauri", "resources", "runtime", "onnxruntime.dll")).toString(), "ort");
  assert.equal(fs.readFileSync(at("src-tauri", "resources", "runtime", "sub", "DirectML.dll")).toString(), "dml");
  assert.deepEqual(result.files.map((file) => [file.path, file.sha256]).sort(), [
    ["src-tauri/resources/runtime/onnxruntime.dll", sha("ort")], ["src-tauri/resources/runtime/sub/DirectML.dll", sha("dml")],
  ]);
  validateStageSidecarReceipt(result.receipt, { target: WIN });
  await assert.rejects(stageSidecars({ appRoot: APP, target: WIN, manifest: [{ kind: "resource", from: "native/win", to: "out", sha256: sha("x") }], deps: deps({ fs }) }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
  await assert.rejects(stageSidecars({ appRoot: APP, target: WIN, manifest: [{ kind: "resource", from: "nope", to: "out" }], deps: deps({ fs }) }), { code: "STAGE_SIDECAR_RESOURCE_MISSING" });
});

test("nested broker context fails fast with the prepare-step fix; the escape hatch and non-broker entries are unaffected", async () => {
  const nested = { TAURI_ENV_PLATFORM: "darwin", TAURI_ENV_TARGET_TRIPLE: MAC, RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/broker.sock" };
  assert.equal(isNestedBrokerContext(nested), true);
  assert.equal(isNestedBrokerContext({ TAURI_ENV_PLATFORM: "darwin", RIGHTSUITE_HEAVY_WORK_OWNER: "1" }), true);
  assert.equal(isNestedBrokerContext({ RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/broker.sock" }), false, "not inside a Tauri hook");
  assert.equal(isNestedBrokerContext({ TAURI_ENV_PLATFORM: "darwin" }), false, "not a managed host");
  assert.equal(isNestedBrokerContext({ ...nested, RIGHTKIT_STAGE_SIDECAR_ALLOW_NESTED: "1" }), false);
  const { fs, files } = fakeFs({ [at("a.bin")]: "a" });
  const calls = [];
  const run = (...args) => { calls.push(args); return { status: 0, stdout: "" }; };
  const error = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run, env: nested }) }).catch((caught) => caught);
  assert.equal(error.code, "STAGE_SIDECAR_NESTED_BROKER");
  assert.match(error.message, /recipe `prepare` step, before the Tauri build/);
  assert.equal(calls.length, 0, "no broker call was attempted");
  await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "resource", from: "a.bin", to: "out/a.bin" }], deps: deps({ fs, env: nested }) });
  assert.ok(files.has(at("out", "a.bin")));
});

test("managed target roots: CARGO_TARGET_DIR and the metadata target_directory are both searched, triple layout then plain", async () => {
  const envDir = path.resolve("/rk-managed-root");
  for (const layout of ["triple", "plain"]) {
    const { fs, files } = fakeFs();
    const dir = layout === "triple" ? path.join(envDir, MAC, "release") : path.join(envDir, "release");
    const { run, calls } = fakeRun({ onBuild: () => { files.set(path.join(dir, "eng"), Buffer.from(layout)); return { stdout: "" }; } });
    await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run, env: { CARGO_TARGET_DIR: envDir } }) });
    assert.equal(fs.readFileSync(at("src-tauri", "binaries", `eng-${MAC}`)).toString(), layout);
    assert.equal(calls.filter((call) => call.args[1] === "metadata").length, 1, "asked the broker for target_directory");
  }
  assert.deepEqual(builtBinaryCandidates({ targetDir: "/m", targetDirs: ["/e"], triple: MAC, profile: "release", bin: "eng", windows: false }),
    [path.join("/e", MAC, "release", "eng"), path.join("/e", "release", "eng"), path.join("/m", MAC, "release", "eng"), path.join("/m", "release", "eng")]);
  // A failed metadata query is not fatal when CARGO_TARGET_DIR names the root; with neither it still is.
  const { fs, files } = fakeFs();
  const run = (command, args) => (args[1] === "build" ? (files.set(path.join(envDir, "release", "eng"), Buffer.from("e")), { status: 0, stdout: "" }) : { status: 1, stdout: "" });
  await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs, run, env: { CARGO_TARGET_DIR: envDir } }) });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "eng" }], deps: deps({ fs: fakeFs().fs, run, env: {} }) }), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
});

test("--check: planSidecarStaging validates and describes without any effect; the CLI parses --check", () => {
  assert.equal(parseStageSidecarArgs(["--check", "--target", MAC]).check, true);
  assert.equal(parseStageSidecarArgs([]).check, false);
  assert.equal(nodeHostTriple("win32", "x64"), WIN);
  assert.equal(nodeHostTriple("darwin", "arm64"), MAC);
  assert.throws(() => nodeHostTriple("freebsd", "x64"), /pass --target/);
  const manifest = { wipeStageDir: true, sidecars: [
    { kind: "cargo-bin", package: "coderight", dest: "resources/coderight-engine/coderight.exe", clean: true, platforms: ["win"] },
    { kind: "cargo-bin", package: "mailright-engine", profile: { dev: "dev", release: "release" } },
    { kind: "remote", name: "m", url: "https://x.example.com/m.onnx", sha256: sha("m"), dest: "resources/m.onnx" },
    { kind: "resource", from: "native", to: "resources/runtime" },
  ] };
  const plan = planSidecarStaging({ manifest, target: WIN, env: {} });
  assert.equal(plan.wipeStageDir, true);
  assert.deepEqual(plan.entries[0].outputs, ["resources/coderight-engine/coderight.exe"]);
  assert.equal(plan.entries[0].externalBin, false);
  assert.deepEqual(plan.entries[1].outputs, [`src-tauri/binaries/mailright-engine-${WIN}.exe`]);
  assert.equal(plan.entries[1].externalBin, true);
  assert.equal(plan.entries[2].url, "https://x.example.com/m.onnx");
  const mac = planSidecarStaging({ manifest, target: MAC, env: { RIGHTKIT_BUILD_MODE: "dev" } });
  assert.equal(mac.entries[0].skipped, true);
  assert.equal(mac.entries[1].profile, "dev");
  assert.ok(formatSidecarPlan(mac).some((line) => line.includes("skipped")));
  assert.throws(() => planSidecarStaging({ manifest: [{ kind: "remote", name: "x", url: "http://x/", sha256: "z" }], target: MAC, env: {} }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
});

test("F6: a failed remote fetch or hash mismatch leaves a clean:true destination in place; success replaces it", async () => {
  const dest = at("src-tauri", "resources", "models", "r2.onnx");
  for (const [body, extra] of [["tampered", {}], ["x", { ok: false, status: 503 }]]) {
    const { fs, files, log } = fakeFs({ [dest]: "previous-good" });
    const { fetch } = fetchOf(body, extra);
    await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...REMOTE, clean: true }], deps: deps({ fs, fetch }) }), { code: /STAGE_SIDECAR_(HASH_MISMATCH|DOWNLOAD_FAILED)/ });
    assert.equal(files.get(dest)?.toString(), "previous-good", "the destination was not removed");
    assert.deepEqual(log.removed, []);
  }
  const { fs, files } = fakeFs({ [dest]: "previous-good" });
  const { fetch } = fetchOf("model-bytes");
  await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...REMOTE, clean: true }], deps: deps({ fs, fetch }) });
  assert.equal(files.get(dest).toString(), "model-bytes");
});

// ---- #489 pinned-commit source build, #490 prebuilt verification, #491 ffmpeg sources/variants/probes/archive unpack, #492 universal lipo merge ----

const hex = (char) => char.repeat(64);
const COMMIT = "60c0be6ac8fa71b1a2ae2dd938a31a34a508e774";
const SOURCE_CMAKE = { kind: "cmake", name: "whisper-cli", repo: "https://github.com/ggml-org/whisper.cpp", commit: COMMIT, target: "whisper-cli", cmakeArgs: ["-DBUILD_SHARED_LIBS=OFF"], platformArgs: { mac: ["-DGGML_METAL=ON"], win: ["-DGGML_CUDA=OFF"] }, smoke: ["--help"] };
const SRC_DIR = at(".right-release", "cmake", "whisper-cli", "src");
const BUILD_DIR = at(".right-release", "cmake", "whisper-cli", "build");

/** A fake git + build-tool runner: `git rev-parse HEAD` is empty until the pinned commit is fetched and checked out. */
function fakeSourceBuild({ files, target = MAC, outputName = "whisper-cli", head: initialHead = "", checkoutHead = COMMIT, dirty = "", produce = true } = {}) {
  const calls = [];
  let head = initialHead;
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (command === "git") {
      if (args[0] === "init") files.set(path.join(SRC_DIR, ".git", "HEAD"), Buffer.from("ref"));
      if (args[0] === "checkout") head = checkoutHead;
      if (args[0] === "rev-parse") return { status: 0, stdout: `${head}\n`, stderr: "" };
      if (args[0] === "status") return { status: 0, stdout: dirty, stderr: "" };
      return { status: 0, stdout: "", stderr: "" };
    }
    if (/cmake/i.test(command) && args[0] === "--build" && produce) files.set(path.join(BUILD_DIR, "bin", `${outputName}${target === WIN ? ".exe" : ""}`), Buffer.from("whisper-bytes"));
    if (command === "lipo" && args[0] === "-create") { files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-whisper")); return { status: 0, stdout: "", stderr: "" }; }
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  return { run, calls };
}

test("cmake kind fetches the pinned commit, configures with per-platform flags, builds, stages <name>-<triple> and smoke-runs it", async () => {
  const { fs, files, log } = fakeFs();
  const { run, calls } = fakeSourceBuild({ files });
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: [SOURCE_CMAKE], deps: deps({ fs, run, host: { os: "macos", arch: "aarch64" } }) });
  const git = calls.filter((call) => call.command === "git").map((call) => call.args.join(" "));
  assert.deepEqual(git.slice(0, 5), ["init -q", "remote add origin https://github.com/ggml-org/whisper.cpp", "status --porcelain --ignored", "rev-parse HEAD", `fetch --depth 1 origin ${COMMIT}`], "clean is asserted before any checkout");
  assert.ok(git.includes("checkout -q --force --detach FETCH_HEAD"));
  const configure = calls.find((call) => call.command === "cmake" && call.args[0] === "-S");
  assert.deepEqual(configure.args.slice(0, 5), ["-S", SRC_DIR, "-B", BUILD_DIR, "-DCMAKE_BUILD_TYPE=Release"]);
  for (const flag of ["-DCMAKE_OSX_ARCHITECTURES=arm64", "-DGGML_METAL=ON", "-DBUILD_SHARED_LIBS=OFF"]) assert.ok(configure.args.includes(flag), flag);
  assert.ok(!configure.args.includes("-DGGML_CUDA=OFF"), "another platform's flags are not passed");
  assert.ok(calls.every((call) => call.options.shell === undefined), "no shell");
  assert.deepEqual(calls.find((call) => call.args[0] === "--build").args, ["--build", BUILD_DIR, "--config", "Release", "--target", "whisper-cli", "--parallel"]);
  assert.equal(fs.readFileSync(at("src-tauri", "binaries", `whisper-cli-${MAC}`)).toString(), "whisper-bytes");
  assert.deepEqual(result.externalBinNames, ["whisper-cli"]);
  const [row] = result.files;
  assert.equal(row.kind, "cmake");
  assert.equal(row.commit, COMMIT);
  assert.equal(row.sha256, sha("whisper-bytes"));
  assert.ok(row.flags.includes("-DGGML_METAL=ON"));
  assert.deepEqual(log.chmods.map(([, mode]) => mode), [0o755]);
  assert.deepEqual(calls.at(-1).args, ["--help"], "the staged binary was smoke-run");
  validateStageSidecarReceipt(result.receipt, { target: MAC, names: ["whisper-cli"] });
});

test("cmake kind reuses a checkout already at the commit, builds the .exe on Windows and the universal build needs both archs", async () => {
  const win = fakeFs();
  const winBuild = fakeSourceBuild({ files: win.files, target: WIN, head: COMMIT });
  await stageSidecars({ appRoot: APP, target: WIN, manifest: [{ ...SOURCE_CMAKE, generator: "Ninja", smoke: undefined }], deps: deps({ fs: { ...win.fs, existsSync: (file) => win.fs.existsSync(file) || file === path.join(SRC_DIR, ".git") }, run: winBuild.run, host: { os: "windows", arch: "x86_64" } }) });
  const winGit = winBuild.calls.filter((call) => call.command === "git").map((call) => call.args[0]);
  assert.deepEqual(winGit, ["remote", "status", "rev-parse", "status"], "clean before and after; no fetch when HEAD already is the pinned commit");
  const configure = winBuild.calls.find((call) => call.args[0] === "-S").args;
  assert.deepEqual(configure.slice(configure.indexOf("-G"), configure.indexOf("-G") + 2), ["-G", "Ninja"]);
  assert.ok(configure.includes("-DGGML_CUDA=OFF") && !configure.some((arg) => arg.startsWith("-DCMAKE_OSX")));
  assert.equal(win.fs.readFileSync(at("src-tauri", "binaries", `whisper-cli-${WIN}.exe`)).toString(), "whisper-bytes");

  const uni = fakeFs();
  const uniBuild = fakeSourceBuild({ files: uni.files });
  await stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [SOURCE_CMAKE], deps: deps({ fs: uni.fs, run: uniBuild.run, host: { os: "macos", arch: "aarch64" } }) });
  assert.ok(uniBuild.calls.find((call) => call.args[0] === "-S").args.includes("-DCMAKE_OSX_ARCHITECTURES=arm64;x86_64"));
  const thin = fakeFs();
  const thinBuild = fakeSourceBuild({ files: thin.files });
  const thinRun = (command, args, options) => (command === "lipo" ? { status: 0, stdout: "arm64\n" } : thinBuild.run(command, args, options));
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [SOURCE_CMAKE], deps: deps({ fs: thin.fs, run: thinRun }) }), { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
});

test("cmake kind refuses a checkout that is not the pinned commit, local edits, a failed step, a missing output and a pin mismatch", async () => {
  const attempt = async (options, manifest = [SOURCE_CMAKE], mutate = (run) => run) => {
    const { fs, files } = fakeFs();
    const { run } = fakeSourceBuild({ files, ...options });
    return stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, run: mutate(run), host: { os: "windows", arch: "x86_64" } }) });
  };
  await assert.rejects(attempt({ checkoutHead: "f".repeat(40) }), { code: "STAGE_SIDECAR_SOURCE_MISMATCH" });
  await assert.rejects(attempt({ head: COMMIT, dirty: " M CMakeLists.txt\n" }), { code: "STAGE_SIDECAR_SOURCE_MISMATCH" });
  await assert.rejects(attempt({ produce: false }), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
  await assert.rejects(attempt({}, [{ ...SOURCE_CMAKE, sha256: hex("a") }]), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  await assert.rejects(attempt({}, [SOURCE_CMAKE], (run) => (command, args, options) => (command === "cmake" && args[0] === "-S" ? { status: 1, stderr: "no compiler" } : run(command, args, options))), { code: "STAGE_SIDECAR_BUILD_FAILED", message: /configure/ });
  await assert.rejects(attempt({}, [SOURCE_CMAKE], (run) => (command, args, options) => (command === "git" && args[0] === "fetch" ? { status: 128, stderr: "no such commit" } : run(command, args, options))), { code: "STAGE_SIDECAR_BUILD_FAILED", message: /git fetch/ });
});

test("cmake kind validates strictly: a branch is not a pin, only -D definitions pass, https only", () => {
  let error;
  try {
    validateSidecarManifest([
      { kind: "cmake", name: "a.exe", repo: "git@github.com:x/y.git", commit: "main", cmakeArgs: ["-DOK=1", "--install-prefix=/x", "-D x=1"], platformArgs: { beos: [] }, generator: "bad;gen", smoke: "x", output: "../o", unknown: 1 },
      { kind: "cmake", name: "b", repo: "https://example.com/r.git", commit: COMMIT, platformArgs: { mac: "-DX=1" } },
    ], { target: MAC });
  } catch (caught) { error = caught; }
  for (const needle of ["name must be a bare sidecar name", "repo must be an https:// URL", "commit must be a full lowercase 40-hex", "cmakeArgs must be a list of -DNAME=value", "platformArgs.beos", "generator must be a generator name", "smoke must be a list", "output must be relative", "unknown key \"unknown\"", "platformArgs.mac must be a list"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
  const plan = planSidecarStaging({ manifest: [SOURCE_CMAKE], target: MAC, env: {} });
  assert.deepEqual(plan.entries[0].outputs, [`src-tauri/binaries/whisper-cli-${MAC}`]);
  assert.equal(plan.entries[0].commit, COMMIT);
  assert.equal(plan.entries[0].externalBin, true);
});

// ---- prebuilt (#490)

const PREBUILT = { kind: "prebuilt", name: "sd-server", manifest: { file: "assets/local-image/runtime-manifest.json", entry: "binaries.{platform}", approvalSha256: "approval_sha256", approvalAlgorithm: "approval_hash_algorithm" } };
function prebuiltFixture({ binary = "sd-bytes", approval = hex("c"), algorithm = "macho-codesign-strip-linkedit-v1", target = MAC, pinned = sha("sd-bytes"), key = "macos-aarch64", stage = true } = {}) {
  const name = target === WIN ? `sd-server-${WIN}.exe` : `sd-server-${target}`;
  const manifest = { binaries: { [key]: { sha256: pinned, approval_sha256: approval, approval_hash_algorithm: algorithm } } };
  return fakeFs({ [at("assets", "local-image", "runtime-manifest.json")]: JSON.stringify(manifest), ...(stage ? { [at("src-tauri", "binaries", name)]: binary } : {}) });
}

test("prebuilt verifies the staged binary against the manifest sha256 and the signing-independent approval hash", async () => {
  const { fs, log } = prebuiltFixture();
  const seen = [];
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: [PREBUILT], deps: deps({ fs, approvalDigest: (file, algorithm) => { seen.push([path.relative(APP, file), algorithm]); return hex("c"); } }) });
  assert.deepEqual(seen, [[path.join("src-tauri", "binaries", `sd-server-${MAC}`), "macho-codesign-strip-linkedit-v1"]]);
  const [row] = result.files;
  assert.equal(row.kind, "prebuilt");
  assert.equal(row.platform, "macos-aarch64");
  assert.equal(row.approvalSha256, hex("c"));
  assert.equal(row.changed, false);
  assert.deepEqual(result.externalBinNames, ["sd-server"]);
  assert.deepEqual(log.chmods.map(([, mode]) => mode), [0o755]);
  validateStageSidecarReceipt(result.receipt, { target: MAC, names: ["sd-server"] });
});

test("prebuilt fails closed on a missing binary, a sha256 or approval mismatch, a missing entry and a failing approval hash", async () => {
  const stage = (fixture, manifest = [PREBUILT], extra = {}, target = MAC) => stageSidecars({ appRoot: APP, target, manifest, deps: deps({ fs: fixture.fs, approvalDigest: () => hex("c"), ...extra }) });
  await assert.rejects(stage(prebuiltFixture({ stage: false })), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
  await assert.rejects(stage(prebuiltFixture({ pinned: sha("other") })), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.actual === sha("sd-bytes"));
  await assert.rejects(stage(prebuiltFixture(), [PREBUILT], { approvalDigest: () => hex("d") }), { code: "STAGE_SIDECAR_HASH_MISMATCH", message: /signing-independent/ });
  await assert.rejects(stage(prebuiltFixture({ key: "macos-x86_64" })), { code: "STAGE_SIDECAR_NOT_APPROVED" });
  await assert.rejects(stage(prebuiltFixture(), [PREBUILT], { approvalDigest: () => { throw Object.assign(new Error("codesign failed"), { code: "APPROVAL_HASH_CODESIGN_FAILED" }); } }), (error) => error.code === "STAGE_SIDECAR_APPROVAL_FAILED" && error.details.causeCode === "APPROVAL_HASH_CODESIGN_FAILED");
  await assert.rejects(stage(prebuiltFixture({ approval: "nope" })), { code: "STAGE_SIDECAR_NOT_APPROVED" });
});

test("prebuilt copies `from`, defaults to raw-sha256 off macOS, and validates the manifest keys", async () => {
  const fixture = prebuiltFixture({ target: WIN, key: "windows-x86_64", algorithm: "raw-sha256", stage: false });
  fixture.files.set(at("build", "sd-server.exe"), Buffer.from("sd-bytes"));
  const seen = [];
  const manifest = [{ ...PREBUILT, from: "build/sd-server.exe", manifest: { ...PREBUILT.manifest, approvalAlgorithm: undefined } }];
  const result = await stageSidecars({ appRoot: APP, target: WIN, manifest, deps: deps({ fs: fixture.fs, approvalDigest: (file, algorithm) => { seen.push(algorithm); return hex("c"); } }) });
  assert.deepEqual(seen, ["raw-sha256"]);
  assert.equal(fixture.fs.readFileSync(at("src-tauri", "binaries", `sd-server-${WIN}.exe`)).toString(), "sd-bytes");
  assert.equal(result.files[0].changed, true);
  assert.doesNotThrow(() => validateSidecarManifest([PREBUILT], { target: UNIVERSAL }), "a universal prebuilt target is merged from per-arch inputs (#692)");
  assert.throws(() => validateSidecarManifest([{ kind: "prebuilt", name: "x", manifest: { file: "../m.json", entry: "a..b", approvalAlgorithm: "alg", extra: 1 } }], { target: MAC }), (error) => ["must be relative", "dot path", "needs approvalSha256", "unknown key \"extra\""].every((needle) => error.message.includes(needle)));
});

// ---- remote archives (#491)

test("remote extract unpacks one archive member after the archive digest passes, verifies memberSha256 and stages only the member", async () => {
  const archive = "zip-bytes";
  const entry = { kind: "remote", name: "ffmpeg-win", url: "https://example.test/ffmpeg.zip", sha256: sha(archive), extract: "ffmpeg.exe", memberSha256: sha("exe-bytes"), executable: true, dest: "resources/ffmpeg/ffmpeg.exe" };
  const make = (memberBytes) => {
    const { fs, files, log } = fakeFs();
    const { fetch } = fetchOf(archive);
    const extractMember = ({ archive: file, fileName, destDir }) => { assert.equal(sha(fs.readFileSync(file)), sha(archive)); assert.equal(fileName, "ffmpeg.exe"); const out = path.join(destDir, fileName); files.set(out, Buffer.from(memberBytes)); return out; };
    return { fs, files, log, deps: deps({ fs, fetch, extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {} }) };
  };
  const ok = make("exe-bytes");
  const result = await stageSidecars({ appRoot: APP, target: WIN, manifest: [entry], deps: ok.deps });
  assert.equal(ok.fs.readFileSync(at("resources", "ffmpeg", "ffmpeg.exe")).toString(), "exe-bytes");
  assert.equal(result.files[0].archiveSha256, sha(archive));
  assert.equal(result.files[0].sourceSha256, sha("exe-bytes"));
  assert.equal(result.files[0].member, "ffmpeg.exe");
  const bad = make("swapped");
  await assert.rejects(stageSidecars({ appRoot: APP, target: WIN, manifest: [{ ...entry, clean: true }], deps: bad.deps }), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  assert.equal(bad.fs.existsSync(at("resources", "ffmpeg", "ffmpeg.exe")), false, "nothing staged");
  for (const bogus of [{ extract: "../b.exe" },{ memberSha256: hex("a"), extract: undefined }, { executable: "yes" }, { memberSha256: "z" }]) {
    assert.throws(() => validateSidecarManifest([{ ...entry, ...bogus }], { target: WIN }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
  }
});

// ---- ffmpeg: variants, sources, probes, universal (#491, #492)

function ffmpegCatalog(rows) {
  const crateDir = path.resolve("/rk-fake-ffmpeg-crate");
  const row = (tool, variant, arch, bytes, id = `${tool}-${variant}-macos-${arch}`) => ({
    id, tool, variant, os: "macos", arch, status: "approved", version: "7.1", contentSha256: sha(bytes), sourceSha256: sha("src"),
    attribution: { noticeFiles: ["notices/LGPL-2.1.txt"], sourceUrl: "https://ffmpeg.org/releases/ffmpeg-7.1.tar.xz", licenseSpdx: "LGPL-2.1-or-later", buildProvenance: "recipe" },
  });
  return { crateDir, catalog: { schemaVersion: 2, artifacts: rows.map((args) => row(...args)) }, initial: { [path.join(crateDir, "notices", "LGPL-2.1.txt")]: "LGPL text" } };
}
const NOTICES = { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.test/source" };

test("ffmpeg variants plus sources: one entry stages the export-ass ffmpeg and the decode ffprobe, fetched, unpacked and checked against the approved rows", async () => {
  const { crateDir, catalog, initial } = ffmpegCatalog([["ffmpeg", "export-ass", "aarch64", "ass-ffmpeg"], ["ffmpeg", "decode", "aarch64", "decode-ffmpeg"], ["ffprobe", "decode", "aarch64", "decode-ffprobe"]]);
  const manifest = [{ kind: "ffmpeg", tools: ["ffmpeg", "ffprobe"], variants: { ffmpeg: "export-ass" }, catalog: "c/artifacts.json", notices: NOTICES,
    sources: { ffmpeg: { url: "https://example.test/ffmpeg.zip", sha256: sha("zip-a"), member: "ffmpeg" }, ffprobe: { url: "https://example.test/ffprobe.zip", sha256: sha("zip-b"), member: "ffprobe" } } }];
  const run = (members) => {
    const { fs, files } = fakeFs(initial);
    const urls = [];
    const fetch = async (url) => { urls.push(url); return { ok: true, status: 200, arrayBuffer: async () => Buffer.from(url.endsWith("ffmpeg.zip") ? "zip-a" : "zip-b") }; };
    const extractMember = ({ fileName, destDir }) => { const out = path.join(destDir, fileName); files.set(out, Buffer.from(members[fileName])); return out; };
    const attempt = () => stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs, fetch, extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {}, loadMediaCatalog: () => ({ catalog, crateDir }) }) });
    return { fs, urls, attempt };
  };
  const ok = run({ ffmpeg: "ass-ffmpeg", ffprobe: "decode-ffprobe" });
  const result = await ok.attempt();
  assert.deepEqual(ok.urls, ["https://example.test/ffmpeg.zip", "https://example.test/ffprobe.zip"]);
  assert.equal(ok.fs.readFileSync(at("src-tauri", "binaries", `ffmpeg-${MAC}`)).toString(), "ass-ffmpeg");
  assert.equal(ok.fs.readFileSync(at("src-tauri", "binaries", `ffprobe-${MAC}`)).toString(), "decode-ffprobe");
  assert.deepEqual(result.files.map((file) => [file.name, file.variant, file.catalogRow]), [["ffmpeg", "export-ass", `ffmpeg-export-ass-macos-aarch64`], ["ffprobe", "decode", "ffprobe-decode-macos-aarch64"]]);
  assert.deepEqual(result.externalBinNames, ["ffmpeg", "ffprobe"]);
  const swapped = run({ ffmpeg: "decode-ffmpeg", ffprobe: "decode-ffprobe" });
  await assert.rejects(swapped.attempt(), { code: "STAGE_SIDECAR_HASH_MISMATCH", message: /approved catalog row ffmpeg-export-ass-macos-aarch64/ });
  assert.equal(swapped.fs.existsSync(at("src-tauri", "binaries", `ffmpeg-${MAC}`)), false, "a swapped archive never stages");
});

test("ffmpeg sources and probe are validated; a universal target needs per-arch sources", () => {
  const base = { kind: "ffmpeg", notices: NOTICES, catalog: "c.json" };
  const source = { url: "https://example.test/f.zip", sha256: hex("a"), member: "ffmpeg" };
  assert.doesNotThrow(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: source }, probe: true }], { target: MAC }));
  assert.doesNotThrow(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: { aarch64: source, x86_64: source } } }], { target: UNIVERSAL }));
  let error;
  try {
    validateSidecarManifest([{ ...base, probe: "yes", sources: { ffmpeg: { url: "http://x/f.zip", sha256: "z", member: "a/b", size: 0, extra: 1 }, ffplay: source } }], { target: MAC });
  } catch (caught) { error = caught; }
  for (const needle of ["probe must be a boolean", "sources.ffmpeg.url must be an https:// URL", "sources.ffmpeg.sha256 is required", "sources.ffmpeg.member must be the bare file name", "sources.ffmpeg.size must be a positive integer", "unknown key \"extra\"", "sources.ffplay: not ffmpeg or ffprobe"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
  assert.throws(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: source } }], { target: UNIVERSAL }), /needs \{ aarch64, x86_64 \} sources for universal-apple-darwin/);
});

test("ffmpeg probe (opt-in) checks the LGPL configuration and the encoder list, and skips a binary built for another target", async () => {
  const configuration = (flags) => `ffmpeg version 7.1\nconfiguration: ${flags}\n`;
  const probeRun = ({ flags = "--disable-gpl --disable-nonfree --disable-autodetect", encoders = " V....D libopenh264           OpenH264\n", fixture, probed }) => (command, args, options) => {
    if (command === at("src-tauri", "binaries", `ffmpeg-${MAC}`)) { probed.push(args.join(" ")); return { status: 0, stdout: args[0] === "-version" ? configuration(flags) : encoders, stderr: "" }; }
    return fixture.run(command, args, options);
  };
  const stage = (options, host = { os: "macos", arch: "aarch64" }) => {
    const fixture = ffmpegFixture();
    const probed = [];
    const attempt = stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...ffmpegManifest[0], probe: true }], deps: deps({ fs: fixture.fs, run: probeRun({ ...options, fixture, probed }), host, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir }) }) });
    return { attempt, probed };
  };
  const ok = stage({});
  assert.equal((await ok.attempt).files[0].probe, "passed");
  assert.deepEqual(ok.probed, ["-version", "-hide_banner -encoders"]);
  const cross = stage({}, { os: "windows", arch: "x86_64" });
  assert.equal((await cross.attempt).files[0].probe, "skipped-cross-target");
  assert.deepEqual(cross.probed, []);
  for (const [options, pattern] of [
    [{ flags: "--enable-gpl --enable-libx264" }, /--disable-gpl/],
    [{ flags: "--disable-gpl --disable-nonfree --enable-libx265" }, /--enable-libx265/],
    [{ encoders: " V....D libx264              H.264\n" }, /libx264 encoder/],
  ]) await assert.rejects(stage(options).attempt, { code: "STAGE_SIDECAR_FFMPEG_PROBE_FAILED", message: pattern });
});

test("universal ffmpeg stages each arch slice, runs the stage command per slice with {triple}/{stageDir}, and lipo-merges into <tool>-universal-apple-darwin", async () => {
  const { crateDir, catalog, initial } = ffmpegCatalog([["ffmpeg", "decode", "aarch64", "arm-ffmpeg", "ffmpeg-arm"], ["ffmpeg", "decode", "x86_64", "x86-ffmpeg", "ffmpeg-x86"]]);
  const { fs, files, log } = fakeFs(initial);
  const stageCalls = [];
  const run = (command, args) => {
    if (command === "rustright" && args[0] === "ffmpeg-stage") {
      stageCalls.push(args);
      const [, triple, stageDir] = args;
      files.set(path.resolve(APP, stageDir, `ffmpeg-${triple}`), Buffer.from(triple.startsWith("aarch64") ? "arm-ffmpeg" : "x86-ffmpeg"));
    }
    if (command === "lipo" && args[0] === "-create") files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-ffmpeg"));
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  const manifest = [{ kind: "ffmpeg", tools: ["ffmpeg"], catalog: "c/artifacts.json", notices: NOTICES, stage: { cmd: "rustright", args: ["ffmpeg-stage", "{triple}", "{stageDir}"] } }];
  const result = await stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest, deps: deps({ fs, run, loadMediaCatalog: () => ({ catalog, crateDir }) }) });
  assert.deepEqual(stageCalls, [["ffmpeg-stage", "aarch64-apple-darwin", ".right-release/slices/aarch64-apple-darwin"], ["ffmpeg-stage", "x86_64-apple-darwin", ".right-release/slices/x86_64-apple-darwin"]]);
  assert.equal(fs.readFileSync(at("src-tauri", "binaries", `ffmpeg-${UNIVERSAL}`)).toString(), "fat-ffmpeg");
  const [row] = result.files;
  assert.equal(row.sha256, sha("fat-ffmpeg"));
  assert.deepEqual(row.slices.map((slice) => [slice.triple, slice.sha256, slice.catalogRow]), [["aarch64-apple-darwin", sha("arm-ffmpeg"), "ffmpeg-arm"], ["x86_64-apple-darwin", sha("x86-ffmpeg"), "ffmpeg-x86"]]);
  assert.equal(row.catalogRow, "ffmpeg-arm+ffmpeg-x86");
  const offer = fs.readFileSync(at("src-tauri", "resources", "third-party", "ffmpeg", "FFMPEG-SOURCE-OFFER.txt")).toString();
  assert.ok(offer.includes("ffmpeg-arm") && offer.includes("ffmpeg-x86"), "one source offer names both slices");
  assert.deepEqual(log.chmods.filter(([file]) => file.endsWith(`ffmpeg-${UNIVERSAL}.tmp-7`)).map(([, mode]) => mode), [0o755]);
  validateStageSidecarReceipt(result.receipt, { target: UNIVERSAL, names: ["ffmpeg"] });
  assert.deepEqual(planSidecarStaging({ manifest, target: UNIVERSAL, env: {} }).entries[0].outputs, [`src-tauri/binaries/ffmpeg-${UNIVERSAL}`]);
  const thin = { ...fakeFs(initial) };
  const thinRun = (command, args) => (command === "lipo" && args[0] === "-archs" ? { status: 0, stdout: "arm64\n" } : command === "rustright" ? (thin.files.set(path.resolve(APP, args[2], `ffmpeg-${args[1]}`), Buffer.from(args[1].startsWith("aarch64") ? "arm-ffmpeg" : "x86-ffmpeg")), { status: 0 }) : command === "lipo" ? (thin.files.set(args[args.indexOf("-output") + 1], Buffer.from("x")), { status: 0 }) : { status: 0 });
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest, deps: deps({ fs: thin.fs, run: thinRun, loadMediaCatalog: () => ({ catalog, crateDir }) }) }), { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
});

test("universal onnxruntime stages the arm64 and x86_64 rows thin and lipo-merges the library; an unapproved arch row refuses", async () => {
  const arm = Buffer.from("ort-arm");
  const x86 = Buffer.from("ort-x86");
  const archives = { "https://example.test/arm.tgz": Buffer.from("arm-archive"), "https://example.test/x86.tgz": Buffer.from("x86-archive") };
  const row = (arch, bytes, url, extra = {}) => ({ id: `ort-${arch}`, library: "onnxruntime", version: "1.24.4", os: "macos", arch, status: "approved", artifactFileName: "libonnxruntime.1.24.4.dylib", sha256: sha(bytes), archive: { url, sha256: sha(archives[url]) }, licenseSpdx: "MIT", noticeFiles: ["third-party/ONNXRuntime-MIT.txt"], ...extra });
  const crateDir = path.resolve("/rk-fake-crate");
  const stage = (x86Extra = {}) => {
    const { fs, files } = fakeFs({ [path.join(crateDir, "third-party", "ONNXRuntime-MIT.txt")]: "MIT text" });
    const catalog = { libraries: [row("aarch64", arm, "https://example.test/arm.tgz"), row("x86_64", x86, "https://example.test/x86.tgz", x86Extra)] };
    const run = (command, args) => {
      if (command === "lipo" && args[0] === "-create") files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-ort"));
      if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
      return { status: 0, stdout: "", stderr: "" };
    };
    const extractMember = ({ archive, fileName, destDir }) => { const out = path.join(destDir, fileName); files.set(out, files.get(archive).toString() === "arm-archive" ? arm : x86); return out; };
    const attempt = stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog: "cat.json" }], deps: deps({ fs, run, loadNativeCatalog: () => ({ catalog, crateDir }), fetchBytes: async (url) => archives[url], extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {} }) });
    return { fs, attempt };
  };
  const ok = stage();
  const result = await ok.attempt;
  assert.equal(ok.fs.readFileSync(at("src-tauri", "resources", "runtime", "libonnxruntime.1.24.4.dylib")).toString(), "fat-ort");
  assert.ok(ok.fs.existsSync(at(".right-release", "slices", "aarch64-apple-darwin", "libonnxruntime.1.24.4.dylib")), "the thin slice is staged under .right-release/slices");
  const [file] = result.files;
  assert.equal(file.sha256, sha("fat-ort"));
  assert.equal(file.catalogRow, "ort-aarch64+ort-x86_64");
  assert.deepEqual(file.slices.map((slice) => [slice.triple, slice.sha256]), [["aarch64-apple-darwin", sha(arm)], ["x86_64-apple-darwin", sha(x86)]]);
  assert.equal(file.notices.length, 1, "the shared notice is listed once");
  await assert.rejects(stage({ status: "planned" }).attempt, { code: "STAGE_SIDECAR_NOT_APPROVED", message: /x86_64.*planned/ });
});

// ---- #691 cmake binary, VS-bundled cmake discovery, -A architecture, per-platform generator; #692 local ffmpeg source, universal prebuilt ----

const VS_ROOT = "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community";
const VSWHERE = "C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe";
const VS_CMAKE = `${VS_ROOT}\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\CMake\\bin\\cmake.exe`;

/** Windows-host cmake build: `where cmake` answers per `onPath`, vswhere per `installation`, and only the listed files exist. */
async function cmakeOnWindows({ cmakeEntry = {}, onPath = false, installation = VS_ROOT, existing = [VSWHERE, VS_CMAKE], env = { "ProgramFiles(x86)": "C:\\Program Files (x86)" } } = {}) {
  const { fs, files } = fakeFs();
  const build = fakeSourceBuild({ files, target: WIN, head: COMMIT });
  const run = (command, args, options) => {
    if (command === "where") { build.calls.push({ command, args, options }); return onPath ? { status: 0, stdout: "C:\\tools\\cmake.exe\r\n" } : { status: 1, stdout: "" }; }
    if (command === VSWHERE) { build.calls.push({ command, args, options }); return { status: 0, stdout: installation ? `${installation}\r\n` : "" }; }
    return build.run(command, args, options);
  };
  const exists = (file) => fs.existsSync(file) || file === path.join(SRC_DIR, ".git") || existing.includes(file);
  const result = await stageSidecars({ appRoot: APP, target: WIN, manifest: [{ ...SOURCE_CMAKE, smoke: undefined, ...cmakeEntry }], deps: deps({ fs: { ...fs, existsSync: exists }, run, env, host: { os: "windows", arch: "x86_64" } }) });
  return { result, calls: build.calls, cmakeCalls: build.calls.filter((call) => call.args[0] === "-S" || call.args[0] === "--build") };
}

test("cmake kind runs the explicit cmakeBin, resolving a path against the app root and a bare name through PATH", async () => {
  const manifest = (cmakeBin) => [{ ...SOURCE_CMAKE, smoke: undefined, cmakeBin }];
  const mac = { os: "macos", arch: "aarch64" };
  const pathed = fakeFs();
  const pathedBuild = fakeSourceBuild({ files: pathed.files });
  await stageSidecars({ appRoot: APP, target: MAC, manifest: manifest("tools/cmake/bin/cmake"), deps: deps({ fs: pathed.fs, run: pathedBuild.run, host: mac }) });
  const tools = pathedBuild.calls.filter((call) => call.args[0] === "-S" || call.args[0] === "--build").map((call) => call.command);
  assert.deepEqual(tools, [at("tools", "cmake", "bin", "cmake"), at("tools", "cmake", "bin", "cmake")]);
  const named = fakeFs();
  const namedBuild = fakeSourceBuild({ files: named.files });
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: manifest("cmake3"), deps: deps({ fs: named.fs, run: namedBuild.run, host: mac }) });
  assert.deepEqual(namedBuild.calls.filter((call) => call.args[0] === "-S").map((call) => call.command), ["cmake3"]);
  assert.equal(result.files[0].cmakeBin, "cmake3");
  const plain = fakeFs();
  const plainBuild = fakeSourceBuild({ files: plain.files });
  const plainResult = await stageSidecars({ appRoot: APP, target: MAC, manifest: manifest(undefined), deps: deps({ fs: plain.fs, run: plainBuild.run, host: mac }) });
  assert.equal(plainBuild.calls.find((call) => call.args[0] === "-S").command, "cmake", "default unchanged");
  assert.equal("cmakeBin" in plainResult.files[0], false);
});

test("cmake kind on a Windows host: PATH cmake wins over Visual Studio's, which is found through vswhere when PATH has none", async () => {
  const onPath = await cmakeOnWindows({ onPath: true });
  assert.deepEqual(onPath.cmakeCalls.map((call) => call.command), ["cmake", "cmake"]);
  assert.ok(!onPath.calls.some((call) => call.command === VSWHERE), "vswhere is not consulted when cmake is on PATH");

  const bundled = await cmakeOnWindows();
  assert.deepEqual(bundled.cmakeCalls.map((call) => call.command), [VS_CMAKE, VS_CMAKE]);
  assert.deepEqual(bundled.calls.find((call) => call.command === VSWHERE).args, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.CMake.Project", "-property", "installationPath"]);
  assert.equal(bundled.result.files[0].cmakeBin, VS_CMAKE);

  const explicit = await cmakeOnWindows({ cmakeEntry: { cmakeBin: "cmake-custom" } });
  assert.ok(!explicit.calls.some((call) => call.command === "where" || call.command === VSWHERE), "an explicit cmakeBin skips discovery");
  assert.deepEqual([...new Set(explicit.cmakeCalls.map((call) => call.command))], ["cmake-custom"]);

  for (const missing of [{ existing: [] }, { existing: [VSWHERE] }, { installation: "" }, { env: {} }]) {
    const fallback = await cmakeOnWindows(missing);
    assert.deepEqual([...new Set(fallback.cmakeCalls.map((call) => call.command))], ["cmake"], JSON.stringify(missing));
  }
});

test("cmake kind passes -A architecture after the generator, picks a per-platform generator over generator, and validates both", async () => {
  const { cmakeCalls } = await cmakeOnWindows({ onPath: true, cmakeEntry: { generator: "Ninja", platformGenerators: { win: "Visual Studio 17 2022" }, architecture: "x64" } });
  const configure = cmakeCalls[0].args;
  assert.deepEqual(configure.slice(configure.indexOf("-G"), configure.indexOf("-G") + 4), ["-G", "Visual Studio 17 2022", "-A", "x64"]);
  const mac = fakeFs();
  const macBuild = fakeSourceBuild({ files: mac.files });
  await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...SOURCE_CMAKE, smoke: undefined, generator: "Ninja", platformGenerators: { win: "Visual Studio 17 2022" } }], deps: deps({ fs: mac.fs, run: macBuild.run, host: { os: "macos", arch: "aarch64" } }) });
  const macConfigure = macBuild.calls.find((call) => call.args[0] === "-S").args;
  assert.deepEqual(macConfigure.slice(macConfigure.indexOf("-G"), macConfigure.indexOf("-G") + 2), ["-G", "Ninja"], "the win generator is not used for macOS");
  assert.ok(!macConfigure.includes("-A"));
  let error;
  try {
    validateSidecarManifest([{ ...SOURCE_CMAKE, architecture: "x64;evil", cmakeBin: "", platformGenerators: { beos: "Ninja", win: "bad;gen", mac: 3 } }], { target: WIN });
  } catch (caught) { error = caught; }
  for (const needle of ["architecture must be a cmake -A platform", "cmakeBin must be a cmake executable name or path", "platformGenerators.beos", "platformGenerators.win must be a generator name", "platformGenerators.mac must be a generator name"]) {
    assert.ok(error.message.includes(needle), `missing: ${needle}\n${error.message}`);
  }
  assert.doesNotThrow(() => validateSidecarManifest([{ ...SOURCE_CMAKE, architecture: "ARM64", cmakeBin: "C:\\Program Files\\CMake\\bin\\cmake.exe" }], { target: WIN }));
});

test("ffmpeg local path source copies a vendored build (outside the app root) after checking the approved catalog row; no download", async () => {
  const stage = (bytes, source = { path: "../vendor/ffmpeg-lgpl" }, file = path.resolve(APP, "..", "vendor", "ffmpeg-lgpl", "bin", "ffmpeg")) => {
    const fixture = ffmpegFixture();
    const base = fakeFs({ [path.join(fixture.crateDir, "notices", "LGPL-2.1.txt")]: "LGPL text", [file]: bytes });
    let fetched = false;
    const manifest = [{ ...ffmpegManifest[0], stage: undefined, sources: { ffmpeg: source } }];
    const attempt = () => stageSidecars({ appRoot: APP, target: MAC, manifest, deps: deps({ fs: base.fs, fetch: () => { fetched = true; }, fetchBytes: () => { fetched = true; }, loadMediaCatalog: () => ({ catalog: fixture.catalog, crateDir: fixture.crateDir }) }) });
    return { attempt, fs: base.fs, wasFetched: () => fetched };
  };
  const ok = stage("ffmpeg-bytes");
  const result = await ok.attempt();
  assert.equal(ok.fs.readFileSync(at("src-tauri", "binaries", `ffmpeg-${MAC}`)).toString(), "ffmpeg-bytes");
  assert.equal(ok.wasFetched(), false);
  assert.equal(result.files[0].sha256, sha("ffmpeg-bytes"));
  assert.equal(result.files[0].catalogRow, "ffmpeg-decode-macos-arm64");
  const direct = stage("ffmpeg-bytes", { path: "../vendor/ffmpeg-lgpl/ffmpeg" }, path.resolve(APP, "..", "vendor", "ffmpeg-lgpl", "ffmpeg"));
  await direct.attempt();
  assert.equal(direct.fs.existsSync(at("src-tauri", "binaries", `ffmpeg-${MAC}`)), true, "a path may name the file itself");
  const named = stage("ffmpeg-bytes", { path: "vendor/out", member: "ffmpeg-custom" }, at("vendor", "out", "ffmpeg-custom"));
  await named.attempt();
  assert.equal(named.fs.existsSync(at("src-tauri", "binaries", `ffmpeg-${MAC}`)), true);
  const swapped = stage("not-the-approved-build");
  await assert.rejects(swapped.attempt(), { code: "STAGE_SIDECAR_HASH_MISMATCH", message: /approved catalog row ffmpeg-decode-macos-arm64/ });
  assert.equal(swapped.fs.existsSync(at("src-tauri", "binaries", `ffmpeg-${MAC}`)), false, "a mismatching vendored build never stages");
  const missing = stage("ffmpeg-bytes", { path: "../vendor/elsewhere" });
  await assert.rejects(missing.attempt(), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
});

test("ffmpeg local path sources are validated, mix with the per-arch form and are not mistaken for URLs", () => {
  const base = { kind: "ffmpeg", notices: NOTICES, catalog: "c.json" };
  assert.doesNotThrow(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: { path: "../vendor/ffmpeg-lgpl" }, ffprobe: { path: "vendor/ffprobe", member: "ffprobe" } } }], { target: MAC }));
  assert.doesNotThrow(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: { aarch64: { path: "../vendor/arm" }, x86_64: { path: "../vendor/x86" } } } }], { target: UNIVERSAL }));
  assert.throws(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: { path: "../vendor/ffmpeg" } } }], { target: UNIVERSAL }), /needs \{ aarch64, x86_64 \} sources/);
  let error;
  try {
    validateSidecarManifest([{ ...base, sources: { ffmpeg: { path: "/abs/ffmpeg" }, ffprobe: { path: "https://example.test/ffprobe", sha256: hex("a") } } }], { target: MAC });
  } catch (caught) { error = caught; }
  assert.ok(error.message.includes("sources.ffmpeg.path must be a relative path") && error.message.includes("sources.ffprobe.path must be a relative path"), error.message);
  for (const bogus of [{ path: ".." }, { path: "." }, { path: "C:\\ffmpeg" }, { path: "v/f", member: "a/b" }, { path: "v/f", extra: 1 }]) {
    assert.throws(() => validateSidecarManifest([{ ...base, sources: { ffmpeg: bogus } }], { target: MAC }), { code: "STAGE_SIDECAR_CONFIG_INVALID" }, JSON.stringify(bogus));
  }
});

function universalPrebuiltFixture({ x86Pinned = sha("sd-x86"), staged = true } = {}) {
  const manifest = { binaries: {
    "macos-aarch64": { sha256: sha("sd-arm"), approval_sha256: hex("a") },
    "macos-x86_64": { sha256: x86Pinned, approval_sha256: hex("b") },
  } };
  const initial = { [at("assets", "runtime-manifest.json")]: JSON.stringify(manifest) };
  if (staged) { initial[at("src-tauri", "binaries", "sd-server-aarch64-apple-darwin")] = "sd-arm"; initial[at("src-tauri", "binaries", "sd-server-x86_64-apple-darwin")] = "sd-x86"; }
  const fixture = fakeFs(initial);
  const calls = [];
  const run = (command, args) => {
    calls.push([command, args]);
    if (command === "lipo" && args[0] === "-create") fixture.files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-sd"));
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  const seen = [];
  const approvalDigest = (file) => { seen.push(path.basename(file)); return /x86/.test(file) ? hex("b") : hex("a"); };
  const entry = { kind: "prebuilt", name: "sd-server", manifest: { file: "assets/runtime-manifest.json", entry: "binaries.{platform}", approvalSha256: "approval_sha256", approvalAlgorithm: undefined } };
  return { ...fixture, calls, seen, entry, deps: (extra) => deps({ fs: fixture.fs, run, approvalDigest, host: { os: "macos", arch: "aarch64" }, ...extra }) };
}

test("prebuilt universal verifies each arch against its own manifest entry, then lipo-merges them with the shared helper", async () => {
  const fixture = universalPrebuiltFixture();
  const result = await stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [fixture.entry], deps: fixture.deps() });
  assert.equal(fixture.fs.readFileSync(at("src-tauri", "binaries", `sd-server-${UNIVERSAL}`)).toString(), "fat-sd");
  const lipo = fixture.calls.find(([command, args]) => command === "lipo" && args[0] === "-create")[1];
  assert.deepEqual(lipo.slice(3), [at("src-tauri", "binaries", "sd-server-aarch64-apple-darwin"), at("src-tauri", "binaries", "sd-server-x86_64-apple-darwin")]);
  assert.deepEqual(fixture.seen, ["sd-server-aarch64-apple-darwin", "sd-server-x86_64-apple-darwin"]);
  const [row] = result.files;
  assert.equal(row.sha256, sha("fat-sd"));
  assert.equal(row.platform, "macos-universal");
  assert.deepEqual(row.slices.map((slice) => [slice.triple, slice.platform, slice.sha256, slice.approvalSha256]), [["aarch64-apple-darwin", "macos-aarch64", sha("sd-arm"), hex("a")], ["x86_64-apple-darwin", "macos-x86_64", sha("sd-x86"), hex("b")]]);
  assert.deepEqual(result.externalBinNames, ["sd-server"]);
  validateStageSidecarReceipt(result.receipt, { target: UNIVERSAL, names: ["sd-server"] });
});

test("prebuilt universal takes per-arch `from` inputs, fails closed on a slice mismatch or a missing input, and refuses a thin merge", async () => {
  const fixture = universalPrebuiltFixture({ staged: false });
  fixture.files.set(at("build", "arm", "sd"), Buffer.from("sd-arm"));
  fixture.files.set(at("build", "x86", "sd"), Buffer.from("sd-x86"));
  const entry = { ...fixture.entry, from: { aarch64: "build/arm/sd", x86_64: "build/x86/sd" } };
  await stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [entry], deps: fixture.deps() });
  assert.ok(fixture.fs.existsSync(at("src-tauri", "binaries", `sd-server-${UNIVERSAL}`)));
  const mismatch = universalPrebuiltFixture({ x86Pinned: sha("other") });
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [mismatch.entry], deps: mismatch.deps() }), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  assert.ok(!mismatch.calls.some(([command]) => command === "lipo"), "nothing is merged after a failed slice");
  const absent = universalPrebuiltFixture({ staged: false });
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [absent.entry], deps: absent.deps() }), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND" });
  const thin = universalPrebuiltFixture();
  const thinRun = (command, args) => (command === "lipo" && args[0] === "-archs" ? { status: 0, stdout: "arm64\n" } : command === "lipo" ? (thin.files.set(args[args.indexOf("-output") + 1], Buffer.from("x")), { status: 0 }) : { status: 0 });
  await assert.rejects(stageSidecars({ appRoot: APP, target: UNIVERSAL, manifest: [thin.entry], deps: thin.deps({ run: thinRun }) }), { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
});

test("prebuilt `from` per-arch form validates, selects the target arch on a thin target, and a plain string is refused for universal", async () => {
  assert.throws(() => validateSidecarManifest([{ ...PREBUILT, from: "build/sd" }], { target: UNIVERSAL }), /from must be \{ aarch64, x86_64 \} for universal-apple-darwin/);
  assert.throws(() => validateSidecarManifest([{ ...PREBUILT, from: { aarch64: "build/sd" } }], { target: UNIVERSAL }), /from needs both aarch64 and x86_64/);
  assert.throws(() => validateSidecarManifest([{ ...PREBUILT, from: { arm64: "build/sd", x86_64: "../sd" } }], { target: MAC }), (error) => error.message.includes("from.arm64: arches are") && error.message.includes("from.x86_64 must be relative"));
  assert.doesNotThrow(() => validateSidecarManifest([{ ...PREBUILT }], { target: UNIVERSAL }), "omitted from uses the staged slice files");
  const fixture = prebuiltFixture({ stage: false });
  fixture.files.set(at("build", "arm", "sd"), Buffer.from("sd-bytes"));
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...PREBUILT, from: { aarch64: "build/arm/sd", x86_64: "build/x86/sd" } }], deps: deps({ fs: fixture.fs, approvalDigest: () => hex("c") }) });
  assert.equal(result.files[0].changed, true);
  assert.equal(fixture.fs.readFileSync(at("src-tauri", "binaries", `sd-server-${MAC}`)).toString(), "sd-bytes");
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ ...PREBUILT, from: { x86_64: "build/x86/sd" } }], deps: deps({ fs: prebuiltFixture({ stage: false }).fs }) }), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND", message: /not aarch64/ });
});

// ---- archive member selection by path (CutRight #724: ORT DirectML nupkgs hold duplicate base names)

const NUPKG_LISTING = [
  "[Content_Types].xml", "runtimes/win-arm64/native/onnxruntime.dll", "runtimes/win-x64/native/onnxruntime.dll", "runtimes/win-x64/native/onnxruntime_providers_shared.dll",
  "runtimes/win-x64/native/", "bin/x64-win/DirectML.dll", "bin/arm64-win/DirectML.dll", "lib/net8.0/Microsoft.ML.OnnxRuntime.dll", "",
].join("\r\n");

/** A fake tar: `-tf` prints `listing`, `-xf` records the extracted member (the last argument). */
function fakeTar(listing) {
  const calls = [];
  const run = (command, args) => { calls.push([command, ...args]); return { status: 0, stdout: args[0] === "-tf" ? listing : "", stderr: "" }; };
  return { run, calls, extracted: () => calls.filter((call) => call[1] === "-xf").map((call) => call[call.length - 1]) };
}

test("a member path selects the one archive member at exactly that path among duplicate base names", () => {
  const destDir = path.resolve("/rk-extract-dest");
  const cases = [
    ["runtimes/win-x64/native/onnxruntime.dll", "runtimes/win-x64/native/onnxruntime.dll"],
    ["runtimes/win-arm64/native/onnxruntime.dll", "runtimes/win-arm64/native/onnxruntime.dll"],
    ["bin/x64-win/DirectML.dll", "bin/x64-win/DirectML.dll"],
    ["./bin/arm64-win/DirectML.dll", "bin/arm64-win/DirectML.dll"],
  ];
  for (const [member, expected] of cases) {
    const tar = fakeTar(NUPKG_LISTING);
    const out = defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName: member, destDir, run: tar.run });
    assert.deepEqual(tar.extracted(), [expected]);
    assert.equal(out, path.join(destDir, ...expected.split("/")));
  }
  const dotted = fakeTar("./runtimes/win-x64/native/onnxruntime.dll\n./runtimes/win-arm64/native/onnxruntime.dll\n");
  defaultExtractMember({ archive: path.resolve("/rk-cache/a.tgz"), fileName: "runtimes/win-x64/native/onnxruntime.dll", destDir, run: dotted.run });
  assert.deepEqual(dotted.extracted(), ["./runtimes/win-x64/native/onnxruntime.dll"], "the listed spelling is what tar is asked for");
});

test("a member path must match the full path exactly once and case-sensitively", () => {
  const destDir = path.resolve("/rk-extract-dest");
  const attempt = (listing, fileName) => defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName, destDir, run: fakeTar(listing).run });
  for (const fileName of ["runtimes/win-x64/native/OnnxRuntime.dll", "native/onnxruntime.dll", "win-x64/native/onnxruntime.dll", "runtimes/win-x64/native", "runtimes/win-x64/native/onnxruntime.dll.bak"]) {
    assert.throws(() => attempt(NUPKG_LISTING, fileName), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /has 0 members at the path/ }, fileName);
  }
  assert.throws(() => attempt("bin/x64-win/DirectML.dll\n./bin/x64-win/DirectML.dll\n", "bin/x64-win/DirectML.dll"), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /has 2 members at the path/ });
});

test("a bare member name keeps the unique-basename behaviour and still refuses duplicates", () => {
  const destDir = path.resolve("/rk-extract-dest");
  const one = fakeTar(NUPKG_LISTING);
  const out = defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName: "Microsoft.ML.OnnxRuntime.dll", destDir, run: one.run });
  assert.deepEqual(one.extracted(), ["lib/net8.0/Microsoft.ML.OnnxRuntime.dll"]);
  assert.equal(out, path.join(destDir, "lib", "net8.0", "Microsoft.ML.OnnxRuntime.dll"));
  for (const fileName of ["onnxruntime.dll", "DirectML.dll"]) {
    const dup = fakeTar(NUPKG_LISTING);
    assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName, destDir, run: dup.run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: new RegExp(`has 2 members named ${fileName.replace(".", "\\.")}, expected exactly one`) });
    assert.deepEqual(dup.extracted(), [], "nothing is extracted on an ambiguous name");
  }
  assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName: "missing.dll", destDir, run: fakeTar(NUPKG_LISTING).run }), { message: /has 0 members named missing\.dll/ });
});

test("traversal-shaped member requests are refused before the archive is even listed", () => {
  const destDir = path.resolve("/rk-extract-dest");
  const bad = ["../x.dll", "a/../x.dll", "/abs/x.dll", "a//b", "a\\b", "C:/x", "C:\\x.dll", "C:x.dll", "a/b/", "a/./b", "./", "..", ".", "", "a/b*.dll", "a/[b].dll", "a/b?.dll", undefined, 7];
  for (const fileName of bad) {
    const tar = fakeTar(NUPKG_LISTING);
    assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName, destDir, run: tar.run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED" }, String(fileName));
    assert.equal(tar.calls.length, 0, `tar must not run for ${String(fileName)}`);
    assert.ok(normalizeArchiveMember(fileName).error, String(fileName));
  }
  assert.deepEqual(normalizeArchiveMember("./a/b.dll"), { value: "a/b.dll", byPath: true });
  assert.deepEqual(normalizeArchiveMember("b.dll"), { value: "b.dll", byPath: false });
});

test("a listed member that would resolve outside the extraction directory is refused", () => {
  const destDir = path.resolve("/rk-extract-dest");
  const tar = fakeTar("../evil/onnxruntime.dll\n");
  assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/a.nupkg"), fileName: "onnxruntime.dll", destDir, run: tar.run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /outside the extraction directory/ });
});

test("remote extract accepts an archive path, refuses traversal in the manifest, and the output stays at the declared dest", async () => {
  const archive = "nupkg-bytes";
  const member = "bin/x64-win/DirectML.dll";
  const entry = { kind: "remote", name: "directml", url: "https://example.test/directml.nupkg", sha256: sha(archive), extract: member, memberSha256: sha("dll-bytes"), dest: "resources/runtime/DirectML.dll" };
  assert.doesNotThrow(() => validateSidecarManifest([entry], { target: WIN }));
  for (const extract of ["../DirectML.dll", "/bin/DirectML.dll", "bin//DirectML.dll", "bin\\x64-win\\DirectML.dll", "C:/DirectML.dll", "bin/../DirectML.dll"]) {
    assert.throws(() => validateSidecarManifest([{ ...entry, extract }], { target: WIN }), { code: "STAGE_SIDECAR_CONFIG_INVALID" }, extract);
  }
  const { fs, files } = fakeFs();
  const { fetch } = fetchOf(archive);
  const seen = [];
  const extractMember = ({ fileName, destDir }) => { seen.push(fileName); const out = path.join(destDir, ...fileName.split("/")); files.set(out, Buffer.from("dll-bytes")); return out; };
  const result = await stageSidecars({ appRoot: APP, target: WIN, manifest: [entry], deps: deps({ fs, fetch, extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {} }) });
  assert.deepEqual(seen, [member]);
  assert.equal(fs.readFileSync(at("resources", "runtime", "DirectML.dll")).toString(), "dll-bytes");
  assert.equal(result.files[0].path, "resources/runtime/DirectML.dll");
  assert.equal(result.files[0].member, member);
  assert.ok(![...files.keys()].some((key) => key.startsWith(path.resolve(APP) + path.sep) && key.includes("x64-win")), "the member path is not recreated under the app");
});

test("onnxruntime extracts the catalog archive.member when present and keeps writing the artifactFileName", async () => {
  const member = "runtimes/win-x64/native/onnxruntime.dll";
  const catalogFor = (archiveMember) => ({
    libraries: [{ id: "ort-win", library: "onnxruntime", version: "1.24.4", os: "windows", arch: "x86_64", status: "pinned", sha256: sha("dll"), artifactFileName: "onnxruntime.dll", archive: { url: "https://example.test/ort.nupkg", sha256: sha("archive"), ...(archiveMember === undefined ? {} : { member: archiveMember }) }, noticeFiles: ["NOTICE.txt"] }],
  });
  const crateDir = path.resolve("/rk-fake-crates/rightkit-ffmpeg");
  const stage = async (archiveMember) => {
    const { fs, files } = fakeFs({ [path.join(crateDir, "NOTICE.txt")]: "notice" });
    const seen = [];
    const extractMember = ({ fileName, destDir }) => { seen.push(fileName); const out = path.join(destDir, ...fileName.split("/")); files.set(out, Buffer.from("dll")); return out; };
    const result = await stageSidecars({
      appRoot: APP, target: WIN, manifest: [{ kind: "onnxruntime", version: "1.24.4", catalog: "cat.json" }],
      deps: deps({ fs, loadNativeCatalog: () => ({ catalog: catalogFor(archiveMember), crateDir }), fetchBytes: async () => Buffer.from("archive"), extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {} }),
    });
    return { seen, files, result };
  };
  const withMember = await stage(member);
  assert.deepEqual(withMember.seen, [member]);
  assert.equal(withMember.result.files[0].name, "onnxruntime.dll");
  assert.ok(withMember.files.has(at("src-tauri", "resources", "runtime", "onnxruntime.dll")), "written under the artifactFileName, not the member path");
  assert.deepEqual((await stage(undefined)).seen, ["onnxruntime.dll"], "without archive.member the artifactFileName is requested as before");
  for (const bad of ["../onnxruntime.dll", "/onnxruntime.dll", "a//b.dll", "a\\b.dll", "C:/x.dll"]) {
    await assert.rejects(stage(bad), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /invalid archive member/ }, bad);
  }
});

test("a bare member name skips the same-named DWARF copy inside a .dSYM bundle (ORT osx tgz)", () => {
  const destDir = path.resolve("/rk-extract-dest");
  const listing = [
    "onnxruntime-osx-arm64-1.24.4/", "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib",
    "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib.dSYM/Contents/Resources/DWARF/libonnxruntime.1.24.4.dylib",
    "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.dylib", "",
  ].join(String.fromCharCode(10));
  const tar = fakeTar(listing);
  defaultExtractMember({ archive: path.resolve("/rk-cache/ort.tgz"), fileName: "libonnxruntime.1.24.4.dylib", destDir, run: tar.run });
  assert.deepEqual(tar.extracted(), ["onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib"]);
  const onlyDsym = fakeTar(`x/lib/a.dylib.dSYM/Contents/Resources/DWARF/a.dylib${String.fromCharCode(10)}`);
  assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/ort.tgz"), fileName: "a.dylib", destDir, run: onlyDsym.run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /has 0 members named a\.dylib/ });
  const pathed = fakeTar(listing);
  defaultExtractMember({ archive: path.resolve("/rk-cache/ort.tgz"), fileName: "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib.dSYM/Contents/Resources/DWARF/libonnxruntime.1.24.4.dylib", destDir, run: pathed.run });
  assert.equal(pathed.extracted().length, 1, "an explicit full path can still address the dSYM copy");
});

// ---- bogusyogi/rightkit#889: a macOS `universal` onnxruntime row (a .zip) serves either Apple arch and the universal target ----

const ORT_ZIP_MEMBER = "onnxruntime-osx-universal2-1.30.0/lib/libonnxruntime.dylib";
const ORT_REAL_CATALOG = JSON.parse(readFileSync(new URL("../../crates/rightkit-ffmpeg/catalog/native-libraries.json", import.meta.url), "utf8"));
const ortRealRow = (id) => ORT_REAL_CATALOG.libraries.find((row) => row.id === id);

/** The real 1.30.0 universal row with its library/archive digests swapped for the fixture bytes (the real ones cannot be forged). */
function ortUniversalFixture({ library = "ort-universal-dylib", archive = "ort-universal-zip", target = MAC, version = "1.30.0", libraries, lipoArchs = "x86_64 arm64\n", extraFiles = {} } = {}) {
  const crateDir = path.resolve("/rk-fake-crate");
  const real = ortRealRow("onnxruntime-1.30.0-macos-universal2");
  const row = { ...real, sha256: sha(library), archive: { ...real.archive, sha256: sha(archive) } };
  const catalog = { libraries: libraries ? libraries(row) : ORT_REAL_CATALOG.libraries.map((entry) => (entry.id === row.id ? row : entry)) };
  const noticeFiles = Object.fromEntries(real.noticeFiles.map((file) => [path.join(crateDir, ...file.split("/")), `notice ${file}`]));
  const { fs, files } = fakeFs({ ...noticeFiles, ...extraFiles });
  const calls = [];
  const fetched = [];
  const extracted = [];
  const run = (command, args) => {
    calls.push([command, ...args]);
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: lipoArchs, stderr: "" };
    if (command === "lipo" && args[0] === "-create") files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-ort"));
    return { status: 0, stdout: "", stderr: "" };
  };
  const extractMember = ({ archive: file, fileName, destDir }) => {
    extracted.push(fileName);
    const out = path.join(destDir, ...fileName.split("/"));
    files.set(out, Buffer.from(library));
    return out;
  };
  const stage = () => stageSidecars({
    appRoot: APP, target, manifest: [{ kind: "onnxruntime", version, catalog: "cat.json" }],
    deps: deps({ fs, run, loadNativeCatalog: () => ({ catalog, crateDir }), fetchBytes: async (url) => { fetched.push(url); return Buffer.from(archive); }, extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {} }),
  });
  return { fs, files, calls, fetched, extracted, stage, row, crateDir };
}

test("the real catalog's 1.30.0 universal zip row serves an aarch64 and an x86_64 macOS target, extracted by its derived member path", async () => {
  const real = ortRealRow("onnxruntime-1.30.0-macos-universal2");
  assert.deepEqual([real.os, real.arch, real.status, real.archive.member], ["macos", "universal", "approved", undefined]);
  assert.ok(real.archive.url.endsWith("/onnxruntime-osx-universal2-1.30.0.zip"));
  for (const target of [MAC, "x86_64-apple-darwin"]) {
    const fixture = ortUniversalFixture({ target });
    const result = await fixture.stage();
    assert.deepEqual(fixture.extracted, [ORT_ZIP_MEMBER], target);
    assert.deepEqual(fixture.fetched, [real.archive.url]);
    const [file] = result.files;
    assert.equal(file.catalogRow, "onnxruntime-1.30.0-macos-universal2");
    assert.equal(file.name, "libonnxruntime.dylib", "written under artifactFileName, never the member path");
    assert.equal(fixture.fs.readFileSync(at("src-tauri", "resources", "runtime", "libonnxruntime.dylib")).toString(), "ort-universal-dylib");
    assert.equal(file.sha256, sha("ort-universal-dylib"));
    assert.deepEqual(file.notices.map((notice) => notice.name), ["ONNXRuntime-MIT.txt", "ONNXRuntime-1.30.0-ThirdPartyNotices.txt"], "both notice files are staged");
    for (const notice of file.notices) assert.equal(fixture.fs.readFileSync(at("src-tauri", "resources", "runtime", "LICENSES", notice.name)).toString().startsWith("notice "), true);
    assert.equal(fixture.calls.filter((call) => call[0] === "lipo").length, 0, "a thin target never runs lipo");
  }
});

test("an exact-arch row is preferred over a universal row; stale-unverified rows are never selected", async () => {
  const exact = (row, extra = {}) => ({ ...row, id: "ort-1.30.0-arm64", arch: "aarch64", artifactFileName: "libonnxruntime.1.30.0.dylib", archive: { ...row.archive, url: "https://example.test/onnxruntime-osx-arm64-1.30.0.tgz" }, ...extra });
  const preferred = ortUniversalFixture({ libraries: (row) => [row, exact(row)] });
  const result = await preferred.stage();
  assert.equal(result.files[0].catalogRow, "ort-1.30.0-arm64");
  assert.deepEqual(preferred.extracted, ["libonnxruntime.1.30.0.dylib"], "the exact tgz row keeps its bare artifactFileName member");
  assert.deepEqual(preferred.fetched, ["https://example.test/onnxruntime-osx-arm64-1.30.0.tgz"]);

  // A stale exact-arch row is skipped in favour of the universal row; the real catalog's stale 1.24.4 rows never serve.
  const skipped = ortUniversalFixture({ libraries: (row) => [row, exact(row, { status: "stale-unverified" })] });
  assert.equal((await skipped.stage()).files[0].catalogRow, "onnxruntime-1.30.0-macos-universal2");
  for (const target of [MAC, "x86_64-apple-darwin"]) {
    const stale = ortUniversalFixture({ target, version: "1.24.4" });
    await assert.rejects(stale.stage(), (error) => error.code === "STAGE_SIDECAR_NOT_APPROVED" && /stale-unverified|no onnxruntime 1\.24\.4 macos\/x86_64 row/.test(error.message), target);
    assert.deepEqual(stale.fetched, [], "nothing is downloaded for a stale row");
  }
  const staleOnly = ortUniversalFixture({ libraries: (row) => [{ ...row, status: "stale-unverified" }] });
  await assert.rejects(staleOnly.stage(), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /no onnxruntime 1\.30\.0 macos\/aarch64 row/ });
  const plannedOnly = ortUniversalFixture({ libraries: (row) => [{ ...row, status: "planned" }, exact(row, { status: "planned" })] });
  await assert.rejects(plannedOnly.stage(), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /planned, not approved or pinned/ });
  const twice = ortUniversalFixture({ libraries: (row) => [row, { ...row, id: "second-universal", status: "pinned" }] });
  await assert.rejects(twice.stage(), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /2 onnxruntime 1\.30\.0 macos\/aarch64 rows/ });
  // Windows rows are untouched by the universal rule: the pinned 1.24.4 row is still selected by exact (os, arch) and gets as far as its own notice files.
  const win = ortUniversalFixture({ target: WIN, version: "1.24.4" });
  await assert.rejects(win.stage(), { code: "STAGE_SIDECAR_NOTICE_MISSING", message: /ONNXRuntime-1\.24\.4-ThirdPartyNotices\.txt/ });
});

test("universal-apple-darwin stages one universal row once and verifies it with lipo -archs instead of merging", async () => {
  const fixture = ortUniversalFixture({ target: UNIVERSAL });
  const result = await fixture.stage();
  const [file] = result.files;
  assert.deepEqual(fixture.fetched, [fixture.row.archive.url], "the archive is downloaded once");
  assert.deepEqual(fixture.extracted, [ORT_ZIP_MEMBER], "extracted once");
  assert.equal(fixture.calls.filter((call) => call[0] === "lipo" && call[1] === "-create").length, 0, "no lipo -create merge");
  assert.deepEqual(fixture.calls.filter((call) => call[0] === "lipo").map((call) => call[1]), ["-archs"]);
  assert.equal(fixture.fs.readFileSync(at("src-tauri", "resources", "runtime", "libonnxruntime.dylib")).toString(), "ort-universal-dylib");
  assert.equal(fixture.fs.existsSync(at(".right-release", "slices", "aarch64-apple-darwin", "libonnxruntime.dylib")), false, "no per-slice staging");
  assert.equal(file.sha256, sha("ort-universal-dylib"), "the staged dylib is the row's sha256");
  assert.equal(file.catalogRow, "onnxruntime-1.30.0-macos-universal2");
  assert.deepEqual(file.slices.map((slice) => [slice.triple, slice.catalogRow]), [["aarch64-apple-darwin", file.catalogRow], ["x86_64-apple-darwin", file.catalogRow]]);
  assert.equal(file.notices.length, 2);
  // A thin file behind a universal row is refused; a wrong library digest stages nothing.
  await assert.rejects(ortUniversalFixture({ target: UNIVERSAL, lipoArchs: "arm64\n" }).stage(), { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
  const bad = ortUniversalFixture({ target: UNIVERSAL });
  bad.row.sha256 = sha("other");
  await assert.rejects(bad.stage(), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  assert.equal(bad.fs.existsSync(at("src-tauri", "resources", "runtime", "libonnxruntime.dylib")), false);
});

test("universal-apple-darwin keeps the per-arch lipo merge for exact rows and refuses a universal row mixed with an exact one", async () => {
  const exactRow = (row, arch) => ({ ...row, id: `ort-1.30.0-${arch}`, arch, artifactFileName: "libonnxruntime.1.30.0.dylib", archive: { ...row.archive, url: `https://example.test/onnxruntime-osx-${arch}-1.30.0.tgz` } });
  const both = ortUniversalFixture({ target: UNIVERSAL, libraries: (row) => [row, exactRow(row, "aarch64"), exactRow(row, "x86_64")] });
  const result = await both.stage();
  assert.equal(result.files[0].catalogRow, "ort-1.30.0-aarch64+ort-1.30.0-x86_64");
  assert.equal(both.calls.filter((call) => call[0] === "lipo" && call[1] === "-create").length, 1, "exact rows are still lipo-merged");
  const mixed = ortUniversalFixture({ target: UNIVERSAL, libraries: (row) => [row, exactRow(row, "aarch64")] });
  await assert.rejects(mixed.stage(), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /mixes a universal row with exact-arch rows/ });
});

test("zip archive members go through the same guarded member-path logic: traversal is rejected", async () => {
  const zipListing = ["onnxruntime-osx-universal2-1.30.0/", `${ORT_ZIP_MEMBER}`, "onnxruntime-osx-universal2-1.30.0/include/onnxruntime_c_api.h", ""].join("\n");
  const destDir = path.resolve("/rk-extract-dest");
  const tar = fakeTar(zipListing);
  const out = defaultExtractMember({ archive: path.resolve("/rk-cache/onnxruntime-osx-universal2-1.30.0.zip"), fileName: ORT_ZIP_MEMBER, destDir, run: tar.run });
  assert.deepEqual(tar.extracted(), [ORT_ZIP_MEMBER]);
  assert.equal(out, path.join(destDir, ...ORT_ZIP_MEMBER.split("/")));
  for (const fileName of ["../onnxruntime-osx-universal2-1.30.0/lib/libonnxruntime.dylib", "/abs/lib/libonnxruntime.dylib", "onnxruntime-osx-universal2-1.30.0/../lib/libonnxruntime.dylib", "a\\b.dylib"]) {
    const guarded = fakeTar(zipListing);
    assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/x.zip"), fileName, destDir, run: guarded.run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED" }, fileName);
    assert.equal(guarded.calls.length, 0);
  }
  assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/x.zip"), fileName: ORT_ZIP_MEMBER, destDir, run: fakeTar("../evil/lib/libonnxruntime.dylib\n").run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /has 0 members at the path/ });
  assert.throws(() => defaultExtractMember({ archive: path.resolve("/rk-cache/x.zip"), fileName: "libonnxruntime.dylib", destDir, run: fakeTar("../libonnxruntime.dylib\n").run }), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /outside the extraction directory/ });
  // A row whose declared or zip-derived member is unsafe is refused before anything is downloaded.
  for (const member of ["../lib/libonnxruntime.dylib", "/lib/libonnxruntime.dylib"]) {
    const fixture = ortUniversalFixture({ libraries: (row) => [{ ...row, archive: { ...row.archive, member } }] });
    await assert.rejects(fixture.stage(), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /invalid archive member/ }, member);
    assert.deepEqual(fixture.fetched, []);
  }
  const odd = ortUniversalFixture({ libraries: (row) => [{ ...row, archive: { ...row.archive, url: "https://example.test/ort*glob.zip" } }] });
  await assert.rejects(odd.stage(), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /invalid archive member/ }, "a zip name that would not be a safe member path is refused");
});

// ---- plan item 2.8: kind "native-library" (the DirectML companion, PDFium, whisper.cpp, ggml, sherpa-onnx) ----

const CPU_ARM64 = 0x0100000c;
const CPU_X86_64 = 0x01000007;
const u32le = (v) => { const b = Buffer.alloc(4); b.writeUInt32LE(v); return b; };
const u64le = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const u32be = (v) => { const b = Buffer.alloc(4); b.writeUInt32BE(v); return b; };
const roundUp = (v, to) => Math.floor((v + to - 1) / to) * to;

/** A thin 64-bit Mach-O (mirrors content-digest.test.mjs): __TEXT + __LINKEDIT, plus an optional code signature at the tail. */
function machoCpu(cputype, payload, signature) {
  const sizeofcmds = 144 + (signature ? 16 : 0);
  const textOff = 0x1000;
  const text = Buffer.alloc(64, 0xaa);
  const linkeditOff = textOff + text.length;
  const sigOff = signature ? roundUp(linkeditOff + payload.length, 16) : null;
  const fileLen = signature ? sigOff + signature.length : linkeditOff + payload.length;
  const filesize = fileLen - linkeditOff;
  const page = cputype === CPU_ARM64 ? 0x4000 : 0x1000;
  const segment = (name, vm, vmsize, off, size) => { const n = Buffer.alloc(16); n.write(name, "latin1"); return Buffer.concat([u32le(0x19), u32le(72), n, u64le(vm), u64le(vmsize), u64le(off), u64le(size), Buffer.alloc(16)]); };
  const head = [u32le(0xfeedfacf), u32le(cputype), u32le(0), u32le(2), u32le(2 + (signature ? 1 : 0)), u32le(sizeofcmds), u32le(0), u32le(0),
    segment("__TEXT", 0, 0x4000, textOff, text.length), segment("__LINKEDIT", 0x4000, roundUp(filesize, page), linkeditOff, filesize)];
  if (signature) head.push(u32le(0x1d), u32le(16), u32le(sigOff), u32le(signature.length));
  const file = Buffer.alloc(fileLen);
  Buffer.concat(head).copy(file); text.copy(file, textOff); payload.copy(file, linkeditOff);
  if (signature) signature.copy(file, sigOff);
  return file;
}

/** A universal (fat) Mach-O of [cputype, thin] slices. */
function fatOf(slices) {
  const head = [u32be(0xcafebabe), u32be(slices.length)];
  const parts = [];
  let cursor = 8 + 20 * slices.length;
  let offset = roundUp(cursor, 0x4000);
  for (const [cputype, slice] of slices) {
    head.push(u32be(cputype), u32be(0), u32be(offset), u32be(slice.length), u32be(14));
    parts.push(Buffer.alloc(offset - cursor), slice);
    cursor = offset + slice.length;
    offset = roundUp(cursor, 0x4000);
  }
  return Buffer.concat([Buffer.concat(head), ...parts]);
}

/** A minimal PE (mirrors content-digest.test.mjs): two sections, an optional Authenticode certificate table at the tail. */
function peOf(machine, fill, cert) {
  const out = Buffer.alloc(0x400);
  out.write("MZ", 0, "latin1"); out.writeUInt32LE(0x80, 0x3c);
  out.write("PE\0\0", 0x80, "latin1"); out.writeUInt16LE(machine, 0x84); out.writeUInt16LE(2, 0x86); out.writeUInt16LE(240, 0x94);
  const opt = 0x98;
  out.writeUInt16LE(0x20b, opt); out.writeUInt32LE(0x400, opt + 60); out.writeUInt32LE(0, opt + 64); out.writeUInt32LE(16, opt + 108);
  [[0x400, 0x200], [0x600, 0x200]].forEach(([off, size], i) => { const at = opt + 240 + i * 40; out.writeUInt32LE(size, at + 16); out.writeUInt32LE(off, at + 20); });
  const body = Buffer.concat([out, Buffer.alloc(0x200, fill), Buffer.alloc(0x200, 0x22)]);
  if (!cert) return body;
  body.writeUInt32LE(body.length, opt + 112 + 32); body.writeUInt32LE(cert.length, opt + 112 + 36);
  return Buffer.concat([body, cert]);
}

/** The signature-independent digest a catalog row pins for `bytes` (a thin file, or the one arch of a universal file). */
const contentOf = (bytes, arch) => contentIdentity(bytes).slices.find((slice) => arch === undefined || slice.arch === arch).sha256;

const NATIVE_CRATE = path.resolve("/rk-native-crate");
const runtimeAt = (...parts) => at("src-tauri", "resources", "runtime", ...parts);
const licenseAt = (name) => runtimeAt("LICENSES", name);
const NATIVE_CATALOG_ENTRY = { catalog: "cat.json" };

/** A fake pinned archive: its url, a label standing for its bytes, and the members `tar` lists and extracts. */
const archiveOf = (url, members) => ({ url, text: `archive:${url}`, members });
const archiveRef = (archive, member) => ({ url: archive.url, sha256: sha(archive.text), ...(member ? { member } : {}) });

/** A catalog row shaped like the real ones; `bytes` is what the pinned file contains. */
const rowOf = (id, library, version, os, arch, bytes, extra = {}) => ({
  id, library, version, os, arch, status: "approved", linkage: "dynamic-library", sha256: sha(bytes), licenseSpdx: "MIT", noticeFiles: [`third-party/${library}-MIT.txt`], ...extra,
});

/**
 * A fake app for native-library staging: the catalog rows, the pinned archives (served by fetchBytes, listed and unpacked by a fake
 * `tar` so the real defaultExtractMember member rules apply), local files, and a lipo that reports `lipoArchs`.
 */
function nativeKit({ rows, archives = [], files: seeded = {}, target = WIN, lipoArchs = "x86_64 arm64\n" }) {
  const notices = Object.fromEntries([...new Set(rows.flatMap((row) => row.noticeFiles ?? []))].map((file) => [path.join(NATIVE_CRATE, ...file.split("/")), `notice ${file}`]));
  const { fs, files, log } = fakeFs({ ...notices, ...seeded });
  const byText = new Map(archives.map((archive) => [archive.text, archive]));
  const byUrl = new Map(archives.map((archive) => [archive.url, archive]));
  const fetched = [];
  const messages = [];
  const calls = [];
  let scratch = 0;
  const archiveAt = (file) => byText.get(String(files.get(path.resolve(file))));
  const run = (command, args) => {
    calls.push([command, ...args]);
    if (command === "tar" && args[0] === "-tf") return { status: 0, stdout: Object.keys(archiveAt(args[1])?.members ?? {}).join("\n"), stderr: "" };
    if (command === "tar" && args[0] === "-xf") { files.set(path.join(args[3], ...args[4].split("/")), archiveAt(args[1]).members[args[4]]); return { status: 0, stdout: "", stderr: "" }; }
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: lipoArchs, stderr: "" };
    if (command === "lipo" && args[0] === "-create") files.set(args[args.indexOf("-output") + 1], Buffer.from(`fat:${args.slice(3).map((file) => files.get(path.resolve(file))).join("+")}`));
    return { status: 0, stdout: "", stderr: "" };
  };
  const stage = (manifest, extra = {}) => stageSidecars({
    appRoot: APP, target, manifest,
    deps: deps({
      fs, run, log: (line) => messages.push(line), loadNativeCatalog: () => ({ catalog: { libraries: rows }, crateDir: NATIVE_CRATE }),
      fetchBytes: async (url) => { fetched.push(url); return Buffer.from(byUrl.get(url)?.text ?? "unexpected download"); },
      makeTempDir: () => path.resolve(`/rk-native-scratch-${scratch += 1}`), removeDir: () => {}, ...extra,
    }),
  });
  /** Everything stage-sidecar wrote under src-tauri (the staged libraries, notices and receipt). */
  const written = () => [...files.keys()].filter((key) => key.startsWith(at("src-tauri") + path.sep)).map((key) => path.relative(APP, key).split(path.sep).join("/")).sort();
  const extracted = () => calls.filter((call) => call[0] === "tar" && call[1] === "-xf").map((call) => call[5]);
  return { fs, files, log, calls, fetched, messages, stage, written, extracted };
}

const ORT_WIN_X64 = peOf(0x8664, 0x11);
const DML_WIN_X64 = peOf(0x8664, 0x33);
const ORT_MEMBER = "runtimes/win-x64/native/onnxruntime.dll";
const DML_MEMBER = "bin/x64-win/DirectML.dll";
const DML_ID = "directml-1.15.4-windows-x64";
const ORT_WIN_ID = "onnxruntime-1.24.4-windows-x64-directml";

/** The real Windows ORT + DirectML catalog pair, with the fixture bytes behind both nupkgs (each also holds a same-named win-arm64 decoy). */
function winOrtFixture({ ort = {}, directml = {}, target = WIN } = {}) {
  const ortNupkg = archiveOf("https://api.example.test/ort.nupkg", { [ORT_MEMBER]: ORT_WIN_X64, "runtimes/win-arm64/native/onnxruntime.dll": peOf(0xaa64, 0x12), "lib/net8.0/Microsoft.ML.OnnxRuntime.dll": Buffer.from("managed") });
  const dmlNupkg = archiveOf("https://api.example.test/directml.nupkg", { [DML_MEMBER]: DML_WIN_X64, "bin/arm64-win/DirectML.dll": peOf(0xaa64, 0x34), "bin/x86-win/DirectML.dll": Buffer.from("x86") });
  const rows = [
    rowOf(ORT_WIN_ID, "onnxruntime", "1.24.4", "windows", "x86_64", ORT_WIN_X64, {
      status: "pinned", artifactFileName: "onnxruntime.dll", contentSha256: contentOf(ORT_WIN_X64), archive: archiveRef(ortNupkg, ORT_MEMBER), companions: [DML_ID], hostProof: "never-run",
      noticeFiles: ["third-party/ONNXRuntime-MIT.txt", "third-party/ONNXRuntime-1.24.4-ThirdPartyNotices.txt"], ...ort,
    }),
    rowOf(DML_ID, "directml", "1.15.4", "windows", "x86_64", DML_WIN_X64, {
      status: "pinned", artifactFileName: "DirectML.dll", contentSha256: contentOf(DML_WIN_X64), archive: archiveRef(dmlNupkg, DML_MEMBER), hostProof: "never-run",
      licenseSpdx: "LicenseRef-Microsoft-DirectML", noticeFiles: ["third-party/DirectML-1.15.4-LICENSE.txt", "third-party/DirectML-1.15.4-ThirdPartyNotices.txt"], ...directml,
    }),
  ];
  return { ...nativeKit({ rows, archives: [ortNupkg, dmlNupkg], target }), rows, ortNupkg, dmlNupkg };
}
const ORT_WIN_ENTRY = { kind: "native-library", library: "onnxruntime", version: "1.24.4", ...NATIVE_CATALOG_ENTRY };

test("native-library stages Windows ORT with its DirectML companion: digests checked, both notice sets shipped, companionOf in the receipt", async () => {
  const kit = winOrtFixture();
  const result = await kit.stage([ORT_WIN_ENTRY]);
  assert.deepEqual(kit.fs.readFileSync(runtimeAt("onnxruntime.dll")), ORT_WIN_X64);
  assert.deepEqual(kit.fs.readFileSync(runtimeAt("DirectML.dll")), DML_WIN_X64);
  for (const name of ["ONNXRuntime-MIT.txt", "ONNXRuntime-1.24.4-ThirdPartyNotices.txt", "DirectML-1.15.4-LICENSE.txt", "DirectML-1.15.4-ThirdPartyNotices.txt"]) {
    assert.equal(kit.fs.readFileSync(licenseAt(name)).toString(), `notice third-party/${name}`, name);
  }
  assert.deepEqual(kit.fetched, ["https://api.example.test/ort.nupkg", "https://api.example.test/directml.nupkg"]);
  assert.deepEqual(kit.extracted(), [ORT_MEMBER, DML_MEMBER], "each file comes out of its archive by the row's archive.member");
  const [ort, dml] = result.files;
  assert.deepEqual([ort.kind, ort.library, ort.name, ort.catalogRow, ort.companionOf], ["native-library", "onnxruntime", "onnxruntime.dll", ORT_WIN_ID, undefined]);
  assert.deepEqual([dml.kind, dml.library, dml.name, dml.catalogRow, dml.companionOf], ["native-library", "directml", "DirectML.dll", DML_ID, ORT_WIN_ID]);
  assert.deepEqual([ort.path, dml.path], ["src-tauri/resources/runtime/onnxruntime.dll", "src-tauri/resources/runtime/DirectML.dll"]);
  assert.deepEqual([ort.sha256, dml.sha256], [sha(ORT_WIN_X64), sha(DML_WIN_X64)]);
  assert.deepEqual([ort.sizeBytes, dml.sizeBytes], [ORT_WIN_X64.length, DML_WIN_X64.length]);
  assert.deepEqual([ort.contentSha256, dml.contentSha256], [contentOf(ORT_WIN_X64), contentOf(DML_WIN_X64)]);
  assert.deepEqual([ort.archiveSha256, dml.archiveSha256], [sha(kit.ortNupkg.text), sha(kit.dmlNupkg.text)]);
  assert.deepEqual([ort.license, dml.license], ["MIT", "LicenseRef-Microsoft-DirectML"]);
  assert.deepEqual(ort.notices.map((notice) => notice.name), ["ONNXRuntime-MIT.txt", "ONNXRuntime-1.24.4-ThirdPartyNotices.txt"]);
  assert.deepEqual(dml.notices.map((notice) => notice.name), ["DirectML-1.15.4-LICENSE.txt", "DirectML-1.15.4-ThirdPartyNotices.txt"]);
  assert.deepEqual([ort.externalBin, dml.externalBin], [false, false]);
  assert.deepEqual(result.externalBinNames, []);
  assert.equal(validateStageSidecarReceipt(result.receipt, { target: WIN }), result.receipt);
});

test("native-library companions: false stages only the library; the alias on Windows now stages the companion too", async () => {
  const solo = winOrtFixture();
  const soloResult = await solo.stage([{ ...ORT_WIN_ENTRY, companions: false }]);
  assert.deepEqual(soloResult.files.map((file) => file.name), ["onnxruntime.dll"]);
  assert.equal(solo.fs.existsSync(runtimeAt("DirectML.dll")), false);
  assert.deepEqual(solo.fetched, ["https://api.example.test/ort.nupkg"], "the companion archive is not even downloaded");
  assert.equal(solo.fs.existsSync(licenseAt("DirectML-1.15.4-LICENSE.txt")), false);

  const alias = winOrtFixture();
  const aliasResult = await alias.stage([{ kind: "onnxruntime", version: "1.24.4", ...NATIVE_CATALOG_ENTRY }]);
  assert.deepEqual(aliasResult.files.map((file) => [file.kind, file.name, file.companionOf]), [["onnxruntime", "onnxruntime.dll", undefined], ["onnxruntime", "DirectML.dll", ORT_WIN_ID]]);
  assert.deepEqual(alias.fs.readFileSync(runtimeAt("DirectML.dll")), DML_WIN_X64);
  assert.ok(alias.fs.existsSync(licenseAt("DirectML-1.15.4-LICENSE.txt")) && alias.fs.existsSync(licenseAt("DirectML-1.15.4-ThirdPartyNotices.txt")), "the DirectML licence and notices ship");
  for (const file of aliasResult.files) assert.equal("library" in file, false, "the alias keeps its own row shape");
  assert.equal(aliasResult.files[1].hostProof, "never-run");
  const aliasOff = winOrtFixture();
  assert.deepEqual((await aliasOff.stage([{ kind: "onnxruntime", version: "1.24.4", companions: false, ...NATIVE_CATALOG_ENTRY }])).files.map((file) => file.name), ["onnxruntime.dll"]);
  // `as` renames the library; a companion keeps the name it is loaded by.
  const renamed = winOrtFixture();
  assert.deepEqual((await renamed.stage([{ ...ORT_WIN_ENTRY, as: "ort.dll" }])).files.map((file) => file.name), ["ort.dll", "DirectML.dll"]);
});

test("native-library refuses an unknown, planned, mismatched or notice-less companion, and writes nothing", async () => {
  const cases = [
    [{ ort: { companions: ["no-such-row"] } }, "STAGE_SIDECAR_NOT_APPROVED", /companion no-such-row of onnxruntime-1\.24\.4-windows-x64-directml.*matches 0/],
    [{ directml: { status: "planned" } }, "STAGE_SIDECAR_NOT_APPROVED", /companion directml-1\.15\.4-windows-x64 .*is planned, not approved or pinned/],
    [{ directml: { status: "stale-unverified" } }, "STAGE_SIDECAR_NOT_APPROVED", /stale-unverified/],
    [{ directml: { arch: "aarch64" } }, "STAGE_SIDECAR_NOT_APPROVED", /windows\/aarch64 row, it does not serve windows\/x86_64/],
    [{ directml: { os: "macos" } }, "STAGE_SIDECAR_NOT_APPROVED", /macos\/x86_64 row, it does not serve windows/],
    [{ directml: { companions: [ORT_WIN_ID] } }, "STAGE_SIDECAR_NOT_APPROVED", /lists companions of its own/],
    [{ directml: { artifactFileName: "onnxruntime.dll" } }, "STAGE_SIDECAR_NOT_APPROVED", /rows onnxruntime-1\.24\.4-windows-x64-directml and directml-1\.15\.4-windows-x64 both stage onnxruntime\.dll/],
    [{ directml: { artifactFileName: "OnnxRuntime.DLL" } }, "STAGE_SIDECAR_NOT_APPROVED", /both stage/],
    [{ directml: { noticeFiles: [] } }, "STAGE_SIDECAR_NOTICE_MISSING", /directml-1\.15\.4-windows-x64 lists no notice files/],
    [{ directml: { noticeFiles: ["third-party/missing-notice.txt"] } }, "STAGE_SIDECAR_NOTICE_MISSING", /missing-notice\.txt/],
  ];
  for (const [fixtureOptions, code, message] of cases) {
    const kit = winOrtFixture(fixtureOptions);
    kit.files.delete(path.join(NATIVE_CRATE, "third-party", "missing-notice.txt"));
    await assert.rejects(kit.stage([ORT_WIN_ENTRY]), { code, message }, JSON.stringify(fixtureOptions));
    assert.deepEqual(kit.written(), [], `nothing staged, not even the library whose own checks passed: ${JSON.stringify(fixtureOptions)}`);
  }
  const twice = winOrtFixture();
  twice.rows.push({ ...twice.rows[1] });
  await assert.rejects(twice.stage([ORT_WIN_ENTRY]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /companion directml-1\.15\.4-windows-x64 of .* matches 2 native catalog rows/ });
  // A companion failure found after the library was downloaded still leaves no staged file behind.
  const late = winOrtFixture({ directml: { noticeFiles: ["third-party/missing-notice.txt"] } });
  late.files.delete(path.join(NATIVE_CRATE, "third-party", "missing-notice.txt"));
  await assert.rejects(late.stage([ORT_WIN_ENTRY]), { code: "STAGE_SIDECAR_NOTICE_MISSING" });
  assert.equal(late.fs.existsSync(runtimeAt("onnxruntime.dll")), false);
  // companions: false never looks at the companion at all.
  const skipped = winOrtFixture({ directml: { status: "planned" } });
  assert.deepEqual((await skipped.stage([{ ...ORT_WIN_ENTRY, companions: false }])).files.map((file) => file.name), ["onnxruntime.dll"]);
});

test("archive.member picks the one member among same-named ones for ORT, DirectML and PDFium; without it the bare name is ambiguous", async () => {
  const pdfiumTgz = archiveOf("https://example.test/pdfium-win-x64.tgz", { "bin/pdfium.dll": peOf(0x8664, 0x55), "symbols/bin/pdfium.dll": peOf(0x8664, 0x56), "lib/pdfium.dll.lib": Buffer.from("import-lib") });
  const pdfium = rowOf("pdfium-chromium-8021-windows-x64", "pdfium", "chromium/8021", "windows", "x86_64", pdfiumTgz.members["bin/pdfium.dll"], {
    status: "pinned", artifactFileName: "pdfium.dll", contentSha256: contentOf(pdfiumTgz.members["bin/pdfium.dll"]), archive: archiveRef(pdfiumTgz, "bin/pdfium.dll"), hostProof: "never-run", licenseSpdx: "BSD-3-Clause",
  });
  const cases = [["onnxruntime", "1.24.4", "onnxruntime.dll", ORT_WIN_X64, ORT_MEMBER], ["directml", "1.15.4", "DirectML.dll", DML_WIN_X64, DML_MEMBER], ["pdfium", "chromium/8021", "pdfium.dll", pdfiumTgz.members["bin/pdfium.dll"], "bin/pdfium.dll"]];
  for (const [library, version, name, bytes, member] of cases) {
    const win = winOrtFixture();
    const kit = nativeKit({ rows: [...win.rows, pdfium], archives: [win.ortNupkg, win.dmlNupkg, pdfiumTgz] });
    const result = await kit.stage([{ kind: "native-library", library, version, companions: false, ...NATIVE_CATALOG_ENTRY }]);
    assert.deepEqual(kit.extracted(), [member], library);
    assert.deepEqual(kit.fs.readFileSync(runtimeAt(name)), bytes, `${library}: the file at archive.member, not the same-named decoy`);
    assert.equal(result.files[0].name, name, "written under the artifactFileName, not the member path");
    const bare = nativeKit({ rows: [...win.rows, pdfium].map((row) => (row.library === library ? { ...row, archive: { url: row.archive.url, sha256: row.archive.sha256 } } : row)), archives: [win.ortNupkg, win.dmlNupkg, pdfiumTgz] });
    await assert.rejects(bare.stage([{ kind: "native-library", library, version, companions: false, ...NATIVE_CATALOG_ENTRY }]), { code: "STAGE_SIDECAR_EXTRACT_FAILED", message: /members named/ }, `${library} without archive.member`);
  }
});

test("native-library stages PDFium: the pinned Windows row, and the macOS arm64 and x64 rows each from their own archive", async () => {
  const dll = peOf(0x8664, 0x55);
  const winTgz = archiveOf("https://example.test/chromium%2F8021/pdfium-win-x64.tgz", { "bin/pdfium.dll": dll });
  const armTgz = archiveOf("https://example.test/chromium%2F8021/pdfium-mac-arm64.tgz", { "lib/libpdfium.dylib": Buffer.from("pdfium-arm64") });
  const x64Tgz = archiveOf("https://example.test/chromium%2F8021/pdfium-mac-x64.tgz", { "lib/libpdfium.dylib": Buffer.from("pdfium-x64") });
  const rows = [
    rowOf("pdfium-chromium-8021-macos-arm64", "pdfium", "chromium/8021", "macos", "aarch64", Buffer.from("pdfium-arm64"), { status: "pinned", artifactFileName: "libpdfium.dylib", archive: archiveRef(armTgz, "lib/libpdfium.dylib"), licenseSpdx: "BSD-3-Clause" }),
    rowOf("pdfium-chromium-8021-macos-x86_64", "pdfium", "chromium/8021", "macos", "x86_64", Buffer.from("pdfium-x64"), { status: "pinned", artifactFileName: "libpdfium.dylib", archive: archiveRef(x64Tgz, "lib/libpdfium.dylib"), licenseSpdx: "BSD-3-Clause" }),
    rowOf("pdfium-chromium-8021-windows-x64", "pdfium", "chromium/8021", "windows", "x86_64", dll, { status: "pinned", artifactFileName: "pdfium.dll", contentSha256: contentOf(dll), archive: archiveRef(winTgz, "bin/pdfium.dll"), hostProof: "never-run", licenseSpdx: "BSD-3-Clause" }),
  ];
  const entry = { kind: "native-library", library: "pdfium", version: "chromium/8021", ...NATIVE_CATALOG_ENTRY };
  const archives = [winTgz, armTgz, x64Tgz];
  const win = nativeKit({ rows, archives, target: WIN });
  const winResult = await win.stage([entry]);
  assert.deepEqual(win.fs.readFileSync(runtimeAt("pdfium.dll")), dll);
  assert.deepEqual([winResult.files[0].library, winResult.files[0].catalogRow, winResult.files[0].hostProof, winResult.files[0].version], ["pdfium", "pdfium-chromium-8021-windows-x64", "never-run", "chromium/8021"]);
  assert.ok(win.fs.existsSync(at(".cache", "native-library", "pdfium-win-x64.tgz")), "the archive is cached under the neutral default cacheDir, named after the URL's last segment");
  for (const [target, url, bytes, row] of [[MAC, armTgz.url, "pdfium-arm64", "pdfium-chromium-8021-macos-arm64"], ["x86_64-apple-darwin", x64Tgz.url, "pdfium-x64", "pdfium-chromium-8021-macos-x86_64"]]) {
    const kit = nativeKit({ rows, archives, target });
    const result = await kit.stage([entry]);
    assert.deepEqual(kit.fetched, [url], target);
    assert.equal(kit.fs.readFileSync(runtimeAt("libpdfium.dylib")).toString(), bytes);
    assert.equal(result.files[0].catalogRow, row);
    assert.equal("hostProof" in result.files[0], false, "a row that was run carries no hostProof");
    assert.equal("contentSha256" in result.files[0], false, "the macOS rows pin the whole-file sha256 only");
  }
  const planned = nativeKit({ rows: rows.map((row) => (row.os === "windows" ? { ...row, status: "planned" } : row)), archives, target: WIN });
  await assert.rejects(planned.stage([entry]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /pdfium-chromium-8021-windows-x64 is planned, not approved or pinned/ });
  assert.deepEqual([planned.fetched, planned.written()], [[], []]);
});

/** The macOS arm64 whisper.cpp and ggml set (no archive: built and retained), each file a thin Mach-O with its own payload. */
const GGML_FILES = ["libggml.0.20.2.dylib", "libggml-base.0.20.2.dylib", "libggml-cpu.0.20.2.dylib", "libggml-metal.0.20.2.dylib", "libggml-blas.0.20.2.dylib"];
const SOURCE_DIR = "../retained/builds";
const sourceAt = (name) => path.resolve(APP, SOURCE_DIR, name);
const bytesOfLibrary = (name) => machoCpu(CPU_ARM64, Buffer.from(`payload of ${name}`), null);
function whisperGgmlFixture({ target = MAC, seeded = {}, rows: rowsOver } = {}) {
  const mac = (id, library, version, name, extra = {}) => rowOf(id, library, version, "macos", "aarch64", bytesOfLibrary(name), { artifactFileName: name, contentSha256: contentOf(bytesOfLibrary(name)), noticeFiles: [`third-party/${library}-MIT.txt`], ...extra });
  const rows = rowsOver ?? [
    mac("whisper-cpp-1.9.3-macos-arm64", "whisper-cpp", "1.9.3", "libwhisper.1.9.3.dylib"),
    ...GGML_FILES.map((name) => mac(`${name.replace(/^lib|\.0\.20\.2\.dylib$/g, "")}-0.20.2-macos-arm64`, "ggml", "0.20.2", name)),
  ];
  const sources = Object.fromEntries(["libwhisper.1.9.3.dylib", ...GGML_FILES].map((name) => [sourceAt(name), bytesOfLibrary(name)]));
  return { ...nativeKit({ rows, files: { ...sources, ...seeded }, target }), rows };
}
const GGML_ENTRY = { kind: "native-library", library: "ggml", version: "0.20.2", source: { dir: SOURCE_DIR }, ...NATIVE_CATALOG_ENTRY };
const WHISPER_ENTRY = { kind: "native-library", library: "whisper-cpp", version: "1.9.3", source: { dir: SOURCE_DIR }, ...NATIVE_CATALOG_ENTRY };

test("native-library stages a multi-file library (ggml) and whisper.cpp from source.dir: every row, no download, digests in the receipt", async () => {
  const kit = whisperGgmlFixture();
  const result = await kit.stage([GGML_ENTRY, WHISPER_ENTRY]);
  assert.deepEqual(result.files.map((file) => file.name), [...GGML_FILES, "libwhisper.1.9.3.dylib"]);
  for (const file of result.files) {
    assert.deepEqual(kit.fs.readFileSync(at(...file.path.split("/"))), bytesOfLibrary(file.name), file.name);
    assert.deepEqual([file.kind, file.sha256, file.contentSha256, file.externalBin], ["native-library", sha(bytesOfLibrary(file.name)), contentOf(bytesOfLibrary(file.name)), false]);
    assert.equal("archiveSha256" in file, false, "no archive was used");
    assert.equal(file.path, `src-tauri/resources/runtime/${file.name}`);
  }
  assert.deepEqual(result.files.map((file) => file.library), [...GGML_FILES.map(() => "ggml"), "whisper-cpp"]);
  assert.deepEqual(kit.fetched, []);
  assert.ok(kit.fs.existsSync(licenseAt("ggml-MIT.txt")) && kit.fs.existsSync(licenseAt("whisper-cpp-MIT.txt")));
  assert.equal(kit.calls.filter((call) => call[0] === "tar").length, 0);
});

test("native-library from source.dir fails closed: a tampered file, a missing file and a row with no source write nothing", async () => {
  const flip = (name) => { const bad = Buffer.from(bytesOfLibrary(name)); bad[0x1000] ^= 1; return bad; };
  const tampered = whisperGgmlFixture({ seeded: { [sourceAt("libggml-metal.0.20.2.dylib")]: flip("libggml-metal.0.20.2.dylib") } });
  await assert.rejects(tampered.stage([GGML_ENTRY]), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === contentOf(bytesOfLibrary("libggml-metal.0.20.2.dylib")) && error.details.actual === contentOf(flip("libggml-metal.0.20.2.dylib")) && /libggml-metal\.0\.20\.2\.dylib.*ggml-metal-0\.20\.2-macos-arm64/.test(error.message));
  assert.deepEqual(tampered.written(), [], "the four files that verified are not staged either");

  const wholeFileOnly = whisperGgmlFixture({ rows: [rowOf("sherpa-row", "sherpa-onnx", "1.13.4", "macos", "aarch64", Buffer.from("sherpa"), { artifactFileName: "libsherpa-onnx-c-api.dylib" })], seeded: { [sourceAt("libsherpa-onnx-c-api.dylib")]: Buffer.from("sherpa-tampered") } });
  await assert.rejects(wholeFileOnly.stage([{ ...GGML_ENTRY, library: "sherpa-onnx", version: "1.13.4" }]), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === sha("sherpa") && error.details.actual === sha("sherpa-tampered"), "a row with only a whole-file sha256 is held to it");
  assert.deepEqual(wholeFileOnly.written(), []);

  const missing = whisperGgmlFixture();
  missing.files.delete(sourceAt("libggml-cpu.0.20.2.dylib"));
  await assert.rejects(missing.stage([GGML_ENTRY]), { code: "STAGE_SIDECAR_BINARY_NOT_FOUND", message: /source\.dir \.\.\/retained\/builds, which has no libggml-cpu\.0\.20\.2\.dylib/ });
  assert.deepEqual(missing.written(), []);

  const noSource = whisperGgmlFixture();
  const { source: _source, ...withoutSource } = WHISPER_ENTRY;
  await assert.rejects(noSource.stage([withoutSource]), { code: "STAGE_SIDECAR_CONFIG_INVALID", message: /whisper-cpp-1\.9\.3-macos-arm64 has no archive.*source: \{ dir \}/ });
  assert.deepEqual([noSource.written(), noSource.fetched], [[], []]);
  // The alias has no source key: a row without an archive is simply not approvable for it.
  const alias = whisperGgmlFixture({ rows: [rowOf("ort-no-archive", "onnxruntime", "1.24.4", "macos", "aarch64", Buffer.from("x"), { artifactFileName: "libonnxruntime.dylib" })] });
  await assert.rejects(alias.stage([{ kind: "onnxruntime", version: "1.24.4", ...NATIVE_CATALOG_ENTRY }]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /has no valid archive/ });
});

test("native-library files restricts a multi-file library; each name must match exactly one serving row", async () => {
  const subset = whisperGgmlFixture();
  const result = await subset.stage([{ ...GGML_ENTRY, files: ["libggml.0.20.2.dylib", "libggml-base.0.20.2.dylib"] }]);
  assert.deepEqual(result.files.map((file) => file.name), ["libggml.0.20.2.dylib", "libggml-base.0.20.2.dylib"]);
  assert.deepEqual(subset.written().filter((file) => file.endsWith(".dylib")), ["src-tauri/resources/runtime/libggml-base.0.20.2.dylib", "src-tauri/resources/runtime/libggml.0.20.2.dylib"]);
  const order = whisperGgmlFixture();
  assert.deepEqual((await order.stage([{ ...GGML_ENTRY, files: ["libggml-base.0.20.2.dylib", "libggml.0.20.2.dylib"] }])).files.map((file) => file.name), ["libggml-base.0.20.2.dylib", "libggml.0.20.2.dylib"], "the order of files is the order staged");

  const unknown = whisperGgmlFixture();
  await assert.rejects(unknown.stage([{ ...GGML_ENTRY, files: ["libggml.0.20.2.dylib", "libggml-vulkan.dylib"] }]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /no ggml 0\.20\.2 macos\/aarch64 row for libggml-vulkan\.dylib/ });
  assert.deepEqual(unknown.written(), []);
  const planned = whisperGgmlFixture();
  planned.rows.find((row) => row.artifactFileName === "libggml-cpu.0.20.2.dylib").status = "planned";
  await assert.rejects(planned.stage([{ ...GGML_ENTRY, files: ["libggml-cpu.0.20.2.dylib"] }]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /row ggml-cpu-0\.20\.2-macos-arm64 for libggml-cpu\.0\.20\.2\.dylib is planned, not approved or pinned/ });
  // Without files a planned row is not staged (and is said so); the serving rows still are.
  const partial = whisperGgmlFixture();
  const last = partial.rows.find((row) => row.artifactFileName === "libggml-blas.0.20.2.dylib");
  last.status = "planned";
  assert.equal((await partial.stage([GGML_ENTRY])).files.length, 4);
  assert.ok(partial.messages.some((line) => /not staging ggml-blas-0\.20\.2-macos-arm64 \(planned\)/.test(line)), partial.messages.join("\n"));
  const dup = whisperGgmlFixture();
  dup.rows.push({ ...dup.rows.find((row) => row.artifactFileName === "libggml.0.20.2.dylib"), id: "ggml-again" });
  await assert.rejects(dup.stage([GGML_ENTRY]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /2 ggml 0\.20\.2 macos\/aarch64 rows for libggml\.0\.20\.2\.dylib/ });
});

test("native-library as renames the single staged file and is refused, typed, when more than one file would be staged", async () => {
  const many = whisperGgmlFixture();
  await assert.rejects(many.stage([{ ...GGML_ENTRY, as: "libggml.dylib" }]), { code: "STAGE_SIDECAR_CONFIG_INVALID", message: /as "libggml\.dylib" renames one file, but the ggml 0\.20\.2 macos\/aarch64 rows stage 5/ });
  assert.deepEqual(many.written(), []);
  const one = whisperGgmlFixture();
  const result = await one.stage([{ ...WHISPER_ENTRY, as: "libwhisper.dylib" }]);
  assert.equal(result.files[0].name, "libwhisper.dylib");
  assert.deepEqual(one.fs.readFileSync(runtimeAt("libwhisper.dylib")), bytesOfLibrary("libwhisper.1.9.3.dylib"));
  assert.equal(one.fs.existsSync(runtimeAt("libwhisper.1.9.3.dylib")), false);
  const subsetOfOne = whisperGgmlFixture();
  assert.equal((await subsetOfOne.stage([{ ...GGML_ENTRY, files: ["libggml.0.20.2.dylib"], as: "libggml.dylib" }])).files[0].name, "libggml.dylib");
  assert.throws(() => validateSidecarManifest([{ ...GGML_ENTRY, files: ["libggml.0.20.2.dylib", "libggml-base.0.20.2.dylib"], as: "x.dylib" }], { target: MAC }), { code: "STAGE_SIDECAR_CONFIG_INVALID", message: /as renames one file, so files must list exactly one \(it lists 2\)/ });
});

test("native-library from source.dir accepts a re-signed copy by its content digest, and refuses changed code or a malformed file", async () => {
  const payload = Buffer.from("symtab-and-strtab-bytes!");
  const unsigned = machoCpu(CPU_ARM64, payload, null);
  const signed = machoCpu(CPU_ARM64, payload, Buffer.alloc(300, 0x11));
  assert.equal(contentOf(signed), contentOf(unsigned), "signed and unsigned share one content digest");
  assert.notEqual(sha(signed), sha(unsigned), "while the whole-file sha256 differs");
  const row = rowOf("whisper-cpp-1.9.3-macos-arm64", "whisper-cpp", "1.9.3", "macos", "aarch64", unsigned, { artifactFileName: "libwhisper.1.9.3.dylib", contentSha256: contentOf(unsigned) });
  const attempt = (bytes, over = {}) => { const kit = nativeKit({ rows: [{ ...row, ...over }], files: { [sourceAt("libwhisper.1.9.3.dylib")]: bytes }, target: MAC }); return { kit, run: kit.stage([WHISPER_ENTRY]) }; };

  const ok = attempt(signed);
  const [file] = (await ok.run).files;
  assert.deepEqual(ok.kit.fs.readFileSync(runtimeAt("libwhisper.1.9.3.dylib")), signed, "the copy is staged as it is");
  assert.deepEqual([file.sha256, file.contentSha256], [sha(signed), contentOf(unsigned)], "the receipt sha256 is the staged file's own hash; the row's content digest rides beside it");

  const changed = Buffer.from(signed); changed[0x1000] ^= 1;
  const tampered = attempt(changed);
  await assert.rejects(tampered.run, (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === contentOf(unsigned) && error.details.actual === contentOf(changed) && /content digest/.test(error.message));
  assert.deepEqual(tampered.kit.written(), []);

  const malformed = attempt(signed.subarray(0, 100));
  await assert.rejects(malformed.run, (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.actual === null && /unreadable \(Mach-O/.test(error.message), "a malformed Mach-O is a typed refusal, never a plain-hash fallback");
  assert.deepEqual(malformed.kit.written(), []);

  // With only a whole-file sha256 pinned, a re-signed copy is not the pinned file.
  await assert.rejects(attempt(signed, { contentSha256: undefined }).run, (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === sha(unsigned) && error.details.actual === sha(signed));
  assert.deepEqual((await attempt(unsigned, { contentSha256: undefined }).run).files[0].sha256, sha(unsigned));
  // A row that pins no digest never serves, from a source dir or an archive.
  await assert.rejects(attempt(unsigned, { sha256: undefined, contentSha256: undefined }).run, { code: "STAGE_SIDECAR_NOT_APPROVED", message: /pins no digest.*never serves/ });
  await assert.rejects(attempt(unsigned, { contentSha256: "xyz" }).run, { code: "STAGE_SIDECAR_NOT_APPROVED", message: /contentSha256 is not a lowercase 64-hex/ });
});

test("native-library holds an archive member to the whole-file sha256 AND the content digest", async () => {
  const unsigned = peOf(0x8664, 0x66);
  const signed = peOf(0x8664, 0x66, Buffer.alloc(64, 7));
  assert.equal(contentOf(signed), contentOf(unsigned));
  const tgz = archiveOf("https://example.test/pdfium-win-x64.tgz", { "bin/pdfium.dll": signed });
  const pdfium = (over = {}) => rowOf("pdfium-chromium-8021-windows-x64", "pdfium", "chromium/8021", "windows", "x86_64", signed, { status: "pinned", artifactFileName: "pdfium.dll", contentSha256: contentOf(unsigned), archive: archiveRef(tgz, "bin/pdfium.dll"), ...over });
  const attempt = (over) => { const kit = nativeKit({ rows: [pdfium(over)], archives: [tgz] }); return { kit, run: kit.stage([{ kind: "native-library", library: "pdfium", version: "chromium/8021", ...NATIVE_CATALOG_ENTRY }]) }; };
  const ok = attempt({});
  assert.deepEqual([(await ok.run).files[0].sha256, ok.kit.fs.readFileSync(runtimeAt("pdfium.dll")).equals(signed)], [sha(signed), true]);
  const wrongContent = attempt({ contentSha256: sha("another build") });
  await assert.rejects(wrongContent.run, (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === sha("another build") && error.details.actual === contentOf(unsigned) && /content digest/.test(error.message));
  assert.deepEqual(wrongContent.kit.written(), []);
  const wrongWhole = attempt({ sha256: sha("another file") });
  await assert.rejects(wrongWhole.run, (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === sha("another file") && error.details.actual === sha(signed) && /sha256/.test(error.message));
  assert.deepEqual(wrongWhole.kit.written(), []);
  const contentOnly = attempt({ sha256: undefined });
  assert.equal((await contentOnly.run).files[0].name, "pdfium.dll");
  await assert.rejects(attempt({ archive: { ...archiveRef(tgz, "bin/pdfium.dll"), sha256: sha("other archive") } }).run, { code: "STAGE_SIDECAR_HASH_MISMATCH", message: /downloaded as/ });
});

test("native-library: the staged-file shortcut applies the same check, a file is never trusted because it exists", async () => {
  const kitFor = (staged) => whisperGgmlFixture({ seeded: staged === undefined ? {} : { [runtimeAt("libwhisper.1.9.3.dylib")]: staged } });
  const wrong = kitFor(Buffer.from("something else that is staged"));
  await wrong.stage([WHISPER_ENTRY]);
  assert.deepEqual(wrong.fs.readFileSync(runtimeAt("libwhisper.1.9.3.dylib")), bytesOfLibrary("libwhisper.1.9.3.dylib"), "a staged file that fails the row's digest is replaced from the verified source");
  assert.ok(wrong.log.copies.some((copy) => copy.startsWith(runtimeAt("libwhisper.1.9.3.dylib"))), "copied through a temporary file beside the destination");
  const right = kitFor(bytesOfLibrary("libwhisper.1.9.3.dylib"));
  const result = await right.stage([WHISPER_ENTRY]);
  assert.equal(right.log.copies.some((copy) => copy.startsWith(runtimeAt("libwhisper.1.9.3.dylib"))), false, "a verified staged file is not rewritten");
  assert.ok(right.messages.some((line) => /libwhisper\.1\.9\.3\.dylib already staged/.test(line)));
  assert.equal(result.files[0].sha256, sha(bytesOfLibrary("libwhisper.1.9.3.dylib")));
  // A bad staged file with a tampered source is refused: the wrong bytes are not left to ship under the receipt.
  const flipped = Buffer.from(bytesOfLibrary("libwhisper.1.9.3.dylib")); flipped[0x1000] ^= 1;
  const both = whisperGgmlFixture({ seeded: { [runtimeAt("libwhisper.1.9.3.dylib")]: flipped, [sourceAt("libwhisper.1.9.3.dylib")]: flipped } });
  await assert.rejects(both.stage([WHISPER_ENTRY]), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  // The same holds for an archive row: a wrong staged file is re-extracted, not trusted.
  const kit = winOrtFixture();
  kit.files.set(runtimeAt("onnxruntime.dll"), Buffer.from("not the pinned dll"));
  await kit.stage([{ ...ORT_WIN_ENTRY, companions: false }]);
  assert.deepEqual(kit.fs.readFileSync(runtimeAt("onnxruntime.dll")), ORT_WIN_X64);
  assert.deepEqual(kit.fetched, ["https://api.example.test/ort.nupkg"]);
  const warm = winOrtFixture();
  warm.files.set(runtimeAt("onnxruntime.dll"), ORT_WIN_X64);
  await warm.stage([{ ...ORT_WIN_ENTRY, companions: false }]);
  assert.deepEqual([warm.fetched, warm.extracted()], [[], []], "a verified staged file needs no download");
});

/** Universal ORT 1.30.0 shaped row: a fat Mach-O in a .zip, pinned by contentSha256ByArch. */
const ORT_ARM_THIN = machoCpu(CPU_ARM64, Buffer.from("arm64 slice payload"), null);
const ORT_X86_THIN = machoCpu(CPU_X86_64, Buffer.from("x86_64 slice payload"), null);
const ORT_FAT = fatOf([[CPU_ARM64, ORT_ARM_THIN], [CPU_X86_64, ORT_X86_THIN]]);
const ORT_BY_ARCH = { aarch64: contentOf(ORT_ARM_THIN, "aarch64"), x86_64: contentOf(ORT_X86_THIN, "x86_64") };
const ORT_ZIP_ENTRY_MEMBER = "onnxruntime-osx-universal2-1.30.0/lib/libonnxruntime.dylib";
const ORT_UNIVERSAL_ENTRY = { kind: "native-library", library: "onnxruntime", version: "1.30.0", ...NATIVE_CATALOG_ENTRY };
function universalOrtFixture({ target = MAC, row: over = {}, bytes = ORT_FAT, lipoArchs } = {}) {
  const zip = archiveOf("https://example.test/onnxruntime-osx-universal2-1.30.0.zip", { [ORT_ZIP_ENTRY_MEMBER]: bytes });
  const row = rowOf("onnxruntime-1.30.0-macos-universal2", "onnxruntime", "1.30.0", "macos", "universal", bytes, { artifactFileName: "libonnxruntime.dylib", contentSha256ByArch: ORT_BY_ARCH, archive: archiveRef(zip), noticeFiles: ["third-party/ONNXRuntime-MIT.txt"], ...over });
  return { ...nativeKit({ rows: [row], archives: [zip], target, ...(lipoArchs ? { lipoArchs } : {}) }), row };
}

test("native-library verifies contentSha256ByArch slice by slice for a universal row, from an archive and from source.dir", async () => {
  const ok = universalOrtFixture();
  const [file] = (await ok.stage([ORT_UNIVERSAL_ENTRY])).files;
  assert.deepEqual(ok.extracted(), [ORT_ZIP_ENTRY_MEMBER], "the zip-derived member serves a native-library entry too");
  assert.deepEqual([file.sha256, file.contentSha256ByArch, "contentSha256" in file, file.catalogRow], [sha(ORT_FAT), ORT_BY_ARCH, false, "onnxruntime-1.30.0-macos-universal2"]);
  assert.equal(ok.calls.filter((call) => call[0] === "lipo").length, 0, "a thin target never runs lipo");
  for (const [arch, wrong] of [["aarch64", { ...ORT_BY_ARCH, aarch64: sha("wrong slice") }], ["x86_64", { ...ORT_BY_ARCH, x86_64: sha("wrong slice") }]]) {
    const bad = universalOrtFixture({ row: { contentSha256ByArch: wrong } });
    await assert.rejects(bad.stage([ORT_UNIVERSAL_ENTRY]), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === sha("wrong slice") && error.details.actual === ORT_BY_ARCH[arch] && error.message.includes(`${arch} content digest`), arch);
    assert.deepEqual(bad.written(), []);
  }
  const thin = universalOrtFixture({ bytes: ORT_ARM_THIN });
  await assert.rejects(thin.stage([ORT_UNIVERSAL_ENTRY]), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.actual === null && /x86_64 content digest.*no matching slice/.test(error.message), "a thin file has no x86_64 slice to match");

  // A copy whose arm64 slice was re-signed has another whole-file hash and the same slice digests.
  const resigned = fatOf([[CPU_ARM64, machoCpu(CPU_ARM64, Buffer.from("arm64 slice payload"), Buffer.alloc(200, 9))], [CPU_X86_64, ORT_X86_THIN]]);
  assert.notEqual(sha(resigned), sha(ORT_FAT));
  const { archive: _archive, ...noArchive } = ok.row;
  const local = nativeKit({ rows: [noArchive], files: { [sourceAt("libonnxruntime.dylib")]: resigned }, target: MAC });
  const localResult = await local.stage([{ ...ORT_UNIVERSAL_ENTRY, source: { dir: SOURCE_DIR } }]);
  assert.deepEqual([localResult.files[0].sha256, localResult.files[0].contentSha256ByArch], [sha(resigned), ORT_BY_ARCH]);
  const otherSlice = fatOf([[CPU_ARM64, ORT_ARM_THIN], [CPU_X86_64, machoCpu(CPU_X86_64, Buffer.from("different x86_64 payload"), null)]]);
  const badLocal = nativeKit({ rows: [noArchive], files: { [sourceAt("libonnxruntime.dylib")]: otherSlice }, target: MAC });
  await assert.rejects(badLocal.stage([{ ...ORT_UNIVERSAL_ENTRY, source: { dir: SOURCE_DIR } }]), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === ORT_BY_ARCH.x86_64);
  assert.deepEqual(badLocal.written(), []);
});

test("universal-apple-darwin stages a universal native-library row once, checks it with lipo -archs, and writes nothing for a thin file", async () => {
  const ok = universalOrtFixture({ target: UNIVERSAL });
  const [file] = (await ok.stage([ORT_UNIVERSAL_ENTRY])).files;
  assert.deepEqual(ok.calls.filter((call) => call[0] === "lipo").map((call) => call[1]), ["-archs"]);
  assert.deepEqual(ok.fs.readFileSync(runtimeAt("libonnxruntime.dylib")), ORT_FAT);
  assert.deepEqual(file.slices.map((slice) => [slice.triple, slice.catalogRow, slice.contentSha256]), [["aarch64-apple-darwin", file.catalogRow, ORT_BY_ARCH.aarch64], ["x86_64-apple-darwin", file.catalogRow, ORT_BY_ARCH.x86_64]]);
  assert.deepEqual([file.sourceSha256, file.library, file.archiveSha256], [sha(ORT_FAT), "onnxruntime", ok.row.archive.sha256]);
  const thin = universalOrtFixture({ target: UNIVERSAL, lipoArchs: "arm64\n" });
  await assert.rejects(thin.stage([ORT_UNIVERSAL_ENTRY]), { code: "STAGE_SIDECAR_NOT_UNIVERSAL" });
  assert.deepEqual(thin.written(), [], "the verified file is checked with lipo before it is written");
  // sherpa-onnx: a universal row measured from a retained build (no archive) is staged once from source.dir by its whole-file sha256.
  const dylib = Buffer.from("sherpa universal dylib");
  const sherpa = rowOf("sherpa-onnx-1.13.4-macos-universal", "sherpa-onnx", "1.13.4", "macos", "universal", dylib, { artifactFileName: "libsherpa-onnx-c-api.dylib", licenseSpdx: "Apache-2.0" });
  const kit = nativeKit({ rows: [sherpa], files: { [sourceAt("libsherpa-onnx-c-api.dylib")]: dylib }, target: UNIVERSAL });
  const staged = await kit.stage([{ kind: "native-library", library: "sherpa-onnx", version: "1.13.4", source: { dir: SOURCE_DIR }, ...NATIVE_CATALOG_ENTRY }]);
  assert.deepEqual([staged.files[0].name, staged.files[0].sha256, staged.files[0].license, "archiveSha256" in staged.files[0]], ["libsherpa-onnx-c-api.dylib", sha(dylib), "Apache-2.0", false]);
  assert.deepEqual(kit.calls.filter((call) => call[0] === "lipo").map((call) => call[1]), ["-archs"]);
});

test("universal-apple-darwin refuses a library whose rows are only aarch64 (whisper.cpp, ggml) instead of guessing", async () => {
  for (const entry of [WHISPER_ENTRY, GGML_ENTRY]) {
    const kit = whisperGgmlFixture({ target: UNIVERSAL });
    await assert.rejects(kit.stage([entry]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: new RegExp(`no ${entry.library} ${entry.version.replaceAll(".", "\\.")} macos/x86_64 row`) }, entry.library);
    assert.deepEqual([kit.written(), kit.calls.filter((call) => call[0] === "lipo")], [[], []]);
  }
  const x86 = whisperGgmlFixture({ target: "x86_64-apple-darwin" });
  await assert.rejects(x86.stage([WHISPER_ENTRY]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /no whisper-cpp 1\.9\.3 macos\/x86_64 row/ });
});

test("universal-apple-darwin merges a multi-file native-library file by file, from exact-arch rows", async () => {
  const names = ["libggml.0.20.2.dylib", "libggml-base.0.20.2.dylib"];
  const slice = (arch, name) => machoCpu(arch === "aarch64" ? CPU_ARM64 : CPU_X86_64, Buffer.from(`${arch} ${name}`), null);
  const archives = Object.fromEntries(["aarch64", "x86_64"].map((arch) => [arch, archiveOf(`https://example.test/ggml-${arch}.tgz`, Object.fromEntries(names.map((name) => [`lib/${name}`, slice(arch, name)])))]));
  const rows = ["aarch64", "x86_64"].flatMap((arch) => names.map((name) => rowOf(`${name}-${arch}`, "ggml", "0.20.2", "macos", arch, slice(arch, name), { artifactFileName: name, contentSha256: contentOf(slice(arch, name)), archive: archiveRef(archives[arch], `lib/${name}`) })));
  const kit = nativeKit({ rows, archives: Object.values(archives), target: UNIVERSAL });
  const result = await kit.stage([{ kind: "native-library", library: "ggml", version: "0.20.2", ...NATIVE_CATALOG_ENTRY }]);
  assert.deepEqual(result.files.map((file) => file.name), names);
  assert.equal(kit.calls.filter((call) => call[0] === "lipo" && call[1] === "-create").length, 2, "one lipo -create per file");
  for (const [index, name] of names.entries()) {
    const file = result.files[index];
    assert.equal(kit.fs.readFileSync(runtimeAt(name)).toString(), `fat:${slice("aarch64", name)}+${slice("x86_64", name)}`);
    assert.ok(kit.fs.existsSync(at(".right-release", "slices", "aarch64-apple-darwin", name)) && kit.fs.existsSync(at(".right-release", "slices", "x86_64-apple-darwin", name)));
    assert.equal(file.catalogRow, `${name}-aarch64+${name}-x86_64`);
    assert.deepEqual(file.slices.map((part) => [part.triple, part.sha256, part.contentSha256]), [["aarch64-apple-darwin", sha(slice("aarch64", name)), contentOf(slice("aarch64", name))], ["x86_64-apple-darwin", sha(slice("x86_64", name)), contentOf(slice("x86_64", name))]]);
    assert.deepEqual([file.library, file.kind], ["ggml", "native-library"]);
  }
  // Both arches must name the same files.
  const uneven = nativeKit({ rows: rows.filter((row) => !(row.arch === "x86_64" && row.artifactFileName === names[1])), archives: Object.values(archives), target: UNIVERSAL });
  await assert.rejects(uneven.stage([{ kind: "native-library", library: "ggml", version: "0.20.2", ...NATIVE_CATALOG_ENTRY }]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /arch rows name different library files/ });
});

test("hostProof never-run: the row stages with a warning that names it, the receipt carries it, and a contradictory hostProof is refused", async () => {
  const kit = winOrtFixture();
  const result = await kit.stage([ORT_WIN_ENTRY]);
  const warnings = kit.messages.filter((line) => /warning/.test(line));
  assert.equal(warnings.length, 2);
  assert.ok(warnings[0].includes(ORT_WIN_ID) && warnings[0].includes("hostProof never-run") && warnings[0].includes("onnxruntime.dll"), warnings[0]);
  assert.ok(warnings[1].includes(DML_ID) && warnings[1].includes("DirectML.dll"), warnings[1]);
  assert.deepEqual(result.files.map((file) => file.hostProof), ["never-run", "never-run"]);
  assert.equal(JSON.parse(kit.fs.readFileSync(at("src-tauri", "binaries", "stage-sidecar.json")).toString()).files[0].hostProof, "never-run", "the receipt on disk carries it");
  const proven = whisperGgmlFixture();
  const provenResult = await proven.stage([WHISPER_ENTRY]);
  assert.deepEqual([proven.messages.filter((line) => /warning/.test(line)), "hostProof" in provenResult.files[0]], [[], false]);
  const approved = winOrtFixture({ ort: { status: "approved" } });
  await assert.rejects(approved.stage([ORT_WIN_ENTRY]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /carries hostProof "never-run".*never on an approved row/ });
  assert.deepEqual(approved.written(), []);
  await assert.rejects(winOrtFixture({ ort: { hostProof: "proven" } }).stage([ORT_WIN_ENTRY]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: /carries hostProof "proven"/ });
  for (const bad of [{ kind: "native-library", library: "x", name: "a", path: "a", sha256: sha("a"), sizeBytes: 1, hostProof: "proven" }, { kind: "onnxruntime", name: "a", path: "a", sha256: sha("a"), sizeBytes: 1, companionOf: "" }, { kind: "native-library", name: "a", path: "a", sha256: sha("a"), sizeBytes: 1 }]) {
    assert.throws(() => validateStageSidecarReceipt({ schema: 1, contract: "rightkit-stage-sidecar-v1", target: MAC, files: [bad] }), { code: "STAGE_SIDECAR_RECEIPT_INVALID" });
  }
});

test("native-library configuration is validated: library, version token, files, source.dir, companions, and the keys the alias keeps", () => {
  const base = { kind: "native-library", library: "pdfium", version: "chromium/8021", catalog: "c.json" };
  const errorsOf = (...items) => { try { validateSidecarManifest(items, { target: MAC }); } catch (error) { assert.equal(error.code, "STAGE_SIDECAR_CONFIG_INVALID"); return error.details.errors; } return []; };
  const refuses = (over, needle) => assert.ok(errorsOf({ ...base, ...over }).some((line) => needle.test(line)), `${JSON.stringify(over)} -> ${errorsOf({ ...base, ...over }).join(" | ")}`);
  refuses({ library: undefined }, /library is required/);
  refuses({ library: "tesseract" }, /unknown library "tesseract" \(one of onnxruntime, directml, pdfium, whisper-cpp, ggml, sherpa-onnx\)/);
  refuses({ library: "sqlite-vec" }, /library sqlite-vec is a static crate: depend on it through Cargo/);
  refuses({ library: "libgit2" }, /library libgit2 is a static crate: depend on it through Cargo/);
  refuses({ library: "ffmpeg" }, /library ffmpeg has its own kind: use \{ kind: "ffmpeg"/);
  refuses({ library: "constructor" }, /unknown library "constructor"/);
  for (const version of [undefined, "", "../x", "a b", "chromium\\8021", "-1.0", ".1", "x".repeat(65), 1]) refuses({ version }, /version must be a catalog version token/);
  for (const files of ["libx.dylib", [], [""], ["sub/libx.dylib"], ["../libx.dylib"], ["a\\b.dll"], [5], ["libx.dylib", "libx.dylib"]]) refuses({ files }, /files (must be a non-empty list of bare file names|names a file twice)/);
  for (const source of ["dir", {}, { dir: "" }, { dir: "   " }, { dir: 5 }, { dir: "https://x.example/builds" }, { dir: "." }, { dir: ".." }, { dir: "../" }, { dir: "C:builds" }]) refuses({ source }, /source(\.dir)? must (be|name)/);
  refuses({ source: { dir: "builds", url: "https://x.example/f" } }, /source: unknown key "url"/);
  refuses({ companions: "yes" }, /companions must be a boolean/);
  refuses({ as: "sub/x.dll" }, /as must be a bare file name/);
  refuses({ clean: true }, /unknown key "clean"/);
  refuses({ files: ["a.dll", "b.dll"], as: "c.dll" }, /as renames one file, so files must list exactly one \(it lists 2\)/);
  assert.ok(errorsOf({ kind: "onnxruntime", version: "1.24.4", catalog: "c.json", library: "pdfium" }).some((line) => /unknown key "library" \(allowed: kind, version, catalog, resourceDir, as, licenseDir, cacheDir, companions, platforms\)/.test(line)));
  assert.ok(errorsOf({ kind: "onnxruntime", version: "1.24.4", files: ["a.dll"] }, { kind: "onnxruntime", version: "1.24.4", source: { dir: "x" } }).length >= 2, "the alias keeps its own key list");
  assert.ok(errorsOf({ kind: "onnxruntime", version: "chromium/8021" }).some((line) => /version must be x\.y\.z/.test(line)), "the alias keeps x.y.z");

  const ok = validateSidecarManifest([
    base,
    { ...base, library: "ggml", version: "0.20.2", catalog: { crate: "rightkit-ffmpeg" }, files: ["libggml.0.20.2.dylib"], as: "libggml.dylib", companions: false, source: { dir: "../retained/ggml/" }, resourceDir: "src-tauri/lib", licenseDir: "legal/ggml", cacheDir: ".cache/ggml" },
    { ...base, library: "whisper-cpp", version: "1.9.3", source: { dir: path.resolve("/abs/builds") } },
    { kind: "onnxruntime", version: "1.24.4", catalog: "c.json" },
  ], { target: MAC }).entries;
  assert.deepEqual([ok[0].library, ok[0].version, ok[0].resourceDir, ok[0].licenseDir, ok[0].cacheDir, ok[0].companions, ok[0].source, ok[0].files], ["pdfium", "chromium/8021", "src-tauri/resources/runtime", "src-tauri/resources/runtime/LICENSES", ".cache/native-library", true, undefined, undefined]);
  assert.deepEqual([ok[1].files, ok[1].as, ok[1].companions, ok[1].source, ok[1].cacheDir, ok[1].licenseDir, ok[1].catalogSpec], [["libggml.0.20.2.dylib"], "libggml.dylib", false, { dir: "../retained/ggml" }, ".cache/ggml", "legal/ggml", { via: "crate", crate: "rightkit-ffmpeg", file: "catalog/native-libraries.json" }]);
  assert.deepEqual(ok[2].source, { dir: path.resolve("/abs/builds") });
  assert.deepEqual([ok[3].library, ok[3].companions, ok[3].cacheDir], ["onnxruntime", true, ".cache/onnxruntime"], "the alias keeps its cache directory and gains companions: true");
  // An entry skipped on this platform is still shape-checked.
  assert.ok(errorsOf({ ...base, platforms: ["win"], library: "nope" }).length > 0);
});

test("native-library outputs are claimed, and its cacheDir is protected like the alias's", () => {
  const entry = { kind: "native-library", library: "ggml", version: "0.20.2", catalog: "c.json" };
  const clash = (...items) => { try { validateSidecarManifest(items, { target: MAC }); } catch (error) { return error.details.errors; } return []; };
  assert.ok(clash({ ...entry, files: ["libggml.dylib"] }, { ...entry, library: "whisper-cpp", version: "1.9.3", files: ["libggml.dylib"] }).some((line) => /writes src-tauri\/resources\/runtime\/libggml\.dylib, which sidecars\[0\] \(native-library\) already writes/.test(line)));
  assert.ok(clash({ ...entry, files: ["a.dylib"] }, { kind: "resource", from: "x", to: "src-tauri/resources/runtime/a.dylib" }).some((line) => /already writes/.test(line)));
  assert.ok(clash({ ...entry, as: "b.dylib", files: ["a.dylib"] }, { kind: "resource", from: "x", to: "src-tauri/resources/runtime/b.dylib" }).some((line) => /already writes/.test(line)), "`as` is the claimed name");
  assert.deepEqual(clash({ ...entry, files: ["a.dylib"] }, { ...entry, library: "ggml", files: ["b.dylib"] }), [], "different files in one resource directory do not clash");
  assert.deepEqual(clash(entry, { ...entry, library: "whisper-cpp" }), [], "a catalog-driven entry names nothing at validation time");
  assert.ok(clash(entry, { kind: "resource", from: "x", to: ".cache/native-library" }).some((line) => /is or contains \.cache\/native-library/.test(line)));
  assert.ok(clash({ ...entry, cacheDir: ".cache/ggml-builds" }, { kind: "resource", from: "x", to: ".cache/ggml-builds" }).some((line) => /is or contains \.cache\/ggml-builds/.test(line)));
  assert.deepEqual(clash({ ...entry, platforms: ["win"], files: ["a.dylib"] }, { kind: "resource", from: "x", to: "src-tauri/resources/runtime/a.dylib" }), [], "a skipped entry claims nothing");
});

test("--check lists native-library outputs with no catalog: the files named, the name as gives, or the catalog's artifactFileName", () => {
  const manifest = [
    { kind: "native-library", library: "ggml", version: "0.20.2", catalog: "c.json", files: ["libggml.0.20.2.dylib", "libggml-base.0.20.2.dylib"], source: { dir: "../retained" } },
    { kind: "native-library", library: "whisper-cpp", version: "1.9.3", catalog: "c.json", files: ["libwhisper.1.9.3.dylib"], as: "libwhisper.dylib" },
    { kind: "native-library", library: "pdfium", version: "chromium/8021", catalog: "c.json", resourceDir: "src-tauri/resources/pdf" },
    { kind: "native-library", library: "sherpa-onnx", version: "1.13.4", catalog: "c.json", as: "libsherpa.dylib", platforms: ["win"] },
    { kind: "onnxruntime", version: "1.30.0", catalog: "c.json" },
  ];
  const plan = planSidecarStaging({ manifest, target: MAC, env: {} });
  assert.deepEqual(plan.entries[0].outputs, ["src-tauri/resources/runtime/libggml.0.20.2.dylib", "src-tauri/resources/runtime/libggml-base.0.20.2.dylib"]);
  assert.deepEqual(plan.entries[1].outputs, ["src-tauri/resources/runtime/libwhisper.dylib"]);
  assert.deepEqual(plan.entries[2].outputs, ["src-tauri/resources/pdf/<catalog artifactFileName>"]);
  assert.equal(plan.entries[3].skipped, true);
  assert.deepEqual(plan.entries[4].outputs, ["src-tauri/resources/runtime/<catalog artifactFileName>"]);
  assert.deepEqual([plan.entries[0].library, plan.entries[0].version, plan.entries[0].externalBin, plan.entries[2].version, "library" in plan.entries[4]], ["ggml", "0.20.2", false, "chromium/8021", false]);
  assert.ok(formatSidecarPlan(plan).some((line) => line.includes("[2] native-library: src-tauri/resources/pdf/<catalog artifactFileName>")));
  assert.throws(() => planSidecarStaging({ manifest: [{ kind: "native-library", library: "sqlite-vec", version: "0.1.9", catalog: "c.json" }], target: MAC, env: {} }), { code: "STAGE_SIDECAR_CONFIG_INVALID" });
});

// ---- the real native catalog (crates/rightkit-ffmpeg/catalog/native-libraries.json): self-consistent, and selectable without a network ----

const REAL_CRATE_DIR = path.dirname(path.dirname(fileURLToPath(new URL("../../crates/rightkit-ffmpeg/catalog/native-libraries.json", import.meta.url))));
const HEX64 = /^[0-9a-f]{64}$/;

test("the real native catalog: every dynamic-library row is consistent (digests, https archives, notice files, companions, hostProof, planned rows)", () => {
  const rows = ORT_REAL_CATALOG.libraries;
  const servingStatus = (row) => row.status === "approved" || row.status === "pinned";
  assert.equal(new Set(rows.map((row) => row.id)).size, rows.length, "row ids are unique");
  const dynamic = rows.filter((row) => row.linkage === "dynamic-library");
  assert.ok(dynamic.length >= 20, `${dynamic.length} dynamic-library rows`);
  for (const row of dynamic) {
    if (servingStatus(row)) {
      assert.ok([row.sha256, row.contentSha256, ...Object.values(row.contentSha256ByArch ?? {})].some((digest) => HEX64.test(digest ?? "")), `${row.id} pins a sha256 or a content digest`);
      assert.match(row.artifactFileName ?? "", /^[A-Za-z0-9][A-Za-z0-9_.-]*$/, `${row.id} artifactFileName`);
      assert.ok(Array.isArray(row.noticeFiles) && row.noticeFiles.length > 0, `${row.id} ships notice files`);
    }
    for (const key of ["sha256", "contentSha256"]) if (row[key] !== undefined) assert.match(row[key], HEX64, `${row.id}.${key}`);
    if (row.contentSha256ByArch !== undefined) {
      assert.equal(row.arch, "universal", `${row.id}: contentSha256ByArch is for universal rows`);
      assert.deepEqual(Object.keys(row.contentSha256ByArch).sort(), ["aarch64", "x86_64"]);
      for (const digest of Object.values(row.contentSha256ByArch)) assert.match(digest, HEX64);
    }
    if (row.arch === "universal") assert.equal(row.contentSha256, undefined, `${row.id}: a universal row pins contentSha256ByArch, not contentSha256`);
    if (row.archive !== undefined) {
      assert.match(row.archive.url, /^https:\/\/[^\s/]+\/\S+$/, `${row.id} archive url`);
      assert.match(row.archive.sha256, HEX64, `${row.id} archive sha256`);
      if (row.archive.member !== undefined) assert.equal(normalizeArchiveMember(row.archive.member).error, undefined, `${row.id} archive.member`);
    }
    for (const file of row.noticeFiles ?? []) assert.ok(existsSync(path.join(REAL_CRATE_DIR, ...file.split("/"))), `${row.id}: ${file} exists under crates/rightkit-ffmpeg`);
    for (const id of row.companions ?? []) {
      const companion = rows.find((candidate) => candidate.id === id);
      assert.ok(companion, `${row.id}: companion ${id} exists`);
      assert.equal(companion.os, row.os, `${row.id}: companion ${id} serves the same os`);
      assert.ok(companion.arch === row.arch || companion.arch === "universal" || row.arch === "universal", `${row.id}: companion ${id} has a compatible arch`);
      assert.ok(servingStatus(companion), `${row.id}: companion ${id} is ${companion.status}`);
      assert.deepEqual(companion.companions ?? [], [], `${id} has no companions of its own`);
    }
    if (row.hostProof !== undefined) {
      assert.equal(row.hostProof, "never-run", `${row.id} hostProof`);
      assert.notEqual(row.status, "approved", `${row.id}: hostProof only on a row that is not approved`);
    }
    if (row.status === "planned") for (const key of ["sha256", "contentSha256", "contentSha256ByArch", "archive"]) assert.equal(row[key], undefined, `${row.id}: a planned row carries no ${key}`);
  }
});

test("the real native catalog: every (library, os, arch) the apps need selects, or refuses, with a typed error and no network", async () => {
  const noticeFiles = Object.fromEntries(ORT_REAL_CATALOG.libraries.flatMap((row) => row.noticeFiles ?? []).filter((file) => existsSync(path.join(REAL_CRATE_DIR, ...file.split("/")))).map((file) => [path.join(REAL_CRATE_DIR, ...file.split("/")), readFileSync(path.join(REAL_CRATE_DIR, ...file.split("/")))]));
  const attempt = async (target, entry) => {
    const { fs } = fakeFs(noticeFiles);
    try {
      await stageSidecars({
        appRoot: APP, target, manifest: [{ kind: "native-library", catalog: "c.json", ...entry }],
        deps: deps({ fs, run: () => ({ status: 0, stdout: "", stderr: "" }), loadNativeCatalog: () => ({ catalog: ORT_REAL_CATALOG, crateDir: REAL_CRATE_DIR }), fetchBytes: async (url) => { throw new StageSidecarError("STAGE_SIDECAR_DOWNLOAD_FAILED", `no network in tests: ${url}`); }, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {} }),
      });
    } catch (error) {
      assert.ok(error instanceof StageSidecarError, `${target} ${entry.library}: ${error.stack}`);
      return error.code;
    }
    return "staged";
  };
  const MAC_X86 = "x86_64-apple-darwin";
  const serves = [
    [MAC, "onnxruntime", "1.30.0"], [MAC_X86, "onnxruntime", "1.30.0"], [UNIVERSAL, "onnxruntime", "1.30.0"], [WIN, "onnxruntime", "1.24.4"], [WIN, "directml", "1.15.4"],
    [MAC, "pdfium", "chromium/8021"], [MAC_X86, "pdfium", "chromium/8021"], [WIN, "pdfium", "chromium/8021"],
    [MAC, "whisper-cpp", "1.9.3"], [MAC, "ggml", "0.20.2"], [MAC, "sherpa-onnx", "1.13.4"], [MAC_X86, "sherpa-onnx", "1.13.4"], [UNIVERSAL, "sherpa-onnx", "1.13.4"],
  ];
  const refuses = [
    [MAC, "onnxruntime", "1.24.4"], [MAC_X86, "onnxruntime", "1.24.4"], [WIN, "whisper-cpp", "1.9.3"], [WIN, "ggml", "0.20.2"], [WIN, "sherpa-onnx", "1.13.4"],
    [MAC_X86, "whisper-cpp", "1.9.3"], [MAC_X86, "ggml", "0.20.2"], [UNIVERSAL, "whisper-cpp", "1.9.3"], [UNIVERSAL, "ggml", "0.20.2"], [MAC, "pdfium", "chromium/9999"], ["x86_64-unknown-linux-gnu", "pdfium", "chromium/8021"],
  ];
  for (const [target, library, version] of serves) {
    // With a source dir that holds nothing, selection and every row check pass and the missing file is the typed stop (nothing is downloaded).
    assert.equal(await attempt(target, { library, version, source: { dir: "/rk-absent-builds" } }), "STAGE_SIDECAR_BINARY_NOT_FOUND", `${target} ${library} ${version} from source.dir`);
    // Without one: a row with an archive reaches the (refused) download, a row measured from a retained build asks for source.dir.
    const code = await attempt(target, { library, version });
    assert.ok(code === "STAGE_SIDECAR_DOWNLOAD_FAILED" || code === "STAGE_SIDECAR_CONFIG_INVALID", `${target} ${library} ${version}: ${code}`);
  }
  for (const [target, library, version] of refuses) {
    assert.equal(await attempt(target, { library, version, source: { dir: "/rk-absent-builds" } }), "STAGE_SIDECAR_NOT_APPROVED", `${target} ${library} ${version}`);
  }
});

test("native-library refuses a catalog row whose fields are inconsistent, before anything is downloaded or written", async () => {
  const bytes = Buffer.from("library bytes");
  const tgz = archiveOf("https://example.test/lib.tgz", { "lib/libx.dylib": bytes });
  const good = rowOf("x-1.0.0-macos-arm64", "pdfium", "1.0.0", "macos", "aarch64", bytes, { status: "pinned", artifactFileName: "libx.dylib", archive: archiveRef(tgz, "lib/libx.dylib") });
  const entry = { kind: "native-library", library: "pdfium", version: "1.0.0", ...NATIVE_CATALOG_ENTRY };
  const refused = async (over, needle, archives = [tgz]) => {
    const kit = nativeKit({ rows: [{ ...good, ...over }], archives, target: MAC });
    await assert.rejects(kit.stage([entry]), { code: "STAGE_SIDECAR_NOT_APPROVED", message: needle }, JSON.stringify(over));
    assert.deepEqual([kit.fetched, kit.written()], [[], []], JSON.stringify(over));
  };
  assert.equal((await nativeKit({ rows: [good], archives: [tgz], target: MAC }).stage([entry])).files[0].name, "libx.dylib", "the unmodified row stages");
  for (const artifactFileName of [undefined, "", "../libx.dylib", "sub/libx.dylib", "lib x.dylib", 5]) await refused({ artifactFileName }, /artifactFileName/);
  await refused({ sha256: "ABC" }, /sha256 is not a lowercase 64-hex/);
  await refused({ sha256: undefined }, /pins no digest/);
  await refused({ archive: { url: "http://example.test/lib.tgz", sha256: sha(tgz.text) } }, /no valid archive \(an https url and a sha256 digest\)/);
  await refused({ archive: { url: "https://example.test/lib.tgz", sha256: "nope" } }, /no valid archive/);
  await refused({ archive: { url: "https://example.test/lib.tgz" } }, /no valid archive/);
  await refused({ archive: "https://example.test/lib.tgz" }, /no valid archive/);
  await refused({ archive: { url: "https://example.test/", sha256: sha("x") } }, /archive url that names no file/);
  await refused({ contentSha256ByArch: { aarch64: sha("a"), x86_64: sha("b") } }, /pins contentSha256ByArch, which only a universal row carries/);
  await refused({ arch: "universal", contentSha256: sha("a") }, /universal row: it pins contentSha256ByArch, not contentSha256/);
  await refused({ arch: "universal", contentSha256ByArch: { aarch64: sha("a") } }, /contentSha256ByArch must be exactly \{ aarch64, x86_64 \}/);
  await refused({ arch: "universal", contentSha256ByArch: { aarch64: sha("a"), x86_64: "zz" } }, /contentSha256ByArch\.x86_64 is not a lowercase 64-hex/);
  await refused({ arch: "universal", contentSha256ByArch: [sha("a"), sha("b")] }, /contentSha256ByArch must be exactly/);
});

test("native-library resolves its catalog like the alias ({ crate }, { npm }, default file) and hands deps.loadNativeCatalog the entry", async () => {
  const crateDir = path.resolve("/rk-fake-crates/rightkit-ffmpeg");
  const bytes = Buffer.from("pdfium for the test");
  const row = rowOf("pdfium-test", "pdfium", "chromium/8021", "macos", "aarch64", bytes, { artifactFileName: "libpdfium.dylib" });
  const entryFor = (catalog) => ({ kind: "native-library", library: "pdfium", version: "chromium/8021", catalog, source: { dir: "builds" } });
  for (const catalog of [{ crate: "rightkit-ffmpeg" }, { npm: "@orthic/catalogs/native-libraries.json" }]) {
    const { fs } = fakeFs({
      [path.join(crateDir, "catalog", "native-libraries.json")]: JSON.stringify({ libraries: [row] }), [path.join(crateDir, "third-party", "pdfium-MIT.txt")]: "notice",
      [path.resolve("/node_modules/@orthic/catalogs/native-libraries.json")]: JSON.stringify({ libraries: [row] }), [path.resolve("/node_modules/@orthic/third-party/pdfium-MIT.txt")]: "notice",
      [path.resolve("/node_modules/.rk-app/builds/libpdfium.dylib")]: bytes,
    });
    const calls = [];
    const run = (command, args) => {
      calls.push([command, ...args]);
      return { status: 0, stderr: "", stdout: JSON.stringify({ packages: [{ name: "rightkit-ffmpeg", manifest_path: path.join(crateDir, "Cargo.toml") }] }) };
    };
    const result = await stageSidecars({ appRoot: path.resolve("/node_modules/.rk-app"), target: MAC, manifest: [entryFor(catalog)], deps: deps({ fs, run }) });
    assert.deepEqual([result.files[0].sha256, result.files[0].library], [sha(bytes), "pdfium"]);
    assert.equal(calls.some((call) => call[0] === "rustright" && call[2] === "metadata"), "crate" in catalog);
  }
  const { fs } = fakeFs();
  const missing = (catalog) => stageSidecars({ appRoot: APP, target: MAC, manifest: [entryFor(catalog)], deps: deps({ fs, run: () => ({ status: 0, stderr: "", stdout: JSON.stringify({ packages: [] }) }) }) });
  await assert.rejects(missing({ crate: "missing-crate" }), { code: "STAGE_SIDECAR_CATALOG_UNREADABLE", message: /native-libraries catalog crate missing-crate/ });
  await assert.rejects(missing({ npm: "nope/x.json" }), { code: "STAGE_SIDECAR_CATALOG_UNREADABLE", message: /native-libraries catalog nope\/x\.json/ });
  await assert.rejects(stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "native-library", library: "pdfium", version: "chromium/8021" }], deps: deps({ fs }) }), { code: "STAGE_SIDECAR_CATALOG_UNREADABLE", message: /native-library needs a catalog path/ });
  // The hook serves both kinds and receives the validated entry.
  const seen = [];
  const hook = (rows) => ({ loadNativeCatalog: ({ entry, appRoot }) => { seen.push([entry.kind, entry.library, entry.version, appRoot]); return { catalog: { libraries: rows }, crateDir: NATIVE_CRATE }; } });
  await nativeKit({ rows: [row], files: { [sourceAt("libpdfium.dylib")]: bytes }, target: MAC }).stage([{ ...entryFor("c.json"), source: { dir: SOURCE_DIR } }], hook([row]));
  const ortTgz = archiveOf("https://example.test/ort.tgz", { "libonnxruntime.dylib": Buffer.from("ort") });
  const ortRow = rowOf("ort-test", "onnxruntime", "1.24.4", "macos", "aarch64", Buffer.from("ort"), { artifactFileName: "libonnxruntime.dylib", archive: archiveRef(ortTgz) });
  await nativeKit({ rows: [ortRow], archives: [ortTgz], target: MAC }).stage([{ kind: "onnxruntime", version: "1.24.4", catalog: "c.json" }], hook([ortRow]));
  assert.deepEqual(seen, [["native-library", "pdfium", "chromium/8021", APP], ["onnxruntime", "onnxruntime", "1.24.4", APP]]);
});

test("native-library receipt rows are catalog-backed: license-notices binaries needs no declaration for them", async () => {
  const kit = winOrtFixture();
  const { receipt } = await kit.stage([ORT_WIN_ENTRY]);
  assert.equal(receipt.files.length, 2);
  assert.deepEqual(uncoveredBinaries(receipt, []), []);
  assert.deepEqual(uncoveredBinaries({ files: [...receipt.files, { kind: "remote", name: "yt-dlp" }, { kind: "command", name: "helper" }] }, []), ["helper", "yt-dlp"], "non-catalog kinds still need a declaration");
});

// ---- ffmpeg resourceDir: ffmpeg and ffprobe staged flat into a resource directory (plan item 2.8, C5-07)

const RUNTIME = "src-tauri/resources/runtime";
const runtimeDirAt = at(...RUNTIME.split("/"));
const runtimeEntry = (extra) => ({ kind: "ffmpeg", notices: NOTICES, catalog: "c/artifacts.json", resourceDir: RUNTIME, ...extra });
/** Names of the files under `dir` (app-relative), sorted. */
const listUnder = (files, dir) => [...files.keys()].filter((key) => key.startsWith(path.resolve(APP, dir) + path.sep)).map((key) => path.relative(path.resolve(APP, dir), key).split(path.sep).join("/")).sort();

function runtimeCatalog(rows) {
  const crateDir = path.resolve("/rk-fake-ffmpeg-crate");
  const artifacts = rows.map(([tool, variant, osName, arch, bytes, id = `${tool}-${variant}-${osName}-${arch}`]) => ({
    id, tool, variant, os: osName, arch, status: "approved", version: "7.1", contentSha256: sha(bytes), sourceSha256: sha("src"),
    attribution: { noticeFiles: ["notices/LGPL-2.1.txt"], sourceUrl: "https://ffmpeg.org/releases/ffmpeg-7.1.tar.xz", licenseSpdx: "LGPL-2.1-or-later", buildProvenance: "recipe" },
  }));
  return { crateDir, catalog: { schemaVersion: 2, artifacts }, initial: { [path.join(crateDir, "notices", "LGPL-2.1.txt")]: "LGPL text" } };
}

/** A fake kit for a thin or universal target: catalog rows, an archive per tool that unpacks `members`, and `stage(manifest, deps)`. */
function runtimeKit({ target = MAC, rows, members = {}, seeded = {} }) {
  const { crateDir, catalog, initial } = runtimeCatalog(rows);
  const { fs, files, log } = fakeFs({ ...initial, ...seeded });
  const exe = target === WIN ? ".exe" : "";
  const sources = {
    ffmpeg: { url: "https://example.test/ffmpeg.zip", sha256: sha("zip-ffmpeg"), member: `ffmpeg${exe}` },
    ffprobe: { url: "https://example.test/ffprobe.zip", sha256: sha("zip-ffprobe"), member: `ffprobe${exe}` },
  };
  const fetch = async (url) => ({ ok: true, status: 200, arrayBuffer: async () => Buffer.from(url.endsWith("ffmpeg.zip") ? "zip-ffmpeg" : "zip-ffprobe") });
  const extractMember = ({ fileName, destDir }) => { const out = path.join(destDir, fileName); files.set(out, Buffer.from(members[fileName])); return out; };
  const stage = (manifest, extra = {}) => stageSidecars({
    appRoot: APP, target, manifest,
    deps: deps({ fs, fetch, extractMember, makeTempDir: () => path.resolve("/rk-scratch"), removeDir: () => {}, loadMediaCatalog: () => ({ catalog, crateDir, crateVersion: "0.9.0" }), ...extra }),
  });
  return { fs, files, log, sources, stage, catalog, crateDir };
}
const MAC_ROWS = [["ffmpeg", "decode", "macos", "aarch64", "mac-ffmpeg"], ["ffprobe", "decode", "macos", "aarch64", "mac-ffprobe"]];
const MAC_MEMBERS = { ffmpeg: "mac-ffmpeg", ffprobe: "mac-ffprobe" };

test("ffmpeg resourceDir stages ffmpeg and ffprobe flat (no triple, not an externalBin) from the approved rows, with notices and a valid receipt", async () => {
  const kit = runtimeKit({ rows: MAC_ROWS, members: MAC_MEMBERS });
  const result = await kit.stage([runtimeEntry({ sources: kit.sources, probe: false })]);
  assert.deepEqual(result.files.map((file) => [file.kind, file.name, file.path, file.externalBin]), [
    ["ffmpeg", "ffmpeg", `${RUNTIME}/ffmpeg`, false], ["ffmpeg", "ffprobe", `${RUNTIME}/ffprobe`, false],
  ]);
  assert.deepEqual(result.externalBinNames, [], "a resourceDir file is not an externalBin");
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, "ffmpeg")).toString(), "mac-ffmpeg");
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, "ffprobe")).toString(), "mac-ffprobe");
  assert.deepEqual(listUnder(kit.files, RUNTIME), ["ffmpeg", "ffprobe"]);
  assert.deepEqual(listUnder(kit.files, "src-tauri/binaries"), ["stage-sidecar.json"], "nothing is staged under the externalBin stage directory but the receipt");
  assert.ok(kit.log.chmods.some(([file, mode]) => file.includes(`${path.sep}runtime${path.sep}ffmpeg`) && mode === 0o755), "the staged tool is executable");
  const [ffmpeg] = result.files;
  assert.deepEqual([ffmpeg.catalogRow, ffmpeg.version, ffmpeg.sha256, ffmpeg.catalogContentSha256, ffmpeg.license], ["ffmpeg-decode-macos-aarch64", "7.1", sha("mac-ffmpeg"), sha("mac-ffmpeg"), "LGPL-2.1-or-later"]);
  assert.deepEqual(ffmpeg.notices.map((item) => item.name).sort(), ["FFMPEG-SOURCE-OFFER.txt", "LGPL-2.1.txt"]);
  assert.equal(kit.fs.existsSync(at("src-tauri", "resources", "third-party", "ffmpeg", "FFMPEG-SOURCE-OFFER.txt")), true, "the source offer is written to notices.dir, not to resourceDir");
  validateStageSidecarReceipt(result.receipt, { target: MAC, names: [] });
  assert.deepEqual(uncoveredBinaries(result.receipt, []), [], "the ffmpeg rows stay catalog-backed");
  assert.deepEqual(JSON.parse(kit.fs.readFileSync(result.receiptPath)).files.map((file) => file.path), [`${RUNTIME}/ffmpeg`, `${RUNTIME}/ffprobe`]);
});

test("ffmpeg without resourceDir is unchanged: <stageDir>/<tool>-<triple>, externalBin true, nothing in a runtime directory", async () => {
  const kit = runtimeKit({ rows: MAC_ROWS, members: MAC_MEMBERS });
  const { resourceDir: _resourceDir, ...plain } = runtimeEntry({ sources: kit.sources });
  const result = await kit.stage([plain]);
  assert.deepEqual(result.files.map((file) => [file.name, file.path, file.externalBin]), [["ffmpeg", `src-tauri/binaries/ffmpeg-${MAC}`, true], ["ffprobe", `src-tauri/binaries/ffprobe-${MAC}`, true]]);
  assert.deepEqual(result.externalBinNames, ["ffmpeg", "ffprobe"]);
  assert.deepEqual(listUnder(kit.files, "src-tauri/binaries"), [`ffmpeg-${MAC}`, `ffprobe-${MAC}`, "stage-sidecar.json"]);
  assert.deepEqual(listUnder(kit.files, RUNTIME), []);
  assert.equal("resourceDir" in validateSidecarManifest([plain], { target: MAC }).entries[0], false);
});

test("ffmpeg resourceDir on a Windows target stages <resourceDir>/ffmpeg.exe and ffprobe.exe with no triple", async () => {
  const kit = runtimeKit({
    target: WIN, rows: [["ffmpeg", "decode", "windows", "x86_64", "win-ffmpeg"], ["ffprobe", "decode", "windows", "x86_64", "win-ffprobe"]],
    members: { "ffmpeg.exe": "win-ffmpeg", "ffprobe.exe": "win-ffprobe" },
  });
  const result = await kit.stage([runtimeEntry({ sources: kit.sources })]);
  assert.deepEqual(result.files.map((file) => [file.name, file.path, file.externalBin, file.catalogRow]), [
    ["ffmpeg", `${RUNTIME}/ffmpeg.exe`, false, "ffmpeg-decode-windows-x86_64"], ["ffprobe", `${RUNTIME}/ffprobe.exe`, false, "ffprobe-decode-windows-x86_64"],
  ]);
  assert.deepEqual(listUnder(kit.files, RUNTIME), ["ffmpeg.exe", "ffprobe.exe"]);
  assert.equal(listUnder(kit.files, "src-tauri/binaries").some((name) => name.includes("windows-msvc")), false, "no triple-named file is staged");
  assert.deepEqual(kit.log.chmods, [], "no exec bit on Windows");
  assert.deepEqual(result.externalBinNames, []);
  validateStageSidecarReceipt(result.receipt, { target: WIN, names: [] });
});

test("ffmpeg resourceDir on universal-apple-darwin stages both slices, lipo-merges each tool into <resourceDir>/<tool> and lists both slices in the receipt", async () => {
  const rows = [
    ["ffmpeg", "decode", "macos", "aarch64", "arm-ffmpeg", "ffmpeg-arm"], ["ffmpeg", "decode", "macos", "x86_64", "x86-ffmpeg", "ffmpeg-x86"],
    ["ffprobe", "decode", "macos", "aarch64", "arm-ffprobe", "ffprobe-arm"], ["ffprobe", "decode", "macos", "x86_64", "x86-ffprobe", "ffprobe-x86"],
  ];
  const kit = runtimeKit({ target: UNIVERSAL, rows });
  const stageCalls = [];
  const lipoOutputs = [];
  const run = (command, args) => {
    if (command === "rustright" && args[0] === "ffmpeg-stage") {
      stageCalls.push(args);
      const [, triple, stageDir] = args;
      const kind = triple.startsWith("aarch64") ? "arm" : "x86";
      for (const tool of ["ffmpeg", "ffprobe"]) kit.files.set(path.resolve(APP, stageDir, `${tool}-${triple}`), Buffer.from(`${kind}-${tool}`));
    }
    if (command === "lipo" && args[0] === "-create") { const out = args[args.indexOf("-output") + 1]; lipoOutputs.push(out); kit.files.set(out, Buffer.from(`fat-${path.basename(out).split(".")[0]}`)); }
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  const manifest = [runtimeEntry({ stage: { cmd: "rustright", args: ["ffmpeg-stage", "{triple}", "{stageDir}"] } })];
  const result = await kit.stage(manifest, { run });
  assert.deepEqual(stageCalls, [["ffmpeg-stage", "aarch64-apple-darwin", ".right-release/slices/aarch64-apple-darwin"], ["ffmpeg-stage", "x86_64-apple-darwin", ".right-release/slices/x86_64-apple-darwin"]],
    "the stage command keeps staging each slice under its own stage directory, not into resourceDir");
  assert.equal(lipoOutputs.length, 2);
  assert.deepEqual(listUnder(kit.files, RUNTIME), ["ffmpeg", "ffprobe"], "only the merged tools are in resourceDir, no slice files and no triple");
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, "ffmpeg")).toString(), "fat-ffmpeg");
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, "ffprobe")).toString(), "fat-ffprobe");
  assert.deepEqual(listUnder(kit.files, "src-tauri/binaries"), ["stage-sidecar.json"]);
  assert.deepEqual(result.files.map((file) => [file.name, file.path, file.externalBin, file.catalogRow]), [
    ["ffmpeg", `${RUNTIME}/ffmpeg`, false, "ffmpeg-arm+ffmpeg-x86"], ["ffprobe", `${RUNTIME}/ffprobe`, false, "ffprobe-arm+ffprobe-x86"],
  ]);
  const [ffmpeg] = result.files;
  assert.equal(ffmpeg.sha256, sha("fat-ffmpeg"));
  assert.deepEqual(ffmpeg.slices.map((slice) => [slice.triple, slice.sha256, slice.catalogRow, slice.catalogContentSha256]), [
    ["aarch64-apple-darwin", sha("arm-ffmpeg"), "ffmpeg-arm", sha("arm-ffmpeg")], ["x86_64-apple-darwin", sha("x86-ffmpeg"), "ffmpeg-x86", sha("x86-ffmpeg")],
  ]);
  const offer = kit.fs.readFileSync(at("src-tauri", "resources", "third-party", "ffmpeg", "FFMPEG-SOURCE-OFFER.txt")).toString();
  assert.ok(offer.includes("ffmpeg-arm") && offer.includes("ffmpeg-x86"), "one source offer names both slices");
  assert.deepEqual(result.externalBinNames, []);
  validateStageSidecarReceipt(result.receipt, { target: UNIVERSAL, names: [] });
});

test("ffmpeg resourceDir with variants: one entry stages the export-ass ffmpeg and the decode ffprobe into runtime/; a swapped archive stages nothing", async () => {
  const rows = [["ffmpeg", "export-ass", "macos", "aarch64", "ass-ffmpeg"], ["ffmpeg", "decode", "macos", "aarch64", "decode-ffmpeg"], ["ffprobe", "decode", "macos", "aarch64", "decode-ffprobe"]];
  const entry = (kit) => runtimeEntry({ variants: { ffmpeg: "export-ass" }, sources: kit.sources });
  const ok = runtimeKit({ rows, members: { ffmpeg: "ass-ffmpeg", ffprobe: "decode-ffprobe" } });
  const result = await ok.stage([entry(ok)]);
  assert.deepEqual(result.files.map((file) => [file.name, file.variant, file.catalogRow, file.path, file.externalBin]), [
    ["ffmpeg", "export-ass", "ffmpeg-export-ass-macos-aarch64", `${RUNTIME}/ffmpeg`, false], ["ffprobe", "decode", "ffprobe-decode-macos-aarch64", `${RUNTIME}/ffprobe`, false],
  ]);
  assert.equal(ok.fs.readFileSync(path.join(runtimeDirAt, "ffmpeg")).toString(), "ass-ffmpeg");
  assert.equal(ok.fs.readFileSync(path.join(runtimeDirAt, "ffprobe")).toString(), "decode-ffprobe");
  assert.deepEqual(result.externalBinNames, []);
  validateStageSidecarReceipt(result.receipt, { target: MAC, names: [] });
  // The catalog is the authority: the decode ffmpeg is not the export-ass row, so it never reaches runtime/.
  const swapped = runtimeKit({ rows, members: { ffmpeg: "decode-ffmpeg", ffprobe: "decode-ffprobe" } });
  await assert.rejects(swapped.stage([entry(swapped)]), { code: "STAGE_SIDECAR_HASH_MISMATCH", message: /approved catalog row ffmpeg-export-ass-macos-aarch64/ });
  assert.deepEqual(listUnder(swapped.files, RUNTIME), [], "nothing is left in resourceDir");
  assert.equal(swapped.fs.existsSync(runtimeDirAt), false);
  assert.deepEqual(listUnder(swapped.files, "src-tauri/binaries"), [], "and nothing is staged as an externalBin either");
});

test("ffmpeg resourceDir with a stage command: the command writes into resourceDir itself, and the staged bytes are held to the approved row", async () => {
  const run = (kit, bytes, dir = RUNTIME) => (command, args) => {
    if (command === "rustright" && args[0] === "ffmpeg-stage") {
      assert.deepEqual(args, ["ffmpeg-stage", "{stageDir}"], "a thin target passes the arguments as written: nothing is substituted");
      for (const tool of ["ffmpeg", "ffprobe"]) kit.files.set(path.resolve(APP, dir, tool), Buffer.from(bytes[tool]));
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  const entry = runtimeEntry({ stage: { cmd: "rustright", args: ["ffmpeg-stage", "{stageDir}"] } });
  const ok = runtimeKit({ rows: MAC_ROWS });
  const result = await ok.stage([entry], { run: run(ok, MAC_MEMBERS) });
  assert.deepEqual(result.files.map((file) => [file.path, file.externalBin]), [[`${RUNTIME}/ffmpeg`, false], [`${RUNTIME}/ffprobe`, false]]);
  const tampered = runtimeKit({ rows: MAC_ROWS });
  await assert.rejects(tampered.stage([entry], { run: run(tampered, { ffmpeg: "tampered", ffprobe: "mac-ffprobe" }) }), { code: "STAGE_SIDECAR_HASH_MISMATCH" });
  // A command that still writes the externalBin name (<stageDir>/<tool>-<triple>) does not satisfy a resourceDir entry: the expected file is named.
  const legacy = runtimeKit({ rows: MAC_ROWS });
  const legacyRun = (command, args) => { if (command === "rustright") for (const tool of ["ffmpeg", "ffprobe"]) legacy.files.set(at("src-tauri", "binaries", `${tool}-${MAC}`), Buffer.from(`mac-${tool}`)); return { status: 0, stdout: "", stderr: "" }; };
  await assert.rejects(legacy.stage([entry], { run: legacyRun }), (error) => error.code === "STAGE_SIDECAR_FFMPEG_FAILED" && error.details.causeCode === "MEDIA_SIDECAR_NOT_STAGED" && error.message.includes(`${path.sep}runtime${path.sep}ffmpeg`));
});

test("ffmpeg resourceDir is validated: a contained relative path, never combined with bundleDir", () => {
  const base = { kind: "ffmpeg", notices: NOTICES, catalog: "c.json" };
  const errorsOf = (item, target = MAC) => { try { validateSidecarManifest([item], { target }); } catch (error) { return error.details.errors; } return []; };
  for (const bad of ["/abs/runtime", "../runtime", "a/../../b", "C:/runtime", "", ".", 7]) {
    assert.ok(errorsOf({ ...base, resourceDir: bad }).some((line) => /resourceDir must (be a non-empty relative path|be relative and stay inside the app|name a path inside the app)/.test(line)), JSON.stringify(bad));
  }
  const both = errorsOf({ ...base, resourceDir: "runtime", bundleDir: "Contents/MacOS" });
  assert.ok(both.some((line) => /bundleDir and resourceDir cannot be combined/.test(line)), both.join("\n"));
  assert.ok(errorsOf({ ...base, resourceDir: "runtime", bundleDir: "Contents/Resources" }, UNIVERSAL).some((line) => line.includes("bundleDir and resourceDir")));
  assert.ok(errorsOf({ ...base, resourceDir: "runtime", bundleDir: "Contents/MacOS" }, WIN).some((line) => line.includes("bundleDir and resourceDir")));
  assert.deepEqual(errorsOf({ ...base, bundleDir: "Contents/Resources" }), [], "bundleDir alone is the externalBin layout, unchanged");
  assert.equal(validateSidecarManifest([{ ...base, resourceDir: "src-tauri/runtime/" }], { target: MAC }).entries[0].resourceDir, "src-tauri/runtime", "a trailing slash is dropped");
  assert.equal(validateSidecarManifest([{ ...base, resourceDir: "runtime", platforms: ["win"] }], { target: MAC }).entries[0].skipped, true, "a skipped entry is shape-checked and claims nothing");
  assert.ok(errorsOf({ ...base, resourceDir: "runtime", platforms: ["win"], bundleDir: "Contents/MacOS" }).some((line) => line.includes("bundleDir and resourceDir")), "also on a skipped entry");
});

test("ffmpeg resourceDir outputs are claimed: two entries (or a resource, onnxruntime or cargo-bin) may not write the same runtime file", () => {
  const base = { kind: "ffmpeg", notices: NOTICES, catalog: "c.json", resourceDir: RUNTIME };
  const errorsOf = (manifest, target = MAC) => { try { validateSidecarManifest(manifest, { target }); } catch (error) { return error.details.errors.join("\n"); } return ""; };
  assert.match(errorsOf([base, { ...base, tools: ["ffprobe"] }]), /sidecars\[1\] \(ffmpeg\) writes src-tauri\/resources\/runtime\/ffprobe, which sidecars\[0\] \(ffmpeg\) already writes/);
  assert.equal(errorsOf([{ ...base, tools: ["ffmpeg"] }, { ...base, tools: ["ffprobe"] }]), "", "one tool each into the same directory is fine");
  assert.equal(errorsOf([base, { ...base, resourceDir: "src-tauri/resources/other" }]), "", "another directory is fine");
  assert.match(errorsOf([base, { kind: "resource", from: "a", to: `${RUNTIME}/ffmpeg` }]), /already writes/);
  assert.match(errorsOf([base, { kind: "native-library", library: "pdfium", version: "chromium/8021", catalog: "c.json", resourceDir: RUNTIME, as: "ffprobe" }]), /already writes/);
  assert.match(errorsOf([base, { kind: "resource", from: "a", to: RUNTIME }]), /overlaps/);
  assert.match(errorsOf([base, { kind: "cargo-bin", package: "e", dest: "src-tauri/resources/runtime/ffmpeg" }]), /already writes/);
  assert.match(errorsOf([{ ...base, tools: ["ffmpeg"], resourceDir: "src-tauri/resources/runtime/" }, { kind: "remote", name: "x", url: "https://x.example.com/f", sha256: sha("f"), dest: "SRC-TAURI/resources/runtime/ffmpeg" }]), /already writes/, "case-insensitive, like every output");
  // The Windows names carry .exe: only the .exe file clashes.
  assert.match(errorsOf([base, { kind: "resource", from: "a", to: `${RUNTIME}/ffmpeg.exe` }], WIN), /already writes/);
  assert.equal(errorsOf([base, { kind: "resource", from: "a", to: `${RUNTIME}/ffmpeg` }], WIN), "");
  // Under universal-apple-darwin the merged file is the claim.
  assert.match(errorsOf([base, { kind: "resource", from: "a", to: `${RUNTIME}/ffmpeg` }], UNIVERSAL), /already writes/);
  // The externalBin layout claim is unchanged: <stageDir>/<tool>-<triple>.
  const { resourceDir: _resourceDir, ...plain } = base;
  assert.match(errorsOf([plain, { kind: "cargo-bin", package: "ffmpeg" }]), /already writes/);
  assert.equal(errorsOf([base, { kind: "cargo-bin", package: "ffmpeg" }]), "", "a resourceDir ffmpeg no longer claims the externalBin name");
});

test("wipeStageDir only knows stageDir names: resourceDir files are neither deleted nor mistaken for stale externalBins", async () => {
  const seeded = {
    [at("src-tauri", "binaries", `ffmpeg-${MAC}`)]: "stale externalBin ffmpeg", [at("src-tauri", "binaries", `ffprobe-${MAC}`)]: "stale externalBin ffprobe", [at("src-tauri", "binaries", "keep.txt")]: "k",
    [path.join(runtimeDirAt, "model.bin")]: "model", [path.join(runtimeDirAt, `ffmpeg-${MAC}`)]: "a file in resourceDir that looks like a stage name",
    [path.join(runtimeDirAt, "ffmpeg")]: "old runtime ffmpeg",
  };
  const kit = runtimeKit({ rows: MAC_ROWS, members: MAC_MEMBERS, seeded });
  const result = await kit.stage({ sidecars: [runtimeEntry({ sources: kit.sources })], wipeStageDir: true });
  assert.deepEqual(listUnder(kit.files, RUNTIME), ["ffmpeg", "ffmpeg-aarch64-apple-darwin", "ffprobe", "model.bin"]);
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, "ffmpeg")).toString(), "mac-ffmpeg", "a stale resourceDir file is replaced by the verified one");
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, `ffmpeg-${MAC}`)).toString(), "a file in resourceDir that looks like a stage name");
  assert.equal(kit.fs.readFileSync(path.join(runtimeDirAt, "model.bin")).toString(), "model");
  assert.deepEqual(listUnder(kit.files, "src-tauri/binaries"), ["keep.txt", "stage-sidecar.json"], "the stale stageDir names are wiped");
  assert.deepEqual([...result.receipt.wiped].sort(), [`src-tauri/binaries/ffmpeg-${MAC}`, `src-tauri/binaries/ffprobe-${MAC}`]);
  // A resourceDir that is the stage directory itself: the produced tools survive and only the triple-named leftovers go.
  const same = runtimeKit({ rows: MAC_ROWS, members: MAC_MEMBERS, seeded });
  const merged = await same.stage({ sidecars: [runtimeEntry({ resourceDir: "src-tauri/binaries", sources: same.sources })], wipeStageDir: true });
  assert.deepEqual(listUnder(same.files, "src-tauri/binaries"), ["ffmpeg", "ffprobe", "keep.txt", "stage-sidecar.json"]);
  assert.deepEqual([...merged.receipt.wiped].sort(), [`src-tauri/binaries/ffmpeg-${MAC}`, `src-tauri/binaries/ffprobe-${MAC}`]);
});

test("--check lists the runtime paths of an ffmpeg resourceDir entry, flat on every target, and no externalBin", () => {
  const entry = runtimeEntry({ variants: { ffmpeg: "export-ass" } });
  const plan = (target, manifest = [entry]) => planSidecarStaging({ manifest, target, env: {} });
  const mac = plan(MAC).entries[0];
  assert.deepEqual([mac.outputs, mac.externalBin], [[`${RUNTIME}/ffmpeg`, `${RUNTIME}/ffprobe`], false]);
  assert.deepEqual(plan(WIN).entries[0].outputs, [`${RUNTIME}/ffmpeg.exe`, `${RUNTIME}/ffprobe.exe`]);
  assert.deepEqual(plan(UNIVERSAL).entries[0].outputs, [`${RUNTIME}/ffmpeg`, `${RUNTIME}/ffprobe`]);
  assert.deepEqual(plan(MAC, [{ ...entry, tools: ["ffmpeg"] }]).entries[0].outputs, [`${RUNTIME}/ffmpeg`]);
  const mixed = plan(MAC, [{ kind: "cargo-bin", package: "eng" }, entry, { kind: "ffmpeg", notices: NOTICES, catalog: "c/artifacts.json", tools: ["ffprobe"] }]);
  assert.deepEqual(mixed.entries.map((item) => [item.outputs, item.externalBin]), [[[`src-tauri/binaries/eng-${MAC}`], true], [[`${RUNTIME}/ffmpeg`, `${RUNTIME}/ffprobe`], false], [[`src-tauri/binaries/ffprobe-${MAC}`], true]]);
  const lines = formatSidecarPlan(plan(MAC));
  assert.ok(lines.some((line) => line.includes(`${RUNTIME}/ffmpeg, ${RUNTIME}/ffprobe`) && !line.includes("(externalBin)")), lines.join("\n"));
  assert.equal(plan(MAC, [{ ...entry, platforms: ["win"] }]).entries[0].skipped, true);
});

// ---- review fixes: warnings reach the CLI, thin rows take one slice, staged bytes are re-verified, runtime exec bit, companion clashes

const COMMAND = fileURLToPath(new URL("./stage-sidecar-command.mjs", import.meta.url));

test("the stage-sidecar command prints the never-run warning and the 'not staging' notice on stderr, and --json stays clean on stdout", () => {
  const app = mkdtempSync(path.join(os.tmpdir(), "rk-stage-cli-"));
  try {
    const bytes = Buffer.from("pdfium for the cli test");
    const row = { id: "pdfium-cli-1-windows-x64", library: "pdfium", version: "1", os: "windows", arch: "x86_64", status: "pinned", linkage: "dynamic-library", artifactFileName: "pdfium.dll", sha256: sha(bytes), licenseSpdx: "BSD-3-Clause", noticeFiles: ["third-party/pdfium-BSD.txt"], hostProof: "never-run" };
    const planned = { id: "pdfium-cli-1-windows-x64-extra", library: "pdfium", version: "1", os: "windows", arch: "x86_64", status: "planned", linkage: "dynamic-library", artifactFileName: "pdfium-extra.dll" };
    for (const dir of ["catalog", "third-party", "builds"]) mkdirSync(path.join(app, dir));
    writeFileSync(path.join(app, "catalog", "native-libraries.json"), JSON.stringify({ libraries: [row, planned] }));
    writeFileSync(path.join(app, "third-party", "pdfium-BSD.txt"), "notice");
    writeFileSync(path.join(app, "builds", "pdfium.dll"), bytes);
    writeFileSync(path.join(app, "right-release.config.mjs"), `export default { sidecars: [{ kind: "native-library", library: "pdfium", version: "1", catalog: "catalog/native-libraries.json", source: { dir: "builds" } }] };\n`);
    const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("TAURI_ENV_")));
    const run = (...extra) => spawnSync(process.execPath, [COMMAND, "--config", path.join(app, "right-release.config.mjs"), "--target", WIN, ...extra], { encoding: "utf8", env });
    const plain = run();
    assert.equal(plain.status, 0, plain.stderr);
    assert.match(plain.stderr, /warning: pdfium-cli-1-windows-x64 has hostProof never-run/);
    assert.match(plain.stderr, /not staging pdfium-cli-1-windows-x64-extra \(planned\)/);
    assert.doesNotMatch(plain.stdout, /warning|not staging/, "stdout keeps the staged-file list");
    assert.match(plain.stdout, /staged src-tauri\/resources\/runtime\/pdfium\.dll/);
    const json = run("--json");
    assert.equal(json.status, 0, json.stderr);
    assert.equal(JSON.parse(json.stdout).files[0].hostProof, "never-run", "--json stdout is one JSON document");
    assert.match(json.stderr, /hostProof never-run/);
  } finally {
    rmSync(app, { recursive: true, force: true });
  }
});

test("stageSidecars is quiet by default, and deps.warn takes the warnings while deps.log keeps the progress lines", async () => {
  const solo = winOrtFixture();
  const warnings = [];
  await solo.stage([ORT_WIN_ENTRY], { warn: (line) => warnings.push(line) });
  assert.equal(warnings.length, 2);
  assert.ok(warnings[0].includes(ORT_WIN_ID) && warnings[1].includes(DML_ID), warnings.join("\n"));
  assert.deepEqual(solo.messages.filter((line) => /warning/.test(line)), [], "a separate warn handler gets the warnings, not log");
  // With no handler at all the API writes nothing to the console: the CLI opts in.
  const quiet = winOrtFixture();
  const written = [];
  const originals = [console.error, console.warn, console.log, process.stderr.write];
  console.error = console.warn = console.log = (...args) => written.push(args.join(" "));
  process.stderr.write = (chunk) => { written.push(String(chunk)); return true; };
  try { await quiet.stage([ORT_WIN_ENTRY], { log: undefined }); } finally { [console.error, console.warn, console.log, process.stderr.write] = originals; }
  assert.deepEqual(written, []);
});

test("a thin native-library row refuses a universal Mach-O (a good slice beside a foreign one), from source.dir and in place of the staged-file shortcut", async () => {
  const good = machoCpu(CPU_ARM64, Buffer.from("good arm64 payload"), null);
  const foreign = machoCpu(CPU_X86_64, Buffer.from("foreign x86_64 payload"), null);
  const fat = fatOf([[CPU_ARM64, good], [CPU_X86_64, foreign]]);
  assert.equal(contentOf(fat, "aarch64"), contentOf(good), "premise: the fat file's arm64 slice is the pinned thin file");
  const name = "libwhisper.1.9.3.dylib";
  const row = rowOf("whisper-cpp-1.9.3-macos-arm64", "whisper-cpp", "1.9.3", "macos", "aarch64", good, { artifactFileName: name, contentSha256: contentOf(good) });
  const kitOf = (files) => nativeKit({ rows: [row], files, target: MAC });
  const sliceRefusal = (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && /universal Mach-O with 2 slices \(aarch64, x86_64\)/.test(error.message);

  const fromSource = kitOf({ [sourceAt(name)]: fat });
  await assert.rejects(fromSource.stage([WHISPER_ENTRY]), sliceRefusal);
  assert.deepEqual(fromSource.written(), [], "nothing is staged from a fat file");

  // The staged-file shortcut applies the same check: a fat staged file is replaced from a good source, never kept.
  const shortcut = kitOf({ [runtimeAt(name)]: fat, [sourceAt(name)]: good });
  const result = await shortcut.stage([WHISPER_ENTRY]);
  assert.deepEqual(shortcut.fs.readFileSync(runtimeAt(name)), good, "the fat staged file did not pass as already staged");
  assert.ok(shortcut.log.copies.some((copy) => copy.startsWith(runtimeAt(name))));
  assert.equal(result.files[0].sha256, sha(good));
  assert.equal(shortcut.messages.some((line) => /already staged/.test(line)), false);
  // With a fat source as well there is nothing good to stage.
  await assert.rejects(kitOf({ [runtimeAt(name)]: fat, [sourceAt(name)]: fat }).stage([WHISPER_ENTRY]), sliceRefusal);
  // A thin file is still accepted, as is a re-signed copy of it.
  assert.equal((await kitOf({ [sourceAt(name)]: good }).stage([WHISPER_ENTRY])).files[0].sha256, sha(good));
});

test("native-library holds the staged bytes to the row: a file that changes between its verification and the copy is refused and removed", async () => {
  const swapAfterFirstRead = (kit, file, bytes) => {
    let done = false;
    return { ...kit.fs, readFileSync: (target) => { const read = kit.fs.readFileSync(target); if (!done && path.resolve(target) === path.resolve(file)) { done = true; kit.files.set(path.resolve(file), bytes); } return read; } };
  };
  const name = "libwhisper.1.9.3.dylib";
  const changed = Buffer.from(bytesOfLibrary(name)); changed[0x1000] ^= 1;
  const local = whisperGgmlFixture();
  await assert.rejects(local.stage([WHISPER_ENTRY], { fs: swapAfterFirstRead(local, sourceAt(name), changed) }), (error) => error.code === "STAGE_SIDECAR_HASH_MISMATCH" && error.details.expected === contentOf(bytesOfLibrary(name)) && error.details.actual === contentOf(changed) && /changed after it was verified/.test(error.message));
  assert.equal(local.fs.existsSync(runtimeAt(name)), false, "the unverified copy is removed");
  assert.deepEqual(local.written(), [], "and no notice or receipt is written");
  // The same window exists for a file unpacked from the pinned archive.
  const archived = winOrtFixture();
  const unpacked = path.join(path.resolve("/rk-native-scratch-1"), ...ORT_MEMBER.split("/"));
  await assert.rejects(archived.stage([{ ...ORT_WIN_ENTRY, companions: false }], { fs: swapAfterFirstRead(archived, unpacked, peOf(0x8664, 0x99)) }), { code: "STAGE_SIDECAR_HASH_MISMATCH", message: /changed after it was verified/ });
  assert.equal(archived.fs.existsSync(runtimeAt("onnxruntime.dll")), false);
  // Unchanged files still stage.
  assert.equal((await whisperGgmlFixture().stage([WHISPER_ENTRY])).files.length, 1);
});

test("ffmpeg resourceDir: an already-staged runtime file that matches its row is made executable (0o755) instead of left at its old mode", async () => {
  const seeded = { [path.join(runtimeDirAt, "ffmpeg")]: "mac-ffmpeg", [path.join(runtimeDirAt, "ffprobe")]: "mac-ffprobe" };
  const kit = runtimeKit({ rows: MAC_ROWS, members: MAC_MEMBERS, seeded });
  const result = await kit.stage([runtimeEntry({ sources: kit.sources })]);
  assert.deepEqual(result.files.map((file) => file.path), [`${RUNTIME}/ffmpeg`, `${RUNTIME}/ffprobe`]);
  assert.deepEqual(kit.log.copies.filter((copy) => copy.startsWith(runtimeDirAt)), [], "nothing is rewritten: both files already match");
  for (const tool of ["ffmpeg", "ffprobe"]) assert.ok(kit.log.chmods.some(([file, mode]) => file === path.join(runtimeDirAt, tool) && mode === 0o755), `${tool} is chmod 0755 on the already-staged path`);
  const win = runtimeKit({ target: WIN, rows: [["ffmpeg", "decode", "windows", "x86_64", "win-ffmpeg"], ["ffprobe", "decode", "windows", "x86_64", "win-ffprobe"]], members: { "ffmpeg.exe": "win-ffmpeg", "ffprobe.exe": "win-ffprobe" }, seeded: { [path.join(runtimeDirAt, "ffmpeg.exe")]: "win-ffmpeg", [path.join(runtimeDirAt, "ffprobe.exe")]: "win-ffprobe" } });
  await win.stage([runtimeEntry({ sources: win.sources })]);
  assert.deepEqual(win.log.chmods, [], "no exec bit on Windows");
});

test("the alias stages its DirectML companion by default: another entry that writes DirectML.dll into the same directory is refused before anything is written", async () => {
  const dml = `${RUNTIME}/DirectML.dll`;
  const alias = { kind: "onnxruntime", version: "1.24.4", ...NATIVE_CATALOG_ENTRY };
  const refused = async (manifest, other) => {
    const kit = winOrtFixture();
    kit.files.set(at("vendor", "DirectML.dll"), Buffer.from("vendored directml"));
    await assert.rejects(kit.stage(manifest), (error) => error.code === "STAGE_SIDECAR_CONFIG_INVALID"
      && error.message.includes(`sidecars[${manifest.indexOf(alias)}] (onnxruntime)`) && error.message.includes(other) && error.message.includes("companions: false") && error.message.includes("DirectML.dll") && error.details.errors.length === 1, JSON.stringify(manifest));
    assert.deepEqual([kit.written(), kit.fetched], [[], []], "no file of either entry was written or downloaded");
  };
  await refused([{ kind: "resource", from: "vendor/DirectML.dll", to: dml }, alias], "sidecars[0] (resource)");
  await refused([alias, { kind: "remote", name: "dml", url: "https://example.test/DirectML.dll", sha256: sha("d"), dest: dml }], "sidecars[1] (remote)");
  await refused([alias, { kind: "native-library", library: "directml", version: "1.15.4", ...NATIVE_CATALOG_ENTRY }], "sidecars[1] (native-library)");
  // The remedy works: without the companion the other entry owns the file and both stage.
  const kit = winOrtFixture();
  kit.files.set(at("vendor", "DirectML.dll"), Buffer.from("vendored directml"));
  const result = await kit.stage([{ kind: "resource", from: "vendor/DirectML.dll", to: dml }, { ...alias, companions: false }]);
  assert.deepEqual(result.files.map((file) => file.name), ["DirectML.dll", "onnxruntime.dll"]);
  // A different directory does not clash.
  const apart = winOrtFixture();
  apart.files.set(at("vendor", "DirectML.dll"), Buffer.from("vendored directml"));
  assert.equal((await apart.stage([{ kind: "resource", from: "vendor/DirectML.dll", to: "src-tauri/resources/other/DirectML.dll" }, alias])).files.length, 3);
});
