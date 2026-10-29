#!/usr/bin/env node
import { existsSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { refuseManagedEnvSecrets } from "./managed-host-secrets.mjs";
import { execWithUpdaterCredential } from "./protected-credentials.mjs";
import { resolvePnpmCli } from "./development.mjs";
import { UPDATER_KEY_ID_RE, updaterSignatureKeyId } from "./github-updater-release.mjs";
import { ArtifactPathError, loadSplitArtifactContext, resolveArtifactPath } from "./release-artifact-path.mjs";

export const UPDATER_SIGNING_SECRET = "updater-signing-key";

export function runSignUpdater(args, {
  env = process.env, platform = process.platform, cwd = process.cwd(),
  execCredential = execWithUpdaterCredential, resolvePnpm = resolvePnpmCli,
  signMac = signMacUpdaterFiles, exists = existsSync, remove = rmSync, read = (file) => readFileSync(file, "utf8"), log = console.log,
  // The artifact-path context of a door split (release-artifact-path.mjs, row 7.04c): `--file` is then the bound Cargo target's, never the checkout's.
  artifactPaths = null,
} = {}) {
  const files = [];
  let dryRun = false;
  let keyId;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--file") {
      if (!args[i + 1] || args[i + 1].startsWith("--")) throw new Error("--file requires an artifact path");
      files.push(artifactPaths?.mode === "split" ? resolveArtifactPath(args[++i], artifactPaths) : path.resolve(cwd, args[++i]));
    } else if (args[i] === "--key-id") {
      if (keyId !== undefined) throw new Error("--key-id may be given only once");
      if (!UPDATER_KEY_ID_RE.test(args[i + 1] ?? "")) throw new Error("--key-id requires the 16 upper-case hex digits of the updater key id");
      keyId = args[++i];
    } else if (args[i] === "--key-file") throw new Error("--key-file is removed: updater keys come from the OS credential provider");
    else if (args[i] === "--dry-run") dryRun = true;
    else throw new Error(`unknown argument: ${args[i]}`);
  }
  if (!files.length) throw new Error("at least one --file is required");
  if (!dryRun) {
    refuseManagedEnvSecrets(env, ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]);
    for (const key of Object.keys(env)) {
      if (["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"].includes(key.toUpperCase()) && env[key]) {
        throw new Error("updater signing key/password must come from the OS credential provider");
      }
    }
    for (const file of files) if (!exists(file)) throw new Error(`updater artifact not found: ${file}`);
  }
  const signingEnv = { ...env };
  for (const key of Object.keys(signingEnv)) {
    if (["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"].includes(key.toUpperCase())) delete signingEnv[key];
  }
  // The credential store holds whichever key was last provisioned, so the signature, not the request, says what signed the file.
  const assertSignedByExpectedKey = (file) => {
    if (keyId === undefined) return;
    let found = null;
    try { found = updaterSignatureKeyId(read(`${file}.sig`)); } catch { /* an unreadable signature is refused below */ }
    if (found !== keyId) throw new Error(`updater signature ${file}.sig was not made by updater key ${keyId} (found ${found ?? "unreadable"})`);
  };
  for (const file of files) {
    log(`${dryRun ? "[dry-run] " : ""}pnpm exec tauri signer sign ${file}`);
    if (dryRun) {
      if (keyId !== undefined) log(`[dry-run] ${file}.sig must be made by updater key ${keyId}`);
      continue;
    }
    remove(`${file}.sig`, { force: true });
    if (platform === "darwin") continue;
    const result = execCredential(process.execPath,
      [resolvePnpm({ root: cwd, env: signingEnv }), "exec", "tauri", "signer", "sign", file],
      { cwd, env: signingEnv });
    if (result.error || result.status !== 0) {
      const error = new Error(`updater credential provider/signing failed (exit ${result.status ?? "unavailable"})`);
      error.exitCode = result.status ?? 1;
      throw error;
    }
    assertSignedByExpectedKey(file);
  }
  if (!dryRun && platform === "darwin") {
    signMac(files, { env: signingEnv, cwd });
    for (const file of files) assertSignedByExpectedKey(file);
  }
  return { files, dryRun };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { runSignUpdater(process.argv.slice(2), { artifactPaths: await loadSplitArtifactContext({ cwd: process.cwd() }) }); }
  catch (error) {
    console.error(`sign-updater: ${error instanceof ArtifactPathError ? "" : error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = error.exitCode ?? 1;
  }
}
