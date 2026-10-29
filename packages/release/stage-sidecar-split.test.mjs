// Item 7 row 7.05a (decision 2 Q3): every sidecar kind stages from a source split: caches, archives (`cacheDir`), slices and catalog downloads
// resolve under `cacheRoot` (the checkout's existing `.right-release`); only final outputs and the receipt land in the snapshot, and the
// attribution diff (the files under the snapshot) equals what the step reports plus its receipt. No network: catalogs, archives and tool
// outputs are injected. Fixtures are shaped like the consumers' configs: genright (macOS onnxruntime and ffmpeg, Windows ffmpeg) and
// scraperight (Windows onnxruntime with licenseDir). Real file systems in temp roots, never the real managed root.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { reportedPaths } from "./stage-ledger.mjs";
import { stageSidecars } from "./stage-sidecar.mjs";

const sha = (v) => createHash("sha256").update(v).digest("hex");
const MAC = "aarch64-apple-darwin";
const MAC_X86 = "x86_64-apple-darwin";
const WIN = "x86_64-pc-windows-msvc";

function roots(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-split-kinds-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const snapshot = path.join(dir, "snapshot", "app"); const checkout = path.join(dir, "checkout", "app");
  for (const d of [snapshot, checkout]) fs.mkdirSync(d, { recursive: true });
  return { dir, snapshot, checkout };
}

/** Every file under `dir`, relative and posix. */
function walk(dir, rel = "") {
  const out = [];
  for (const d of fs.readdirSync(path.join(dir, rel), { withFileTypes: true })) {
    const r = rel ? `${rel}/${d.name}` : d.name;
    if (d.isDirectory()) out.push(...walk(dir, r)); else out.push(r);
  }
  return out.sort();
}

/** The attribution diff of a step: what is under the snapshot is exactly what it reported plus its receipt. */
function assertAttributed(snapshot, result, label) {
  const receipt = path.relative(snapshot, result.receiptPath).split(path.sep).join("/");
  assert.deepEqual(walk(snapshot), [...new Set([...reportedPaths(result), receipt])].sort(), `${label}: the snapshot holds exactly the reported outputs and the receipt`);
}

const base = (extra = {}) => ({ now: () => "2026-10-29T00:00:00.000Z", pid: 7, log: () => {}, env: {}, ...extra });

// ---- onnxruntime: genright (macOS) and scraperight (Windows) ---------------------------------------------------------------------------------

/**
 * A native catalog served from memory: one row per `rows` entry, each with its own pinned archive; a row's `companions` name other rows of the
 * same catalog (the real Windows onnxruntime row lists its DirectML companion). Nothing touches the network or the host.
 */
function nativeFixture(dir, { target, rows, version }) {
  const crateDir = path.join(dir, "crate"); fs.mkdirSync(path.join(crateDir, "third-party"), { recursive: true });
  fs.writeFileSync(path.join(crateDir, "third-party", "NOTICE.txt"), "licence text");
  const os_ = target.includes("windows") ? "windows" : "macos"; const arch = target.startsWith("aarch64") ? "aarch64" : "x86_64";
  const made = rows.map((r) => {
    const bytes = Buffer.from(`${r.library}-${target}-bytes`); const archive = Buffer.from(`${r.library}-${target}-archive`);
    const id = `${r.library}-${r.version ?? version}-${os_}-${arch}`;
    return { ...r, id, bytes, archive, row: { id, library: r.library, version: r.version ?? version, os: os_, arch, status: "approved", artifactFileName: r.fileName, sha256: sha(bytes), archive: { url: r.url, sha256: sha(archive), member: r.member ?? r.fileName }, licenseSpdx: "MIT", noticeFiles: ["third-party/NOTICE.txt"], ...(r.companions ? { companions: r.companions.map((c) => `${c}-${r.version ?? version}-${os_}-${arch}`) } : {}) } };
  });
  const catalog = { libraries: made.map((m) => m.row) };
  const counts = { fetched: 0, extracted: 0 };
  const byUrl = new Map(made.map((m) => [m.url, m])); const byFile = new Map(made.map((m) => [m.fileName, m]));
  const deps = base({
    loadNativeCatalog: () => ({ catalog, crateDir }),
    fetchBytes: (u) => { counts.fetched += 1; const m = byUrl.get(u); assert.ok(m, `an unexpected download ${u}`); return m.archive; },
    extractMember: ({ destDir, fileName: name }) => { counts.extracted += 1; const m = byFile.get(path.basename(name)); assert.ok(m, `an unexpected member ${name}`); const out = path.join(destDir, path.basename(name)); fs.writeFileSync(out, m.bytes); return out; },
  });
  return { deps, counts, made, bytes: made[0].bytes, archive: made[0].archive, catalog };
}

test("[7.05a onnxruntime / genright macOS] the library and its notices land in the snapshot, the downloaded archive in the checkout's .right-release, nothing else under the snapshot; a second run needs no fetch", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const fx = nativeFixture(dir, { target: MAC, version: "1.30.0", rows: [{ library: "onnxruntime", fileName: "libonnxruntime.1.30.0.dylib", url: "https://example.test/ort-1.30.0.tgz" }] });
  // genright's real entry (right-release.config.mjs): macOS only, renamed with `as`, catalog from the rightkit-ffmpeg crate.
  const manifest = [{ kind: "onnxruntime", version: "1.30.0", catalog: { crate: "rightkit-ffmpeg" }, as: "libonnxruntime.dylib", platforms: ["mac"] }];
  const stage = () => stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest, target: MAC, deps: fx.deps });
  const result = await stage();
  assert.equal(fs.readFileSync(path.join(snapshot, "src-tauri/resources/runtime/libonnxruntime.dylib")).toString(), fx.bytes.toString());
  assert.equal(fs.readFileSync(path.join(snapshot, "src-tauri/resources/runtime/LICENSES/NOTICE.txt"), "utf8"), "licence text");
  assertAttributed(snapshot, result, "onnxruntime mac");
  assert.ok(fs.existsSync(path.join(checkout, ".right-release", ".cache", "onnxruntime", "ort-1.30.0.tgz")), "the archive cache is the checkout's");
  assert.ok(!fs.existsSync(path.join(snapshot, ".cache")) && !fs.existsSync(path.join(snapshot, ".right-release")));
  assert.equal(fx.counts.fetched, 1);
  // The next sync removed every ledger entry: the snapshot is empty again; re-staging reads the cached archive and fetches nothing.
  fs.rmSync(path.join(snapshot, "src-tauri"), { recursive: true, force: true });
  const again = await stage();
  assert.equal(fx.counts.fetched, 1, "a second run reports the cache and does not fetch");
  assertAttributed(snapshot, again, "onnxruntime mac, second run");
});

test("[7.05a onnxruntime / scraperight Windows] the DLL, its DirectML companion and their notices (licenseDir) land in the snapshot, both archives in the checkout's cache", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  // scraperight's real entry: Windows only, notices flat in runtime/ (licenseDir), companions as the catalog row lists them (DirectML beside onnxruntime).
  const fx = nativeFixture(dir, { target: WIN, version: "1.24.4", rows: [
    { library: "onnxruntime", fileName: "onnxruntime.dll", url: "https://example.test/ort-win-1.24.4.zip", companions: ["directml"] },
    { library: "directml", fileName: "DirectML.dll", url: "https://example.test/directml-1.15.zip", version: "1.24.4" },
  ] });
  const manifest = [{ kind: "onnxruntime", version: "1.24.4", catalog: { crate: "rightkit-ffmpeg" }, licenseDir: "src-tauri/resources/runtime", platforms: ["win"] }];
  const result = await stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest, target: WIN, deps: fx.deps });
  for (const m of fx.made) assert.equal(fs.readFileSync(path.join(snapshot, "src-tauri/resources/runtime", m.fileName)).toString(), m.bytes.toString());
  assert.ok(result.files.some((f) => f.companionOf), "the DirectML companion is reported as a companion of the onnxruntime row");
  assertAttributed(snapshot, result, "onnxruntime win");
  for (const name of ["ort-win-1.24.4.zip", "directml-1.15.zip"]) assert.ok(fs.existsSync(path.join(checkout, ".right-release", ".cache", "onnxruntime", name)), `${name} is cached in the checkout`);
  assert.ok(!fs.existsSync(path.join(snapshot, ".cache")));
});

test("[7.05a native-library] a catalog library (pdfium) stages like onnxruntime: output and notices in the snapshot, its cacheDir under the checkout's .right-release", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const fx = nativeFixture(dir, { target: MAC, version: "7000", rows: [{ library: "pdfium", fileName: "libpdfium.dylib", url: "https://example.test/pdfium-mac.tgz" }] });
  const manifest = [{ kind: "native-library", library: "pdfium", version: "7000", catalog: { crate: "rightkit-ffmpeg" }, resourceDir: "src-tauri/resources/runtime", cacheDir: ".cache/pdfium" }];
  const result = await stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest, target: MAC, deps: fx.deps });
  assert.equal(fs.readFileSync(path.join(snapshot, "src-tauri/resources/runtime/libpdfium.dylib")).toString(), fx.bytes.toString());
  assertAttributed(snapshot, result, "native-library");
  assert.ok(fs.existsSync(path.join(checkout, ".right-release", ".cache", "pdfium", "pdfium-mac.tgz")));
  assert.ok(!fs.existsSync(path.join(snapshot, ".cache")));
});

test("[7.05a negative] a cache file whose digest differs from the catalog's pin is re-fetched, never used; a source dir or a catalog outside the snapshot is refused", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const fx = nativeFixture(dir, { target: MAC, version: "1.30.0", rows: [{ library: "onnxruntime", fileName: "libonnxruntime.1.30.0.dylib", url: "https://example.test/ort-1.30.0.tgz" }] });
  const manifest = [{ kind: "onnxruntime", version: "1.30.0", catalog: { crate: "rightkit-ffmpeg" }, as: "libonnxruntime.dylib" }];
  // A poisoned cache entry in the checkout (its digest is not the catalog's pin): it is replaced by a fresh download.
  const cached = path.join(checkout, ".right-release", ".cache", "onnxruntime", "ort-1.30.0.tgz");
  fs.mkdirSync(path.dirname(cached), { recursive: true }); fs.writeFileSync(cached, "POISONED");
  await stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest, target: MAC, deps: fx.deps });
  assert.equal(fx.counts.fetched, 1, "the poisoned cache entry was not used");
  assert.equal(fs.readFileSync(cached).toString(), fx.archive.toString(), "and was replaced by the verified archive");
  // A native source dir that resolves into the checkout (an absolute path) is refused: checkout bytes enter only as digest-pinned cache entries.
  const outside = path.join(checkout, "vendor"); fs.mkdirSync(outside, { recursive: true });
  await assert.rejects(stageSidecars({ appRoot: snapshot, cacheRoot: checkout, target: MAC, deps: fx.deps,
    manifest: [{ kind: "native-library", library: "onnxruntime", version: "1.30.0", catalog: { crate: "rightkit-ffmpeg" }, resourceDir: "src-tauri/resources/runtime2", source: { dir: outside } }] }), { code: "STAGE_SIDECAR_SOURCE_OUTSIDE_SNAPSHOT" });
  // A catalog file by absolute path outside the snapshot is refused too.
  await assert.rejects(stageSidecars({ appRoot: snapshot, cacheRoot: checkout, target: MAC, deps: base(), manifest: [{ kind: "onnxruntime", version: "1.30.0", catalog: path.join(checkout, "catalog.json") }] }), { code: "STAGE_SIDECAR_SOURCE_OUTSIDE_SNAPSHOT" });
});

// ---- ffmpeg: genright macOS (thin and universal) and Windows -------------------------------------------------------------------------------

function ffmpegFixture(dir, { rows }) {
  const crateDir = path.join(dir, "ffmpeg-crate"); fs.mkdirSync(path.join(crateDir, "notices"), { recursive: true });
  fs.writeFileSync(path.join(crateDir, "notices", "LGPL-2.1.txt"), "LGPL text");
  const artifacts = rows.map((r) => ({
    id: r.id, tool: r.tool, variant: r.variant, os: r.os, arch: r.arch, status: "approved", version: "7.1", contentSha256: sha(r.bytes), sourceSha256: sha("src"),
    attribution: { noticeFiles: ["notices/LGPL-2.1.txt"], sourceUrl: "https://ffmpeg.org/releases/ffmpeg-7.1.tar.xz", licenseSpdx: "LGPL-2.1-or-later", buildProvenance: "recipe" },
  }));
  return { catalog: { schemaVersion: 1, artifacts }, crateDir };
}
const NOTICES = { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.test/source" };

test("[7.05a ffmpeg / genright macOS thin] the verified ffmpeg and its notices land in the snapshot; the stage command's own cargo output goes to the checkout's target", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const bytes = Buffer.from("ffmpeg-decode-macos");
  const fx = ffmpegFixture(dir, { rows: [{ id: "ffmpeg-decode-macos-arm64", tool: "ffmpeg", variant: "decode", os: "macos", arch: "aarch64", bytes }] });
  const calls = [];
  const targetDir = path.join(dir, "checkout-target");
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (command === "rustright" && args[0] === "cargo" && args[1] === "metadata") return { status: 0, stdout: JSON.stringify({ target_directory: targetDir }), stderr: "" };
    if (command === "rustright" && args[0] === "cargo" && args[1] === "run") { fs.mkdirSync(path.join(options.cwd, "src-tauri", "binaries"), { recursive: true }); fs.writeFileSync(path.join(options.cwd, "src-tauri", "binaries", `ffmpeg-${MAC}`), bytes); }
    return { status: 0, stdout: "", stderr: "" };
  };
  const result = await stageSidecars({
    appRoot: snapshot, cacheRoot: checkout, target: MAC,
    manifest: [{ kind: "ffmpeg", variant: "decode", tools: ["ffmpeg"], catalog: { crate: "rightkit-ffmpeg" }, notices: NOTICES, stage: { cmd: "rustright", args: ["cargo", "run", "-p", "studio", "--example", "stage-ffmpeg"] } }],
    deps: base({ run, loadMediaCatalog: () => ({ catalog: fx.catalog, crateDir: fx.crateDir, crateVersion: "0.9.0" }) }),
  });
  assert.equal(result.files[0].path, `src-tauri/binaries/ffmpeg-${MAC}`);
  assertAttributed(snapshot, result, "ffmpeg mac thin");
  const stageRun = calls.find((c) => c.args[1] === "run");
  assert.equal(stageRun.options.cwd, snapshot, "the stage command runs in the snapshot");
  assert.equal(stageRun.options.env.CARGO_TARGET_DIR, targetDir, "with Cargo's output pinned to the checkout's target directory");
  assert.ok(!fs.existsSync(path.join(snapshot, "target")));
});

test("[7.05a ffmpeg / genright Windows] a thin windows ffmpeg (export-ass-zimg) stages as ffmpeg-<triple>.exe with its notices; the snapshot holds only reported outputs", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const bytes = Buffer.from("ffmpeg-win-bytes");
  const fx = ffmpegFixture(dir, { rows: [{ id: "ffmpeg-export-ass-zimg-windows-x64", tool: "ffmpeg", variant: "export-ass-zimg", os: "windows", arch: "x86_64", bytes }] });
  const run = (command, args, options) => {
    if (args[1] === "run") { fs.mkdirSync(path.join(options.cwd, "src-tauri", "binaries"), { recursive: true }); fs.writeFileSync(path.join(options.cwd, "src-tauri", "binaries", `ffmpeg-${WIN}.exe`), bytes); }
    return { status: 0, stdout: JSON.stringify({ target_directory: path.join(dir, "t") }), stderr: "" };
  };
  const result = await stageSidecars({
    appRoot: snapshot, cacheRoot: checkout, target: WIN,
    manifest: [{ kind: "ffmpeg", variant: "export-ass-zimg", tools: ["ffmpeg"], catalog: { crate: "rightkit-ffmpeg" }, notices: NOTICES, stage: { cmd: "rustright", args: ["cargo", "run", "-p", "studio", "--example", "stage-ffmpeg"] }, platforms: ["win"] }],
    deps: base({ run, loadMediaCatalog: () => ({ catalog: fx.catalog, crateDir: fx.crateDir, crateVersion: "0.9.0" }) }),
  });
  assert.equal(result.files[0].path, `src-tauri/binaries/ffmpeg-${WIN}.exe`);
  assertAttributed(snapshot, result, "ffmpeg win");
});

test("[7.05a ffmpeg universal] the per-arch slices are staged under the checkout's .right-release/slices, never in the snapshot; only the merged output, its notices and the receipt land there", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const arm = Buffer.from("ffmpeg-arm64"); const x86 = Buffer.from("ffmpeg-x86_64");
  const fx = ffmpegFixture(dir, { rows: [
    { id: "ffmpeg-decode-macos-arm64", tool: "ffmpeg", variant: "decode", os: "macos", arch: "aarch64", bytes: arm },
    { id: "ffmpeg-decode-macos-x64", tool: "ffmpeg", variant: "decode", os: "macos", arch: "x86_64", bytes: x86 },
  ] });
  const slicesSeen = [];
  const run = (command, args, options) => {
    if (command === "lipo" && args[0] === "-archs") return { status: 0, stdout: "x86_64 arm64\n", stderr: "" };
    if (command === "lipo" && args[0] === "-create") { const out = args[args.indexOf("-output") + 1]; fs.writeFileSync(out, Buffer.concat(args.slice(args.indexOf("-output") + 2).map((f) => fs.readFileSync(f)))); return { status: 0, stdout: "", stderr: "" }; }
    if (command === "rustright" && args[0] === "ffmpeg-stage") {
      const arch = args[1]; const stageDir = args[2]; const triple = arch === "aarch64" ? MAC : MAC_X86;
      slicesSeen.push(stageDir);
      fs.mkdirSync(stageDir, { recursive: true }); fs.writeFileSync(path.join(stageDir, `ffmpeg-${triple}`), arch === "aarch64" ? arm : x86);
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  const result = await stageSidecars({
    appRoot: snapshot, cacheRoot: checkout, target: "universal-apple-darwin",
    manifest: [{ kind: "ffmpeg", variant: "decode", tools: ["ffmpeg"], catalog: { crate: "rightkit-ffmpeg" }, notices: NOTICES, stage: { cmd: "rustright", args: ["ffmpeg-stage", "{arch}", "{stageDir}"] } }],
    deps: base({ run, loadMediaCatalog: () => ({ catalog: fx.catalog, crateDir: fx.crateDir, crateVersion: "0.9.0" }) }),
  });
  assert.equal(result.files[0].path, "src-tauri/binaries/ffmpeg-universal-apple-darwin");
  assert.ok(slicesSeen.length === 2 && slicesSeen.every((d) => d.startsWith(path.join(checkout, ".right-release", "slices"))), `the stage command wrote its slices under the checkout: ${slicesSeen.join(", ")}`);
  assert.ok(!fs.existsSync(path.join(snapshot, ".right-release")), "no slice or cache in the snapshot");
  assertAttributed(snapshot, result, "ffmpeg universal");
});

test("[7.05a command / cmake / remote] the other kinds keep their trees and caches in the checkout: a command's outputs and the remote download cache; no .right-release in the snapshot", async (t) => {
  const { dir, snapshot, checkout } = roots(t);
  const bytes = Buffer.from("tls-client");
  const manifest = [
    { kind: "remote", name: "tls", url: "https://example.test/tls.dll", sha256: sha(bytes), dest: "src-tauri/resources/runtime/tls.dll", platforms: ["win"] },
    { kind: "command", name: "ffmpeg-win", cmd: "node", args: ["scripts/prepare.mjs"], output: ".cache/ffmpeg/ffmpeg.exe", dest: "src-tauri/resources/runtime/ffmpeg.exe", platforms: ["win"] },
  ];
  const run = (command, args, options) => { if (command === "node") { fs.mkdirSync(path.join(options.cwd, ".cache", "ffmpeg"), { recursive: true }); fs.writeFileSync(path.join(options.cwd, ".cache", "ffmpeg", "ffmpeg.exe"), "exe"); } return { status: 0, stdout: "", stderr: "" }; };
  let fetched = 0;
  const result = await stageSidecars({ appRoot: snapshot, cacheRoot: checkout, manifest, target: WIN, deps: base({ run, fetch: async () => { fetched += 1; return { ok: true, status: 200, arrayBuffer: async () => bytes }; } }) });
  assert.ok(fs.existsSync(path.join(checkout, ".right-release", "cache", "remote", sha(bytes))), "the remote download cache is the checkout's");
  assert.ok(!fs.existsSync(path.join(snapshot, ".right-release")));
  assert.equal(fetched, 1);
  assert.ok(result.files.some((f) => f.name === "tls") && result.files.some((f) => f.name === "ffmpeg-win"));
});
