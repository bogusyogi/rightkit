#!/usr/bin/env node
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { addonManifestSha256, addonRoutes, canonicalAddonManifest, createAddonManifest, downloadImmutableAddOn, immutableAddonManifestUrl, validateAddonConfig, validateAddonManifest } from "./addon-contract.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { assertCleanSource } from "./source-gate.mjs";

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const SIGN_WINDOWS = path.join(ROOT, "sign-windows.mjs");
const HARDEN = path.join(ROOT, "hardeningscan.mjs");
const [operation, ...args] = process.argv.slice(2);
const options = parse(args);
if (!new Set(["doctor", "build", "upload", "adopt"]).has(operation)) usage(2);
if (!new Set(["mac", "win"]).has(options.platform)) fail("--platform must be mac or win");

if (operation === "adopt") await adopt(options);
else {
  if (!options.config) fail("--config is required");
  const configPath = path.resolve(options.config);
  const root = path.dirname(configPath);
  const repoRoot = git(root, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot);
  const config = (await import(`${pathToFileURL(configPath).href}?addon=${Date.now()}`)).default;
  if (operation === "doctor") doctor({ config, configPath, root, repoRoot, options });
  else if (operation === "build") await build({ config, configPath, root, repoRoot, options });
  else await upload({ config, root, repoRoot, options });
}

function doctor({ config, configPath, root, repoRoot, options }) {
  validateAddonConfig(config, { root, platform: options.platform, allowMissingExecutables: true });
  console.log(`right-release addon doctor: ${config.addon} ${config.version} ${options.platform}`);
  console.log(`config: ${configPath}`);
  console.log(`repo: ${repoRoot}`);
}

async function build({ config, configPath, root, repoRoot, options }) {
  const commit = git(repoRoot, ["rev-parse", "HEAD"]);
  const { target } = validateAddonConfig(config, { root, platform: options.platform, allowMissingExecutables: true });
  if (options.dryRun) {
    console.log(`right-release addon build dry-run: ${config.addon} ${config.version} ${options.platform}`);
    console.log(`config: ${configPath}`);
    console.log(`commit: ${commit}`);
    console.log(`would run: ${target.build.cmd} ${target.build.args.join(" ")}`);
    return;
  }
  assertCleanSource({ status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]), commandId: "right-release addon build" });
  run(target.build.cmd, target.build.args, root);
  const signing = signExecutableRoles(config, root, options.platform);
  const manifest = createAddonManifest({ config, root, platform: options.platform, commit, signing });
  const digest = addonManifestSha256(manifest);
  const sealed = path.join(repoRoot, ".right-release", "addons", config.addon, config.version, commit.slice(0, 8), options.platform);
  if (existsSync(sealed)) fail(`add-on seal already exists: ${sealed}`);
  const temp = `${sealed}.tmp-${process.pid}`;
  mkdirSync(temp, { recursive: true });
  for (const file of config.targets[options.platform].files ?? config.files) copyFileSync(path.resolve(root, file.source), path.join(temp, file.name));
  writeFileSync(path.join(temp, "addon-manifest.json"), canonicalAddonManifest(manifest));
  run(process.execPath, [HARDEN, ...manifest.files.map((file) => path.join(temp, file.name))], root);
  mkdirSync(path.dirname(sealed), { recursive: true });
  renameSync(temp, sealed);
  console.log(`right-release addon build: sealed ${sealed}`);
  console.log(`manifestSha256: ${digest}`);
}

function signExecutableRoles(config, root, platform) {
  const files = config.targets[platform].files ?? config.files;
  const executable = files.filter((file) => file.executable === true);
  if (platform === "win") run(process.execPath, [SIGN_WINDOWS, ...executable.map((file) => path.resolve(root, file.source))], root);
  else for (const file of executable) {
    const absolute = path.resolve(root, file.source);
    run("codesign", ["--force", "--options", "runtime", "--sign", "Developer ID Application", absolute], root);
    const output = command("codesign", ["-dv", "--verbose=4", absolute], root);
    if (!new RegExp(`TeamIdentifier=${config.targets.mac.signing.teamId}`).test(output)) fail(`codesign team mismatch: ${file.name}`);
    run("codesign", ["--verify", "--strict", "--verbose=2", absolute], root);
  }
  return Object.fromEntries(executable.map((file) => [file.role, { contract: config.targets[platform].signing.contract, status: "verified" }]));
}

async function upload({ config, root, repoRoot, options }) {
  if (config.distribution?.provider === "github-releases") fail("this add-on publishes with right-release github --addon <config> --platform <platform>");
  if (!new Set(["patch", "update"]).has(options.tier)) fail("add-on upload requires --tier patch|update");
  const commit = git(repoRoot, ["rev-parse", "HEAD"]);
  const sealed = path.join(repoRoot, ".right-release", "addons", config.addon, config.version, commit.slice(0, 8), options.platform);
  const manifest = JSON.parse(readFileSync(path.join(sealed, "addon-manifest.json"), "utf8"));
  validateAddonManifest(manifest);
  const routes = addonRoutes(manifest);
  if (options.dryRun) {
    for (const route of routes) console.log(`would upload ${route.key}${route.manifest ? " (last)" : ""}`);
    return;
  }
  if (!process.env.CLOUDFLARE_API_TOKEN) fail("CLOUDFLARE_API_TOKEN is required before R2 mutation");
  for (const route of routes) {
    const source = route.manifest ? path.join(sealed, "addon-manifest.json") : path.join(sealed, route.name);
    await putImmutable({ source, route, repoRoot });
  }
  console.log(`right-release addon upload: verified ${config.addon} ${options.platform} tier=${options.tier}`);
}

async function putImmutable({ source, route, repoRoot }) {
  const target = `rightapps-downloads/${route.key}`;
  const temp = path.join(os.tmpdir(), `right-addon-r2-${process.pid}-${createHash("sha256").update(route.key).digest("hex").slice(0, 12)}`);
  const existing = wrangler(["r2", "object", "get", target, "--file", temp, "--remote"], repoRoot);
  if (existing.status === 0) {
    const actual = sha256(temp); rmSync(temp, { force: true });
    if (actual !== route.sha256) fail(`immutable object exists with different bytes: ${route.key}`);
    return;
  }
  if (!/not found|does not exist|404/i.test(`${existing.stdout}\n${existing.stderr}`)) fail(`cannot inspect immutable object: ${route.key}`);
  const uploaded = wrangler(["r2", "object", "put", target, "--file", source, "--remote"], repoRoot);
  if (uploaded.status !== 0) fail(`immutable upload failed: ${route.key}`);
}

async function adopt(options) {
  if (!options.lock || !options.output) fail("adopt requires --lock and --output");
  const lockPath = path.resolve(options.lock);
  const lock = JSON.parse(readFileSync(lockPath, "utf8"));
  const entry = lock.targets?.[options.platform] ?? lock[options.platform];
  if (!entry?.manifestSha256 || !entry?.manifestUrl) fail("add-on lock needs platform manifestUrl and manifestSha256");
  const temporary = options.source ? null : mkdtempSync(path.join(os.tmpdir(), "right-addon-adopt-"));
  const source = options.source ? path.resolve(options.source) : temporary;
  if (!options.source) await downloadImmutableAddOn(entry.manifestUrl, source);
  const manifest = JSON.parse(readFileSync(path.join(source, "addon-manifest.json"), "utf8"));
  validateAddonManifest(manifest);
  if (addonManifestSha256(manifest) !== entry.manifestSha256) fail("add-on lock manifest SHA mismatch");
  if (!immutableAddonManifestUrl(manifest, entry.manifestUrl, entry.manifestSha256)) fail("add-on lock manifestUrl is not immutable");
  const output = path.resolve(options.output);
  const destination = path.join(output, manifest.addon);
  const stage = `${destination}.tmp-${process.pid}`;
  const adoptedFiles = [];
  rmSync(stage, { recursive: true, force: true }); mkdirSync(stage, { recursive: true });
  for (const file of manifest.files) {
    const sourceFile = path.join(source, file.name);
    if (!existsSync(sourceFile) || statSync(sourceFile).size !== file.sizeBytes || sha256(sourceFile) !== file.sha256) fail(`add-on file verification failed: ${file.name}`);
    verifyAdoptedSignature({ file, sourceFile, platform: manifest.platform, sourceFixture: Boolean(options.source) });
    const subdir = file.role === "command" || file.role === "service" ? "bin" : file.role === "icon" || LEGAL(file.role) ? "resources" : "resources";
    mkdirSync(path.join(stage, subdir), { recursive: true });
    const installed = path.join(stage, subdir, file.name);
    copyFileSync(sourceFile, installed);
    // Tauri resolves externalBin artifacts by Rust target triple. Keep the
    // portable role name too, then provide the required target-qualified alias.
    if (file.role === "command" || file.role === "service") {
      const base = file.name.replace(/\.exe$/i, "");
      const suffix = manifest.platform === "win" ? `${base}-${manifest.targetTriple}.exe` : `${base}-${manifest.targetTriple}`;
      const alias = path.join(stage, "bin", suffix);
      copyFileSync(sourceFile, alias);
      const adoptedSha256 = sha256(alias);
      if (adoptedSha256 !== file.sha256) fail(`adopted alias hash mismatch: ${file.name}`);
      adoptedFiles.push({ role: file.role, name: file.name, sealedSha256: file.sha256, adoptedName: `bin/${suffix}`, adoptedSha256 });
    }
  }
  copyFileSync(path.join(source, "addon-manifest.json"), path.join(stage, "addon-manifest.json"));
  // Directory rename is atomic on one filesystem. Move any previous add-on
  // aside first, restore it if replacement fails, then clean it only after the
  // new verified tree is in place.
  const previous = `${destination}.previous-${process.pid}`;
  mkdirSync(output, { recursive: true });
  if (existsSync(destination)) renameSync(destination, previous);
  try {
    renameSync(stage, destination);
  } catch (error) {
    if (existsSync(previous)) renameSync(previous, destination);
    throw error;
  }
  rmSync(previous, { recursive: true, force: true });
  if (temporary) rmSync(temporary, { recursive: true, force: true });
  const repoRoot = git(path.dirname(lockPath), ["rev-parse", "--show-toplevel"]);
  const receipt = path.join(repoRoot, ".right-release", "addons", "adoptions", `${manifest.addon}-${manifest.platform}.json`);
  mkdirSync(path.dirname(receipt), { recursive: true });
  writeFileSync(receipt, `${JSON.stringify({ schema: 1, addon: manifest.addon, platform: manifest.platform, manifestSha256: entry.manifestSha256, manifestUrl: entry.manifestUrl, output: destination, files: adoptedFiles, adoptedAt: new Date().toISOString() }, null, 2)}\n`);
  console.log(`right-release addon adopt: ${destination}`);
  console.log(`receipt: ${receipt}`);
}

function verifyAdoptedSignature({ file, sourceFile, platform, sourceFixture }) {
  if (!file.executable) return;
  if (file.signing.contract === "test-fixture-v1" && sourceFixture && process.env.RIGHT_RELEASE_TEST_FIXTURE === "1") return;
  if (platform === "mac") {
    if (file.signing.contract !== "apple-developer-id-executable-v1") fail(`unexpected mac signing contract: ${file.name}`);
    const output = command("codesign", ["-dv", "--verbose=4", sourceFile], path.dirname(sourceFile));
    if (!/TeamIdentifier=6KLGD3LLKF/.test(output)) fail(`codesign team mismatch: ${file.name}`);
    run("codesign", ["--verify", "--strict", "--verbose=2", sourceFile], path.dirname(sourceFile));
  } else {
    if (file.signing.contract !== "azure-artifact-signing-v1") fail(`unexpected Windows signing contract: ${file.name}`);
    run(process.execPath, [SIGN_WINDOWS, "--verify-only", sourceFile], path.dirname(sourceFile));
  }
}

function LEGAL(role) { return new Set(["license", "eula", "privacy", "third-party-notices"]).has(role); }
function parse(args) { const value = { platform: process.platform === "win32" ? "win" : "mac", dryRun: false }; for (let i = 0; i < args.length; i += 1) { const key = args[i]; if (key === "--dry-run") value.dryRun = true; else if (["--config", "--platform", "--tier", "--lock", "--output", "--source"].includes(key)) value[key.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = args[++i]; else fail(`unknown argument: ${key}`); } return value; }
function git(cwd, args) { return command("git", args, cwd).trim(); }
function command(cmd, args, cwd) { const result = spawnSync(cmd, args, { cwd, encoding: "utf8", windowsHide: true }); if (result.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed: ${result.stderr || result.stdout}`); return `${result.stdout ?? ""}${result.stderr ?? ""}`; }
function run(cmd, args, cwd) { command(cmd, args, cwd); }
function wrangler(args, cwd) { return spawnSync("pnpm", ["dlx", "wrangler@4", ...args], { cwd, encoding: "utf8", windowsHide: true }); }
function sha256(file) { return createHash("sha256").update(readFileSync(file)).digest("hex"); }
function usage(code) { console.log("usage: right-release addon doctor|build|upload|adopt --platform mac|win ..."); process.exit(code); }
function fail(message) { console.error(`right-release addon: ${message}`); process.exit(1); }
