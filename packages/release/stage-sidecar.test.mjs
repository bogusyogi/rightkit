import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import path from "node:path";
import test from "node:test";
import {
  StageSidecarError, builtBinaryCandidates, isNestedBrokerContext, parseTargetTriple, planSidecarStaging, resolveBuildMode, stageSidecars, validateSidecarManifest, validateStageSidecarReceipt,
} from "./stage-sidecar.mjs";
import { formatSidecarPlan, nodeHostTriple, parseStageSidecarArgs } from "./stage-sidecar-command.mjs";

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
    if (command === "cmake" && args[0] === "--build" && produce) files.set(path.join(BUILD_DIR, "bin", `${outputName}${target === WIN ? ".exe" : ""}`), Buffer.from("whisper-bytes"));
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

test("prebuilt copies `from`, defaults to raw-sha256 off macOS, and refuses a universal target", async () => {
  const fixture = prebuiltFixture({ target: WIN, key: "windows-x86_64", algorithm: "raw-sha256", stage: false });
  fixture.files.set(at("build", "sd-server.exe"), Buffer.from("sd-bytes"));
  const seen = [];
  const manifest = [{ ...PREBUILT, from: "build/sd-server.exe", manifest: { ...PREBUILT.manifest, approvalAlgorithm: undefined } }];
  const result = await stageSidecars({ appRoot: APP, target: WIN, manifest, deps: deps({ fs: fixture.fs, approvalDigest: (file, algorithm) => { seen.push(algorithm); return hex("c"); } }) });
  assert.deepEqual(seen, ["raw-sha256"]);
  assert.equal(fixture.fs.readFileSync(at("src-tauri", "binaries", `sd-server-${WIN}.exe`)).toString(), "sd-bytes");
  assert.equal(result.files[0].changed, true);
  assert.throws(() => validateSidecarManifest([PREBUILT], { target: UNIVERSAL }), /no universal manifest entry/);
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
  for (const bogus of [{ extract: "a/b.exe" }, { memberSha256: hex("a"), extract: undefined }, { executable: "yes" }, { memberSha256: "z" }]) {
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
    if (command === "rightkit" && args[0] === "ffmpeg-stage") {
      stageCalls.push(args);
      const [, triple, stageDir] = args;
      files.set(path.resolve(APP, stageDir, `ffmpeg-${triple}`), Buffer.from(triple.startsWith("aarch64") ? "arm-ffmpeg" : "x86-ffmpeg"));
    }
    if (command === "lipo" && args[0] === "-create") files.set(args[args.indexOf("-output") + 1], Buffer.from("fat-ffmpeg"));
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  const manifest = [{ kind: "ffmpeg", tools: ["ffmpeg"], catalog: "c/artifacts.json", notices: NOTICES, stage: { cmd: "rightkit", args: ["ffmpeg-stage", "{triple}", "{stageDir}"] } }];
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
  const thinRun = (command, args) => (command === "lipo" && args[0] === "-archs" ? { status: 0, stdout: "arm64\n" } : command === "rightkit" ? (thin.files.set(path.resolve(APP, args[2], `ffmpeg-${args[1]}`), Buffer.from(args[1].startsWith("aarch64") ? "arm-ffmpeg" : "x86-ffmpeg")), { status: 0 }) : command === "lipo" ? (thin.files.set(args[args.indexOf("-output") + 1], Buffer.from("x")), { status: 0 }) : { status: 0 });
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
