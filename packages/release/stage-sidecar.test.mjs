import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import path from "node:path";
import test from "node:test";
import {
  StageSidecarError, builtBinaryCandidates, parseTargetTriple, resolveBuildMode, stageSidecars, validateSidecarManifest, validateStageSidecarReceipt,
} from "./stage-sidecar.mjs";

const sha = (value) => createHash("sha256").update(value).digest("hex");
const APP = path.resolve("/rk-fake-app");
const at = (...parts) => path.join(APP, ...parts);
const MAC = "aarch64-apple-darwin";
const WIN = "x86_64-pc-windows-msvc";

function fakeFs(initial = {}) {
  const files = new Map(Object.entries(initial).map(([file, bytes]) => [path.resolve(file), Buffer.from(bytes)]));
  const log = { copies: [], chmods: [], writes: [] };
  const fs = {
    existsSync: (file) => files.has(path.resolve(file)),
    mkdirSync: () => {},
    readFileSync: (file) => {
      if (!files.has(path.resolve(file))) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" });
      return files.get(path.resolve(file));
    },
    writeFileSync: (file, bytes) => { log.writes.push(path.resolve(file)); files.set(path.resolve(file), Buffer.from(bytes)); },
    copyFileSync: (from, to) => { log.copies.push(path.resolve(to)); files.set(path.resolve(to), files.get(path.resolve(from))); },
    renameSync: (from, to) => { files.set(path.resolve(to), files.get(path.resolve(from))); files.delete(path.resolve(from)); },
    chmodSync: (file, mode) => { log.chmods.push([path.resolve(file), mode]); },
    statSync: (file) => ({ isFile: () => files.has(path.resolve(file)), size: files.get(path.resolve(file))?.length ?? 0 }),
  };
  return { fs, files, log };
}

const stream = (bin, executable) => `${JSON.stringify({ reason: "compiler-artifact", executable, target: { name: bin, kind: ["bin"] } })}\n`;

/** A fake `rightkit` runner: records calls; `onBuild` returns { stdout } and may seed the fake fs. */
function fakeRun({ onBuild, targetDir = path.resolve("/rk-fake-target") } = {}) {
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (command === "rightkit" && args[0] === "cargo" && args[1] === "metadata") return { status: 0, stdout: JSON.stringify({ target_directory: targetDir }), stderr: "" };
    if (command === "rightkit" && args[0] === "cargo" && args[1] === "build") return { status: 0, stderr: "", ...onBuild(args) };
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

test("builtBinaryCandidates prefers the explicit-target layout, then the plain one", () => {
  const dirs = builtBinaryCandidates({ targetDir: "/t", triple: MAC, profile: "dev", bin: "eng", windows: false });
  assert.deepEqual(dirs, [path.join("/t", MAC, "debug", "eng"), path.join("/t", "debug", "eng")]);
  assert.equal(builtBinaryCandidates({ targetDir: "/t", triple: WIN, profile: "release", bin: "eng", windows: true })[0], path.join("/t", WIN, "release", "eng.exe"));
});

test("cargo-bin builds through rightkit cargo, stages <name>-<triple>, writes a receipt", async () => {
  const { fs, files, log } = fakeFs();
  const exe = path.resolve("/rk-fake-target", MAC, "release", "mailright-engine");
  const { run, calls } = fakeRun({ onBuild: () => { files.set(exe, Buffer.from("engine-bytes")); return { stdout: stream("mailright-engine", exe) }; } });
  const result = await stageSidecars({ appRoot: APP, target: MAC, manifest: [{ kind: "cargo-bin", package: "mailright-engine", features: ["a", "pkg/b"] }], deps: deps({ fs, run }) });
  const staged = at("src-tauri", "binaries", `mailright-engine-${MAC}`);
  assert.equal(fs.readFileSync(staged).toString(), "engine-bytes");
  assert.deepEqual(result.externalBinNames, ["mailright-engine"]);
  assert.ok(calls.every((call) => call.command === "rightkit" && call.args[0] === "cargo"), "never bare cargo");
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
    if (command === "rightkit" && args[0] === "ffmpeg-stage") files.set(at("src-tauri", "binaries", `ffmpeg-${MAC}`), Buffer.from(stagedBytes));
    return { status: 0, stdout: "", stderr: "" };
  };
  return { fs, log, run, crateDir, catalog: { schemaVersion: 1, artifacts: [row] } };
}

const ffmpegManifest = [{ kind: "ffmpeg", tools: ["ffmpeg"], catalog: "c/artifacts.json", notices: { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.test/source" }, stage: { cmd: "rightkit", args: ["ffmpeg-stage"] } }];

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

test("the dev mode builds with the dev profile through rightkit cargo", async () => {
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
    assert.equal(calls.some((call) => call.command === "rightkit" && call.args[1] === "metadata"), "crate" in catalog);
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
    if (command === "rightkit" && args[1] === "build") {
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
  const targets = seen.filter((call) => call.command === "rightkit").map((call) => call.args[call.args.indexOf("--target") + 1]);
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
    if (command !== "rightkit") return undefined;
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

test("universal: command output and machO resources must already carry both archs; ffmpeg and onnxruntime are refused", async () => {
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
  assert.throws(() => validateSidecarManifest([{ kind: "onnxruntime", version: "1.24.4" }], { target: UNIVERSAL }), /no universal row/);
  assert.throws(() => validateSidecarManifest([{ kind: "ffmpeg", notices: { dir: "n", sourceOfferContact: "a@b.co" } }], { target: UNIVERSAL }), /no universal row/);
});
