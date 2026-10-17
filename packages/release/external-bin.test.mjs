import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import {
  EXTERNAL_BIN_CONTRACT,
  buildExternalBinReceipt,
  externalBinNestedCode,
  planExternalBin,
  validateExternalBinConfig,
  validateExternalBinReceipt,
  verifyStagedExternalBins,
} from "./external-bin.mjs";

const NAMES = ["mailright-engine", "mailright-core"];
const hash = (char) => char.repeat(64);

test("config: two sidecars stage as <name>-<triple> and bundle without the triple", () => {
  const mac = validateExternalBinConfig({ names: NAMES, arch: "aarch64" }, { platform: "mac" });
  assert.equal(mac.stageDir, "src-tauri/binaries");
  assert.equal(mac.bundleDir, "Contents/MacOS");
  const plan = planExternalBin({ config: mac, root: "/app" });
  assert.deepEqual(plan.files.map((file) => path.basename(file.stagedPath)), ["mailright-engine-aarch64-apple-darwin", "mailright-core-aarch64-apple-darwin"]);
  assert.deepEqual(plan.files.map((file) => file.bundlePath), ["Contents/MacOS/mailright-engine", "Contents/MacOS/mailright-core"]);
  assert.deepEqual(Object.keys(externalBinNestedCode(mac)), ["Contents/MacOS/mailright-engine", "Contents/MacOS/mailright-core"]);

  const win = validateExternalBinConfig({ names: NAMES, arch: "x86_64" }, { platform: "win" });
  const winPlan = planExternalBin({ config: win, root: "/app" });
  assert.deepEqual(winPlan.files.map((file) => path.basename(file.stagedPath)), ["mailright-engine-x86_64-pc-windows-msvc.exe", "mailright-core-x86_64-pc-windows-msvc.exe"]);
  assert.deepEqual(winPlan.files.map((file) => file.bundledName), ["mailright-engine.exe", "mailright-core.exe"]);
  assert.deepEqual(externalBinNestedCode(win), {});
});

test("config: bad names, paths, keys and ffmpeg name clashes are refused together", () => {
  assert.throws(() => validateExternalBinConfig({ names: ["a/b"], stageDir: "../x", extra: 1 }, { platform: "mac", hostArch: "aarch64" }), /names must be[\s\S]*stageDir must be relative[\s\S]*unknown key/);
  assert.throws(() => validateExternalBinConfig({ names: ["ffmpeg"] }, { platform: "win", hostArch: "x86_64", reservedNames: ["ffmpeg", "ffprobe"] }), /already staged by mediaSidecar/);
  assert.throws(() => validateExternalBinConfig({ names: NAMES, bundleDir: "Contents/MacOS" }, { platform: "win", hostArch: "x86_64" }), /bundleDir applies to macOS only/);
  assert.throws(() => validateExternalBinConfig({ names: NAMES }, { platform: "ios" }), /not supported/);
});

test("staged check reuses the sidecar rule: a missing or empty file refuses", () => {
  const plan = planExternalBin({ config: validateExternalBinConfig({ names: NAMES, arch: "x86_64" }, { platform: "win" }), root: "/app" });
  assert.throws(() => verifyStagedExternalBins(plan, { exists: () => false }), (error) => error.code === "MEDIA_SIDECAR_NOT_STAGED");
  const pre = verifyStagedExternalBins(plan, { exists: () => true, stat: () => ({ isFile: () => true, size: 5 }), hashFile: () => hash("a") });
  assert.deepEqual(pre.map((file) => file.preSignSha256), [hash("a"), hash("a")]);
});

test("receipt: signed digests are required and signing must change the bytes", () => {
  const plan = planExternalBin({ config: validateExternalBinConfig({ names: NAMES, arch: "aarch64" }, { platform: "mac" }), root: "/app" });
  const pre = plan.files.map((file) => ({ ...file, preSignSha256: hash("a"), preSignSizeBytes: 5 }));
  assert.throws(() => buildExternalBinReceipt(plan, pre, {}), /no signed digest/);
  const receipt = buildExternalBinReceipt(plan, pre, Object.fromEntries(NAMES.map((name) => [name, { sha256: hash("b"), sizeBytes: 9 }])));
  assert.equal(receipt.contract, EXTERNAL_BIN_CONTRACT);
  validateExternalBinReceipt(receipt, { os: "macos", names: NAMES });
  assert.throws(() => validateExternalBinReceipt({ ...receipt, files: receipt.files.map((file) => ({ ...file, signedSha256: file.preSignSha256 })) }), /not changed by signing/);
  assert.throws(() => validateExternalBinReceipt(receipt, { names: ["other"] }), /names differ/);
});
