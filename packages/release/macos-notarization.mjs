import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { MACOS_PORTABLE_SIGNING_CONTRACT } from "./macos-signing-contract.mjs";
import { detectMacSigningSession, runMacGuiNotarization } from "./macos-signing-session.mjs";
import { notarytoolAuthArgs } from "./notary-auth.mjs";

function checkedRun(args, run) {
  const result = run("xcrun", args, { encoding: "utf8" });
  if (result.error || result.status !== 0) throw new Error(`xcrun ${args.slice(0, 2).join(" ")} failed: ${result.error?.message ?? result.stderr ?? result.stdout ?? result.status}`);
  return result.stdout ?? "";
}

export function stapleMacosArtifact(file, options = {}) {
  const { run = spawnSync, env = process.env } = options;
  file = path.resolve(file);
  if (!/\.(dmg|pkg|app)$/i.test(file)) throw new Error(`not a stapleable macOS artifact: ${file}`);
  if (detectMacSigningSession({ env, run }).guiRunner) {
    return { ...runMacGuiNotarization({ ...options, kind: "staple", file }).stapling[0], file };
  }
  const probe = run("xcrun", ["stapler", "validate", file], { encoding: "utf8" });
  if (probe.error) throw new Error(`cannot validate macOS staple: ${probe.error.message}`);
  const alreadyStapled = probe.status === 0;
  if (!alreadyStapled) checkedRun(["stapler", "staple", file], run);
  checkedRun(["stapler", "validate", file], run);
  return { file, alreadyStapled, validated: true };
}

function fileEvidence(file) {
  return { sha256: createHash("sha256").update(readFileSync(file)).digest("hex"), sizeBytes: statSync(file).size };
}

// DMG/PKG receive their own ticket. For a separately submitted app ZIP,
// target.notarize.app (CLI --app) identifies the bundle to staple before packaging.
export function notarizeMacosArtifact({ archive, receiptPath, authArgs, profile, app, run = spawnSync, env = process.env, ...agentOptions }) {
  archive = path.resolve(archive);
  const stapleTargets = /\.(dmg|pkg)$/i.test(archive) ? [archive] : [];
  if (app) {
    const bundle = path.resolve(app);
    if (!/\.app$/i.test(bundle)) throw new Error(`notarize.app must name an app bundle: ${bundle}`);
    stapleTargets.push(bundle);
  }
  if (!stapleTargets.length) throw new Error("notarization archives require notarize.app (or --app) for stapling their app bundle");
  let result, stapling;
  if (detectMacSigningSession({ env, run }).guiRunner) {
    // SSH ignores API credential environment/argv entirely. Only profile name
    // reaches Aqua, where Keychain authentication already works.
    const selectedProfile = profile ?? (authArgs?.[0] === "--keychain-profile" && authArgs.length === 2 ? authArgs[1] : undefined);
    const report = runMacGuiNotarization({ ...agentOptions, kind: "notarize", file: archive, app, profile: selectedProfile, env, run });
    result = report.notarization;
    stapling = report.stapling.map((item) => ({ ...item, file: stapleTargets.find((file) => realpathSync.native(file) === item.file) ?? item.file }));
  } else {
    authArgs ??= notarytoolAuthArgs({ profile, env });
    try { result = JSON.parse(checkedRun(["notarytool", "submit", archive, "--wait", "--output-format", "json", ...authArgs], run)); }
    catch (error) { throw new Error(`macOS notarization failed for ${archive}: ${error.message}`); }
    if (result.status !== "Accepted") throw new Error(`notarization was not accepted for ${archive}: ${result.status ?? "missing status"}`);
    stapling = [...new Set(stapleTargets)].map((file) => stapleMacosArtifact(file, { run, env }));
  }
  // Stapling changes container bytes. Record final bytes, never the submission digest.
  const receipt = {
    schema: 1,
    signingContract: MACOS_PORTABLE_SIGNING_CONTRACT,
    archive: { file: archive, ...fileEvidence(archive) },
    notarization: { status: result.status, id: result.id ?? result.submissionId ?? null },
    stapling,
  };
  mkdirSync(path.dirname(path.resolve(receiptPath)), { recursive: true });
  writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  return receipt;
}

export function validateNotarizationReceipt({ receiptPath, archive, run = spawnSync }) {
  let receipt;
  try { receipt = JSON.parse(readFileSync(receiptPath, "utf8")); }
  catch { throw new Error(`missing or invalid macOS notarization receipt: ${receiptPath}`); }
  archive = path.resolve(archive);
  if (receipt.schema !== 1 || receipt.signingContract !== MACOS_PORTABLE_SIGNING_CONTRACT
    || path.resolve(receipt.archive?.file ?? "") !== archive || receipt.archive.sha256 !== fileEvidence(archive).sha256
    || receipt.notarization?.status !== "Accepted") throw new Error(`invalid macOS notarization receipt evidence: ${receiptPath}`);
  const stapling = receipt.stapling;
  if (!Array.isArray(stapling) || !stapling.length || stapling.some((item) => item.validated !== true || !/\.(dmg|pkg|app)$/i.test(item.file ?? ""))
    || (/\.(dmg|pkg|app)$/i.test(archive) && !stapling.some((item) => path.resolve(item.file) === archive))) {
    throw new Error(`missing macOS stapling evidence: ${receiptPath}; rerun notarization before sealing`);
  }
  for (const item of stapling) checkedRun(["stapler", "validate", path.resolve(item.file)], run);
  return receipt;
}
