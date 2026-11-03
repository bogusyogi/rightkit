#!/usr/bin/env node
// `right-release direct-release finalize --config <file> [--output-dir <dir>] [--dry-run]`: the finalize route of a direct (non-store) release
// (#1142). It composes the owner functions of direct-bootstrap.mjs, so an app keeps no staging or finalize script of its own:
//
//   1. optional payload archive: `createPortableArchive` zips `sourceDir` into `archive` for every asset that names a `sourceDir`;
//   2. `collectReleaseAsset` binds each archive to its SLSA provenance and CycloneDX SBOM (both validated against the archive's sha256);
//   3. `materializeDirectRelease` writes checksums.json, the release manifest and its detached Azure-signed catalog (Windows signing host);
//   4. `renderPowerShellBootstrap` writes install.ps1 (the signer the manifest was just signed with is the one it accepts), checked by
//      `validatePowerShellBootstrap`, and `planBootstrapPublication` writes the bootstrap publication plan (publication is its own step).
//
//   directRelease: {
//     product: "coderight-engine", repository: "owner/repo",           // GitHub repository holding the immutable release assets
//     minimumBootstrapVersion: "1.0.0", bootstrapVersion: "1.0.0",
//     outputDir: "dist/direct-release",                                 // optional (default); --output-dir overrides
//     assets: [{ target: "windows-x86_64", name: "engine-win-x64.zip", archive: "dist/engine-win-x64.zip", sourceDir?: "dist/payload",
//                executablePath: "bin/engine.exe", nativeSignaturePolicy: "authenticode-valid",
//                provenance: "dist/engine-win-x64.intoto.jsonl", sbom: "dist/engine-win-x64.cdx.json" }],
//     bootstrap: { installRootSubdir, executablePath, activationArgs, statusArgs, preflightArgs?, expectedStatusKind? | healthAssertions?,
//                  expectedStatus?, priorExecutableMinVersion?, allowLocalReleaseRoot? }
//   }
//
// The release version is the config's `version`; the source commit is HEAD of a clean checkout. `--dry-run` validates the config and every
// input file and signs, writes and uploads nothing. Uploads are not part of this route (the R2 upload is its own step).
import { existsSync, mkdirSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { CommandSpecError, loadAppConfig, relativeInside } from "./command-spec.mjs";
import { collectReleaseAsset, createPortableArchive, materializeDirectRelease, planBootstrapPublication, renderPowerShellBootstrap, validatePowerShellBootstrap } from "./direct-bootstrap.mjs";
import { defaultReleaseConfigFile } from "./release-config.mjs";
import { assertCleanSource } from "./source-gate.mjs";

const fail = (message) => { throw new CommandSpecError(message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const KEYS = ["product", "repository", "minimumBootstrapVersion", "bootstrapVersion", "outputDir", "assets", "bootstrap"];
const ASSET_KEYS = ["target", "name", "archive", "sourceDir", "executablePath", "nativeSignaturePolicy", "provenance", "sbom"];
const BOOTSTRAP_KEYS = ["installRootSubdir", "executablePath", "activationArgs", "statusArgs", "preflightArgs", "expectedStatusKind", "expectedStatus", "healthAssertions", "priorExecutableMinVersion", "allowLocalReleaseRoot"];

/** Validate `directRelease` and return it with every path checked as app-relative. The deeper contracts are the owner functions' own. */
export function resolveDirectReleaseOptions(config, flags = {}) {
  const declared = config?.directRelease;
  if (!isObject(declared)) fail("no directRelease block in the release config; see right-release direct-release --help");
  const unknown = Object.keys(declared).filter((key) => !KEYS.includes(key));
  if (unknown.length) fail(`directRelease has unknown key(s): ${unknown.join(", ")} (allowed: ${KEYS.join(", ")})`);
  for (const key of ["product", "repository", "minimumBootstrapVersion", "bootstrapVersion"]) if (typeof declared[key] !== "string" || !declared[key]) fail(`directRelease.${key} is required`);
  if (typeof config.version !== "string" || !config.version) fail("the release config has no version");
  const outputDir = relativeInside(flags.outputDir ?? declared.outputDir ?? "dist/direct-release", "directRelease.outputDir");
  if (!Array.isArray(declared.assets) || declared.assets.length === 0) fail("directRelease.assets must list at least one asset");
  const assets = declared.assets.map((asset, index) => {
    const where = `directRelease.assets[${index}]`;
    if (!isObject(asset)) fail(`${where} must be an object`);
    const bad = Object.keys(asset).filter((key) => !ASSET_KEYS.includes(key));
    if (bad.length) fail(`${where} has unknown key(s): ${bad.join(", ")} (allowed: ${ASSET_KEYS.join(", ")})`);
    for (const key of ["target", "name", "archive", "executablePath", "nativeSignaturePolicy", "provenance", "sbom"]) if (typeof asset[key] !== "string" || !asset[key]) fail(`${where}.${key} is required`);
    for (const key of ["archive", "sourceDir", "provenance", "sbom"]) if (asset[key] !== undefined) relativeInside(asset[key], `${where}.${key}`);
    return asset;
  });
  if (!isObject(declared.bootstrap)) fail("directRelease.bootstrap is required (installRootSubdir, executablePath, activationArgs, statusArgs, health)");
  const badBootstrap = Object.keys(declared.bootstrap).filter((key) => !BOOTSTRAP_KEYS.includes(key));
  if (badBootstrap.length) fail(`directRelease.bootstrap has unknown key(s): ${badBootstrap.join(", ")} (allowed: ${BOOTSTRAP_KEYS.join(", ")})`);
  return { ...declared, outputDir, assets, version: config.version };
}

const gitText = (cwd, args) => {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
  return String(result.stdout ?? "");
};

/** Finalize the direct release for the app at `appRoot`. Returns what was written (or would be, under `dryRun`). */
export function finalizeDirectRelease({ appRoot, config, flags = {}, dryRun = false, repoRoot = gitText(appRoot, ["rev-parse", "--show-toplevel"]).trim() }) {
  const options = resolveDirectReleaseOptions(config, flags);
  const inApp = (relative) => path.join(appRoot, relative);
  for (const asset of options.assets) {
    if (asset.sourceDir !== undefined && !(existsSync(inApp(asset.sourceDir)) && statSync(inApp(asset.sourceDir)).isDirectory())) fail(`${asset.name}: sourceDir ${asset.sourceDir} is not a directory`);
    if (asset.sourceDir === undefined && !existsSync(inApp(asset.archive))) fail(`${asset.name}: archive ${asset.archive} does not exist (name a sourceDir to have it built)`);
    for (const key of ["provenance", "sbom"]) if (!existsSync(inApp(asset[key]))) fail(`${asset.name}: ${key} ${asset[key]} does not exist`);
  }
  if (dryRun) return { dryRun: true, product: options.product, version: options.version, assets: options.assets.map((asset) => asset.name), outputDir: inApp(options.outputDir) };
  assertCleanSource({ status: gitText(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]), commandId: "right-release direct-release finalize" });
  const sourceCommit = gitText(repoRoot, ["rev-parse", "HEAD"]).trim();
  const outputDir = inApp(options.outputDir);
  mkdirSync(outputDir, { recursive: true });
  const tag = `v${options.version}`;
  const assets = options.assets.map((asset) => {
    if (asset.sourceDir !== undefined) createPortableArchive({ sourceDir: inApp(asset.sourceDir), outputPath: inApp(asset.archive) });
    return collectReleaseAsset({
      target: asset.target,
      name: asset.name,
      url: `https://github.com/${options.repository}/releases/download/${tag}/${asset.name}`,
      archivePath: inApp(asset.archive),
      executablePath: asset.executablePath,
      nativeSignaturePolicy: asset.nativeSignaturePolicy,
      provenancePath: inApp(asset.provenance),
      sbomPath: inApp(asset.sbom),
    });
  });
  const release = materializeDirectRelease({
    outputDir,
    manifestInput: { product: options.product, version: options.version, tag, sourceCommit, minimumBootstrapVersion: options.minimumBootstrapVersion, assets },
  });
  const script = renderPowerShellBootstrap({
    product: options.product,
    repository: options.repository,
    bootstrapVersion: options.bootstrapVersion,
    acceptedManifestSigners: [release.signing.signer],
    ...options.bootstrap,
  });
  const validation = validatePowerShellBootstrap(script, { product: options.product, acceptedSignerIds: [release.signing.signer.id] });
  if (!validation.valid) throw new Error(`generated bootstrap violates its own contract: ${validation.errors.join("; ")}`);
  const scriptPath = path.join(outputDir, "install.ps1");
  writeFileSync(scriptPath, script);
  const plan = planBootstrapPublication({ product: options.product, bootstrapVersion: options.bootstrapVersion, scriptPath });
  const planPath = path.join(outputDir, "bootstrap-publication-plan.json");
  writeFileSync(planPath, `${JSON.stringify(plan, null, 2)}\n`);
  return { dryRun: false, product: options.product, version: options.version, sourceCommit, outputDir, manifestPath: release.manifestPath, signaturePath: release.signaturePath, scriptPath, planPath, bootstrapSha256: plan.sha256 };
}

export function parseDirectReleaseArgs(argv) {
  const opts = { config: defaultReleaseConfigFile(), outputDir: undefined, dryRun: false };
  const [operation, ...rest] = argv;
  if (operation !== "finalize") throw new Error("expected: direct-release finalize [--config <file>] [--output-dir <dir>] [--dry-run]");
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--config" || arg === "--output-dir") {
      const value = rest[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      opts[arg === "--output-dir" ? "outputDir" : "config"] = value;
      i += 1;
    } else if (arg === "--dry-run") opts.dryRun = true;
    else throw new Error(`unknown direct-release argument: ${arg}`);
  }
  return opts;
}

export async function main(argv = process.argv.slice(2)) {
  const opts = parseDirectReleaseArgs(argv);
  const { config, appRoot } = await loadAppConfig(opts.config);
  const outcome = finalizeDirectRelease({ appRoot, config, flags: { outputDir: opts.outputDir }, dryRun: opts.dryRun });
  if (outcome.dryRun) {
    console.log(`right-release direct-release finalize dry-run: ${outcome.product} ${outcome.version}: ${outcome.assets.join(", ")} -> ${outcome.outputDir}; nothing signed or written`);
    return 0;
  }
  console.log(`right-release direct-release finalize: ${outcome.product} ${outcome.version} at ${outcome.sourceCommit.slice(0, 12)}`);
  console.log(`manifest: ${outcome.manifestPath}\nsignature: ${outcome.signaturePath}\nbootstrap: ${outcome.scriptPath} (sha256 ${outcome.bootstrapSha256})\nplan: ${outcome.planPath}`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`right-release direct-release: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
