import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import {
  MEDIA_SIDECAR_CONTRACT,
  MEDIA_SIDECAR_OFFER_NAME,
  buildMediaSidecarReceipt,
  bundledFileName,
  macSidecarSigningProblems,
  mediaSidecarNestedCode,
  planMediaSidecar,
  selectSidecarRows,
  sidecarTargetTriple,
  sourceOfferText,
  stagedFileName,
  validateMediaSidecarConfig,
  validateMediaSidecarReceipt,
  verifyStagedSidecars,
  windowsSidecarSigningProblems,
  writeMediaSidecarNotices,
} from "./media-sidecar.mjs";
import { cargoFfmpegCrate, loadSidecarCatalog } from "./media-sidecar-catalog.mjs";

const hex = (char) => char.repeat(64);
const CONTACT = "https://example.test/ffmpeg-source";

function row(tool, os, arch, overrides = {}) {
  return {
    id: `${tool}-7.1.5-${os}-${arch}`,
    tool,
    variant: "decode",
    os,
    arch,
    version: "7.1.5",
    status: "approved",
    sha256: hex("1"),
    contentSha256: hex("2"),
    sourceSha256: hex("3"),
    attribution: {
      licenseSpdx: "LGPL-2.1-or-later",
      sourceUrl: "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz",
      buildProvenance: "third-party/BUILD-NOTES-macos-arm64.txt",
      noticeFiles: ["third-party/LGPL-2.1.txt", "third-party/BUILD-NOTES-macos-arm64.txt"],
    },
    requiredConfigureFlags: ["--disable-gpl", "--disable-nonfree", "--disable-autodetect"],
    ...overrides,
  };
}

const catalog = {
  schemaVersion: 2,
  artifacts: [
    row("ffmpeg", "macos", "aarch64"),
    row("ffprobe", "macos", "aarch64"),
    row("ffmpeg", "macos", "x86_64"),
    row("ffprobe", "macos", "x86_64"),
    row("ffmpeg", "windows", "x86_64"),
    row("ffprobe", "windows", "x86_64"),
    row("ffmpeg", "linux", "x86_64", { status: "pending", contentSha256: undefined }),
  ],
};

const macConfig = () => validateMediaSidecarConfig({ arch: "aarch64", notices: { dir: "src-tauri/resources/ffmpeg", sourceOfferContact: CONTACT } }, { platform: "mac" });
const winConfig = () => validateMediaSidecarConfig({ notices: { dir: "src-tauri/resources/ffmpeg", sourceOfferContact: "legal@example.test" } }, { platform: "win", hostArch: "x86_64" });
const ROOT = path.resolve("/app");

test("config defaults: decode variant, both tools, Tauri binaries dir, macOS Contents/MacOS", () => {
  const config = macConfig();
  assert.equal(config.os, "macos");
  assert.equal(config.variant, "decode");
  assert.deepEqual(config.tools, ["ffmpeg", "ffprobe"]);
  assert.equal(config.stageDir, "src-tauri/binaries");
  assert.equal(config.bundleDir, "Contents/MacOS");
  const win = winConfig();
  assert.equal(win.arch, "x86_64");
  assert.equal(win.bundleDir, undefined);
});

test("config refuses unknown keys, escaping paths, bad arch, missing notices and bad contact, all at once", () => {
  assert.throws(() => validateMediaSidecarConfig({ nope: 1, arch: "arm64", stageDir: "../x", tools: ["ffplay"], notices: { dir: "n", sourceOfferContact: "no spaces allowed" } }, { platform: "mac" }),
    (error) => error.code === "MEDIA_SIDECAR_CONFIG_INVALID"
      && /unknown key "nope"/.test(error.message) && /arch must be/.test(error.message) && /stageDir must be relative/.test(error.message)
      && /tools must be/.test(error.message) && /sourceOfferContact/.test(error.message));
  assert.throws(() => validateMediaSidecarConfig({ arch: "aarch64" }, { platform: "mac" }), /notices must be/);
  assert.throws(() => validateMediaSidecarConfig({ bundleDir: "Contents/MacOS", notices: { dir: "n", sourceOfferContact: CONTACT } }, { platform: "win", hostArch: "x86_64" }), /bundleDir applies to macOS only/);
  assert.throws(() => validateMediaSidecarConfig({ arch: "aarch64", bundleDir: "Contents/Frameworks", notices: { dir: "n", sourceOfferContact: CONTACT } }, { platform: "mac" }), /bundleDir must be/);
  assert.throws(() => validateMediaSidecarConfig({}, { platform: "android" }), /not supported on platform/);
});

test("file names follow the Tauri externalBin convention", () => {
  assert.equal(sidecarTargetTriple("macos", "aarch64"), "aarch64-apple-darwin");
  assert.equal(stagedFileName("ffmpeg", "windows", "x86_64"), "ffmpeg-x86_64-pc-windows-msvc.exe");
  assert.equal(stagedFileName("ffprobe", "linux", "x86_64"), "ffprobe-x86_64-unknown-linux-gnu");
  assert.equal(bundledFileName("ffmpeg", "windows"), "ffmpeg.exe");
  assert.equal(bundledFileName("ffprobe", "macos"), "ffprobe");
  assert.throws(() => sidecarTargetTriple("macos", "riscv"), /no Tauri target triple/);
});

test("macOS nested code names both executables with the hardened runtime and no entitlements", () => {
  assert.deepEqual(mediaSidecarNestedCode(macConfig()), {
    "Contents/MacOS/ffmpeg": { hardenedRuntime: true },
    "Contents/MacOS/ffprobe": { hardenedRuntime: true },
  });
  assert.deepEqual(mediaSidecarNestedCode(winConfig()), {});
});

test("row selection takes the single approved row per tool and refuses missing, pending and ambiguous rows", () => {
  const rows = selectSidecarRows(catalog, macConfig());
  assert.deepEqual(rows.map((item) => item.id), ["ffmpeg-7.1.5-macos-aarch64", "ffprobe-7.1.5-macos-aarch64"]);
  const linux = { ...winConfig(), os: "linux", tools: ["ffprobe"] };
  assert.throws(() => selectSidecarRows(catalog, linux), (error) => error.code === "MEDIA_SIDECAR_NO_ROW" && /linux\/x86_64/.test(error.message));
  assert.throws(() => selectSidecarRows(catalog, { ...linux, tools: ["ffmpeg"] }), (error) => error.code === "MEDIA_SIDECAR_NOT_APPROVED" && /pending/.test(error.message));
  assert.throws(() => selectSidecarRows({ artifacts: [...catalog.artifacts, row("ffmpeg", "windows", "x86_64", { id: "dup" })] }, winConfig()), (error) => error.code === "MEDIA_SIDECAR_AMBIGUOUS_ROW");
  assert.throws(() => selectSidecarRows({ artifacts: [row("ffmpeg", "windows", "x86_64", { contentSha256: undefined }), row("ffprobe", "windows", "x86_64")] }, winConfig()), /no content digest/);
  assert.throws(() => selectSidecarRows(catalog, { ...macConfig(), variant: "export-ass" }), (error) => error.code === "MEDIA_SIDECAR_NO_ROW");
});

test("a Linux row becomes usable the moment the catalog approves it, with no code change", () => {
  const approved = { artifacts: [row("ffmpeg", "linux", "x86_64"), row("ffprobe", "linux", "x86_64")] };
  const config = { ...winConfig(), os: "linux" };
  const plan = planMediaSidecar({ config, catalog: approved, crateDir: path.resolve("/crate"), root: ROOT });
  assert.deepEqual(plan.files.map((file) => path.basename(file.stagedPath)), ["ffmpeg-x86_64-unknown-linux-gnu", "ffprobe-x86_64-unknown-linux-gnu"]);
});

function plan(config = macConfig()) {
  return planMediaSidecar({ config, catalog, crateDir: path.resolve("/crate"), crateVersion: "0.2.6", catalogBytes: Buffer.from("{}"), root: ROOT });
}

test("plan stages triple-named files, bundle paths, the crate's notice files and the source offer", () => {
  const mac = plan();
  assert.deepEqual(mac.files.map((file) => path.relative(ROOT, file.stagedPath).replaceAll("\\", "/")),
    ["src-tauri/binaries/ffmpeg-aarch64-apple-darwin", "src-tauri/binaries/ffprobe-aarch64-apple-darwin"]);
  assert.deepEqual(mac.files.map((file) => file.bundlePath), ["Contents/MacOS/ffmpeg", "Contents/MacOS/ffprobe"]);
  assert.deepEqual(mac.notices.map((item) => path.basename(item.dest)), ["LGPL-2.1.txt", "BUILD-NOTES-macos-arm64.txt"]);
  assert.equal(mac.notices[0].source, path.join(path.resolve("/crate"), "third-party", "LGPL-2.1.txt"));
  assert.equal(path.basename(mac.offerPath), MEDIA_SIDECAR_OFFER_NAME);
  assert.equal(mac.catalog.crateVersion, "0.2.6");
  assert.match(mac.catalog.sha256, /^[0-9a-f]{64}$/);
  const win = plan(winConfig());
  assert.deepEqual(win.files.map((file) => path.basename(file.stagedPath)), ["ffmpeg-x86_64-pc-windows-msvc.exe", "ffprobe-x86_64-pc-windows-msvc.exe"]);
  assert.equal(win.files[0].bundlePath, undefined);
  assert.equal(win.files[0].bundledName, "ffmpeg.exe");
});

test("plan refuses a notice path that leaves the crate", () => {
  const bad = { artifacts: [row("ffmpeg", "windows", "x86_64", { attribution: { ...row("ffmpeg", "windows", "x86_64").attribution, noticeFiles: ["../secret.txt"] } }), row("ffprobe", "windows", "x86_64")] };
  assert.throws(() => planMediaSidecar({ config: winConfig(), catalog: bad, crateDir: path.resolve("/crate"), root: ROOT }), /escapes the crate/);
});

test("source offer names the LGPL, the exact source, its digest, the recipes and the contact", () => {
  const text = plan().offer;
  assert.match(text, /LGPL-2\.1-or-later/);
  assert.match(text, /--disable-gpl and --disable-nonfree/);
  assert.match(text, /https:\/\/ffmpeg\.org\/releases\/ffmpeg-7\.1\.5\.tar\.xz/);
  assert.match(text, new RegExp(hex("3")));
  assert.match(text, /crates\/rightkit-ffmpeg \(crate rightkit-ffmpeg 0\.2\.6/);
  assert.match(text, /example\.test\/ffmpeg-source/);
  assert.match(sourceOfferText({ rows: plan().rows, contact: "a@b.test" }), /a@b\.test/);
});

test("notices are copied and the offer written through the injected file system; a missing notice refuses", () => {
  const written = [];
  const copied = [];
  const made = [];
  const result = writeMediaSidecarNotices(plan(), {
    exists: () => true,
    mkdir: (dir) => made.push(dir),
    copy: (from, to) => copied.push([from, to]),
    write: (file, text) => written.push([file, text]),
  });
  assert.equal(made.length, 1);
  assert.equal(copied.length, 2);
  assert.equal(written.length, 1);
  assert.equal(path.basename(written[0][0]), MEDIA_SIDECAR_OFFER_NAME);
  assert.equal(result.length, 3);
  assert.throws(() => writeMediaSidecarNotices(plan(), { exists: (file) => !file.endsWith("LGPL-2.1.txt"), mkdir() {}, copy() { assert.fail("nothing is copied when a notice is missing"); }, write() {} }),
    (error) => error.code === "MEDIA_SIDECAR_NOT_STAGED" || error.code === "MEDIA_SIDECAR_NOTICE_MISSING");
});

test("staged binaries must exist, be non-empty files, and are hashed before signing", () => {
  const pre = verifyStagedSidecars(plan(), {
    exists: () => true,
    stat: () => ({ isFile: () => true, size: 4096 }),
    hashFile: (file) => (file.includes("ffprobe") ? hex("b") : hex("a")),
  });
  assert.deepEqual(pre.map((file) => file.preSignSha256), [hex("a"), hex("b")]);
  assert.throws(() => verifyStagedSidecars(plan(), { exists: () => false }), (error) => error.code === "MEDIA_SIDECAR_NOT_STAGED" && /stage_external_bin/.test(error.message));
  assert.throws(() => verifyStagedSidecars(plan(), { exists: () => true, stat: () => ({ isFile: () => true, size: 0 }), hashFile: () => hex("a") }), /non-empty file/);
});

function receiptFor(config = macConfig()) {
  const sidecarPlan = plan(config);
  const pre = sidecarPlan.files.map((file, index) => ({ ...file, preSignSha256: hex(String(index + 4)), preSignSizeBytes: 10 }));
  const signed = Object.fromEntries(sidecarPlan.files.map((file, index) => [file.tool, { sha256: hex("cd"[index]), sizeBytes: 12 }]));
  return buildMediaSidecarReceipt(sidecarPlan, pre, signed, { hashFile: (file) => hex(file.includes("LGPL") ? "e" : "f") });
}

test("receipt records catalog identity, pre-sign and signed digests, notice digests and the contract", () => {
  const receipt = receiptFor();
  assert.equal(receipt.contract, MEDIA_SIDECAR_CONTRACT);
  assert.equal(receipt.signing, "developer-id-hardened-runtime");
  assert.equal(receipt.files[0].catalogContentSha256, hex("2"));
  assert.equal(receipt.files[0].signedSha256, hex("c"));
  assert.equal(receipt.files[1].bundlePath, "Contents/MacOS/ffprobe");
  assert.ok(receipt.notices.some((item) => item.name === MEDIA_SIDECAR_OFFER_NAME));
  assert.equal(receiptFor(winConfig()).signing, "authenticode");
  validateMediaSidecarReceipt(receipt, { os: "macos", variant: "decode", tools: ["ffmpeg", "ffprobe"] });
  assert.doesNotThrow(() => validateMediaSidecarReceipt(receipt));
});

test("receipt validation refuses an unsigned change, missing LGPL, wrong variant or tools, and a missing signed digest", () => {
  const good = receiptFor();
  const clone = () => structuredClone(good);
  const unchanged = clone(); unchanged.files[0].signedSha256 = unchanged.files[0].preSignSha256;
  assert.throws(() => validateMediaSidecarReceipt(unchanged), /was not changed by signing/);
  const noLgpl = clone(); noLgpl.notices = noLgpl.notices.filter((item) => !/LGPL/.test(item.name));
  assert.throws(() => validateMediaSidecarReceipt(noLgpl), /LGPL text/);
  const noOffer = clone(); noOffer.notices = noOffer.notices.filter((item) => item.name !== MEDIA_SIDECAR_OFFER_NAME);
  assert.throws(() => validateMediaSidecarReceipt(noOffer), /source offer/);
  assert.throws(() => validateMediaSidecarReceipt(good, { variant: "export-ass" }), /variant/);
  assert.throws(() => validateMediaSidecarReceipt(good, { os: "windows" }), /os macos/);
  assert.throws(() => validateMediaSidecarReceipt(good, { tools: ["ffmpeg"] }), /tools differ/);
  const dup = clone(); dup.files[1].tool = "ffmpeg";
  assert.throws(() => validateMediaSidecarReceipt(dup), /duplicate tool/);
  assert.throws(() => validateMediaSidecarReceipt({ ...good, contract: "other" }), /unrecognized contract/);
  const sidecarPlan = plan();
  assert.throws(() => buildMediaSidecarReceipt(sidecarPlan, sidecarPlan.files.map((f) => ({ ...f, preSignSha256: hex("4") })), {}), /no signed digest/);
});

test("macOS: every sidecar must be strict-valid in the nested signing receipt with the digest now on disk", () => {
  const files = [{ file: path.resolve("/app/A.app/Contents/MacOS/ffmpeg"), sha256: hex("c") }];
  const ok = { files: [{ file: files[0].file, codesign: "strict-valid", after: { sha256: hex("c") } }] };
  assert.deepEqual(macSidecarSigningProblems(ok, files), []);
  assert.match(macSidecarSigningProblems({ files: [] }, files)[0], /not in the nested signing receipt/);
  assert.match(macSidecarSigningProblems({ files: [{ ...ok.files[0], codesign: "failed" }] }, files)[0], /not strict-valid/);
  assert.match(macSidecarSigningProblems({ files: [{ ...ok.files[0], after: { sha256: hex("d") } }] }, files)[0], /bytes changed after signing/);
});

test("Windows: sidecar digests must be Authenticode-valid, timestamped entries of the raw-EXE receipt", () => {
  const receipt = receiptFor(winConfig());
  const entries = receipt.files.map((file) => ({ file: file.tool, authenticode: "Valid", timestampPresent: true, after: { sha256: file.signedSha256 } }));
  assert.deepEqual(windowsSidecarSigningProblems({ files: entries }, receipt), []);
  assert.match(windowsSidecarSigningProblems({ files: entries.slice(1) }, receipt)[0], /not in the raw-EXE signing receipt/);
  assert.match(windowsSidecarSigningProblems({ files: entries.map((item) => ({ ...item, timestampPresent: false })) }, receipt)[0], /not Valid with a timestamp/);
});

test("catalog loads from the configured path, or from the crate Cargo resolves for the app", () => {
  const reads = [];
  const read = (file) => { reads.push(file); return Buffer.from(JSON.stringify(catalog)); };
  const explicit = loadSidecarCatalog({ config: { catalog: "vendor/rightkit-ffmpeg/catalog/artifacts.json" }, root: ROOT, read });
  assert.equal(explicit.crateDir, path.join(ROOT, "vendor", "rightkit-ffmpeg"));
  assert.equal(explicit.catalog.schemaVersion, 2);
  const viaCargo = loadSidecarCatalog({ config: {}, root: ROOT, manifestPath: path.join(ROOT, "src-tauri", "Cargo.toml"), read, crateFor: () => ({ crateDir: path.resolve("/reg/rightkit-ffmpeg-0.2.6"), crateVersion: "0.2.6" }) });
  assert.equal(viaCargo.crateVersion, "0.2.6");
  assert.equal(reads.at(-1), path.join(path.resolve("/reg/rightkit-ffmpeg-0.2.6"), "catalog", "artifacts.json"));
  assert.throws(() => loadSidecarCatalog({ config: {}, root: ROOT }), (error) => error.code === "MEDIA_SIDECAR_CRATE_NOT_FOUND");
  assert.throws(() => loadSidecarCatalog({ config: { catalog: "c.json" }, root: ROOT, read: () => { throw new Error("ENOENT"); } }), (error) => error.code === "MEDIA_SIDECAR_CATALOG_INVALID");
  assert.throws(() => loadSidecarCatalog({ config: { catalog: "c.json" }, root: ROOT, read: () => Buffer.from("nope") }), /not JSON/);
});

test("cargo resolution wants exactly one rightkit-ffmpeg package and reports launch or metadata failures", () => {
  const metadata = (packages) => ({ status: 0, stdout: JSON.stringify({ packages }) });
  const pkg = { name: "rightkit-ffmpeg", version: "0.2.6", manifest_path: path.resolve("/reg/rightkit-ffmpeg-0.2.6/Cargo.toml") };
  assert.deepEqual(cargoFfmpegCrate("/app/Cargo.toml", { run: () => metadata([pkg, { name: "serde" }]) }), { crateDir: path.dirname(pkg.manifest_path), crateVersion: "0.2.6" });
  assert.throws(() => cargoFfmpegCrate("/app/Cargo.toml", { run: () => metadata([]) }), /0 rightkit-ffmpeg packages/);
  assert.throws(() => cargoFfmpegCrate("/app/Cargo.toml", { run: () => metadata([pkg, { ...pkg, version: "0.2.5" }]) }), /2 rightkit-ffmpeg packages/);
  assert.throws(() => cargoFfmpegCrate("/app/Cargo.toml", { run: () => ({ status: 101, stderr: "no lock" }) }), /cargo metadata failed: no lock/);
  assert.throws(() => cargoFfmpegCrate("/app/Cargo.toml", { run: () => ({ error: new Error("spawn"), status: null }) }), /could not run cargo metadata/);
});
