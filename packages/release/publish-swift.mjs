// Publish RightKitSwift by the ONE Swift distribution contract: a git repository pinned by URL +
// exact version tag (docs/RIGHTKIT-SHARED-PACKAGES.md, Swift section). Gates, in order:
// secret scan -> swift test -> release build -> mirror tree into a git checkout + tag ->
// FRESH CONSUMER RESOLUTION (a throwaway app pins the tag with `exact:` and runs
// `swift package resolve`) -> push. A dry-run performs everything except the push, so a green
// dry-run proves the mirror layout, the tag, and clean consumer resolution -- not just that tests passed.
// swift-tools is macOS/Linux only, so the plan builder, arg parser, and version check are
// pure and unit-tested on any OS (publish-swift.test.mjs); the swift steps run where swift exists.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { scanCrateForSecrets } from "./publish-cargo.mjs";
import { enforcePublicationGate, defaultEvidencePath } from "./publication-gate.mjs";

export const DEFAULT_SWIFT_GIT_URL = "https://github.com/bogusyogi/rightkit-swift.git";
// The mirror must reproduce the verified git subtree exactly (its tree id is
// compared), so only untracked build state is excluded. Tracked Xcode scheme
// metadata under .swiftpm ships with the package; SwiftPM consumers ignore it.
const MIRROR_EXCLUDES = new Set([".build", ".git", "Package.resolved", ".DS_Store"]);

export function parseSwiftPublishArgs(argv, env = process.env) {
  const allowed = new Set(["--repo", "--dry-run", "--allow-dirty", "--evidence", "--workspace"]);
  let repo = env.RIGHTKIT_SWIFT_GIT_URL ?? DEFAULT_SWIFT_GIT_URL;
  let dryRun = false;
  let allowDirty = false;
  let evidence;
  let workspace;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!allowed.has(arg)) throw new Error(`unexpected argument: ${arg}`);
    if (arg === "--dry-run") dryRun = true;
    else if (arg === "--allow-dirty") allowDirty = true;
    else {
      const value = argv[(index += 1)];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      if (arg === "--evidence") evidence = value;
      else if (arg === "--workspace") workspace = value;
      else repo = value;
    }
  }
  if (!/^(https:\/\/|git@|ssh:\/\/|file:\/\/)/.test(repo)) throw new Error(`invalid swift git url: ${repo}`);
  if (allowDirty && !dryRun) throw new Error("--allow-dirty is only valid with --dry-run");
  return { repo, dryRun, allowDirty, evidence, workspace };
}

// The VERSION file is the single source of truth; Package.swift must name the package and the
// version must be a clean semver so the registry archive is reproducible.
export function readPackageIdentity(packageDir) {
  const manifest = path.join(packageDir, "Package.swift");
  if (!fs.existsSync(manifest)) throw new Error(`Package.swift not found: ${manifest}`);
  const versionFile = path.join(packageDir, "VERSION");
  if (!fs.existsSync(versionFile)) throw new Error(`VERSION not found: ${versionFile}`);
  const version = fs.readFileSync(versionFile, "utf8").trim();
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`VERSION is not clean semver: "${version}"`);
  }
  const nameMatch = fs.readFileSync(manifest, "utf8").match(/name:\s*"([^"]+)"/);
  if (!nameMatch) throw new Error("could not read package name from Package.swift");
  return { name: nameMatch[1], version };
}

export function buildSwiftPublishPlan({ packageDir, name, version, repo, dryRun }) {
  const plan = [
    { label: "secret scan", internal: "secret-scan" },
    { label: "tests", command: "swift", args: ["test", "--package-path", packageDir] },
    { label: "release build", command: "swift", args: ["build", "-c", "release", "--package-path", packageDir] },
    // Dry-run mirrors into a fresh local repo; a real publish clones the distribution repo first.
    { label: "mirror and tag", internal: "mirror", tag: version, repo: dryRun ? null : repo },
    { label: "fresh consumer resolution (exact pin)", internal: "consume", tag: version, name },
  ];
  if (!dryRun) plan.push({ label: "git push (branch and tag)", internal: "push", tag: version, repo });
  return plan;
}

function git(args, cwd, execute = spawnSync) {
  const result = execute("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${(result.stderr ?? "").trim()}`);
  return (result.stdout ?? "").trim();
}

function copyTree(from, to) {
  fs.mkdirSync(to, { recursive: true });
  for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
    if (MIRROR_EXCLUDES.has(entry.name)) continue;
    const source = path.join(from, entry.name);
    const target = path.join(to, entry.name);
    if (entry.isDirectory()) copyTree(source, target);
    else if (entry.isFile()) { fs.copyFileSync(source, target); fs.chmodSync(target, fs.statSync(source).mode & 0o777); }
    else throw new Error(`unsupported Swift source entry: ${entry.name}`);
  }
}

function mirrorAndTag(step, packageDir, work, execute) {
  const mirror = path.join(work, "rightkit-swift"); // SwiftPM names the package by repo directory
  if (step.repo) git(["clone", step.repo, mirror], work, execute);
  else { fs.mkdirSync(mirror, { recursive: true }); git(["init", "-q", "-b", "main"], mirror, execute); }
  for (const entry of fs.readdirSync(mirror)) if (entry !== ".git") fs.rmSync(path.join(mirror, entry), { recursive: true, force: true });
  copyTree(packageDir, mirror);
  git(["add", "-A"], mirror, execute);
  git(["-c", "user.email=release@rightsuite.invalid", "-c", "user.name=right-release", "commit", "-q", "--allow-empty", "-m", `RightKitSwift ${step.tag}`], mirror, execute);
  if (git(["tag", "-l", step.tag], mirror, execute) !== "") throw new Error(`tag ${step.tag} already exists: bump VERSION`);
  git(["tag", step.tag], mirror, execute);
  return mirror;
}

function consumeFresh(step, mirror, work, execute) {
  const app = path.join(work, "consumer");
  fs.mkdirSync(path.join(app, "Sources", "App"), { recursive: true });
  fs.writeFileSync(path.join(app, "Package.swift"), [
    "// swift-tools-version: 6.0", "import PackageDescription", "let package = Package(",
    '  name: "App", platforms: [.iOS(.v17), .macOS(.v14)],',
    `  dependencies: [.package(url: "file://${mirror}", exact: "${step.tag}")],`,
    '  targets: [.target(name: "App", dependencies: [.product(name: "RightKitStore", package: "rightkit-swift")])]', ")", "",
  ].join("\n"));
  fs.writeFileSync(path.join(app, "Sources", "App", "App.swift"), "import RightKitStore\npublic let ok = true\n");
  const result = execute("swift", ["package", "resolve"], { cwd: app, encoding: "utf8", windowsHide: true, stdio: "inherit" });
  if (result.error || result.status !== 0) throw new Error("fresh consumer resolution failed");
  const pins = fs.readFileSync(path.join(app, "Package.resolved"), "utf8");
  if (!pins.includes(`"version" : "${step.tag}"`)) throw new Error(`consumer did not resolve exact tag ${step.tag}`);
}

// Evidence binds this exact git-archive command, rooted at the package tree.
export function verifySwiftSourceArchive(bytes, evidence) {
  if (!/^[a-f0-9]{64}$/.test(evidence?.artifactSha256 ?? "")) throw new Error("Swift source archive hash evidence is required");
  if (digestBytes(bytes) !== evidence.artifactSha256) throw new Error("Swift source archive sha256 does not match evidence artifactSha256");
  return bytes;
}

export function runSwiftPublish(options, execute = spawnSync) {
  const packageDir = options.packageDir;
  const { name, version } = readPackageIdentity(packageDir);
  const workspaceRoot = path.resolve(options.workspace ?? path.join(packageDir, "..", ".."));
  const gate = enforcePublicationGate({
    kind: "swift", name, version, packagePath: packageDir,
    evidencePath: path.resolve(options.evidence ?? defaultEvidencePath(workspaceRoot, "swift", name)),
  }, { dryRun: options.dryRun });
  const prefix = git(["rev-parse", "--show-prefix"], packageDir, execute).replace(/\/$/, "");
  const sourceCommit = gate.evidence?.commit ?? "HEAD";
  const tree = git(["rev-parse", prefix ? `${sourceCommit}:${prefix}` : `${sourceCommit}^{tree}`], packageDir, execute);
  // `git archive` filters by the current subdirectory, so archiving the package
  // subtree from inside the package yields an empty tar; run it at the toplevel.
  const toplevel = git(["rev-parse", "--show-toplevel"], packageDir, execute) || packageDir;
  const archive = execute("git", ["archive", "--format=tar", "--mtime=1970-01-01T00:00:00Z", tree], { cwd: toplevel, windowsHide: true, maxBuffer: 128 * 1024 * 1024 });
  if (archive.error || archive.status !== 0 || !archive.stdout?.length) throw new Error("Swift source archive creation failed");
  const bytes = Buffer.from(archive.stdout);
  if (gate.ok || !options.dryRun) verifySwiftSourceArchive(bytes, gate.evidence);
  const plan = buildSwiftPublishPlan({ ...options, packageDir, name, version });
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-swift-publish-"));
  let mirror = null;
  let sealedCommit;
  const snapshot = path.join(work, "source");
  fs.mkdirSync(snapshot);
  try {
    const extracted = execute("tar", ["-xf", "-", "-C", snapshot], { input: bytes, windowsHide: true });
    if (extracted.error || extracted.status !== 0) throw new Error("Swift source archive extraction failed");
    const identity = readPackageIdentity(snapshot);
    if (identity.name !== name || identity.version !== version) throw new Error("Swift source archive identity mismatch");
    for (const step of plan) {
      process.stdout.write(`[right-release] swift ${step.label}\n`);
      if (step.internal === "secret-scan") {
        const violations = scanCrateForSecrets(snapshot);
        if (violations.length) throw new Error(`secret scan rejected: ${violations.join(", ")}`);
        continue;
      }
      if (step.internal === "mirror") { mirror = mirrorAndTag(step, snapshot, work, execute);
        if (git(["rev-parse", `${step.tag}^{tree}`], mirror, execute) !== tree) throw new Error("Swift mirror differs from verified source archive");
        sealedCommit = git(["rev-parse", step.tag], mirror, execute);
        continue; }
      if (step.internal === "consume") { consumeFresh(step, mirror, work, execute); continue; }
      if (step.internal === "push") {
        if (git(["rev-parse", `${sealedCommit}^{tree}`], mirror, execute) !== tree || git(["rev-parse", step.tag], mirror, execute) !== sealedCommit) throw new Error("Swift publication tag changed after verification");
        const branch = git(["symbolic-ref", "--short", "HEAD"], mirror, execute);
        git(["push", "--atomic", "origin", `${sealedCommit}:refs/heads/${branch}`, `${sealedCommit}:refs/tags/${step.tag}`], mirror, execute);
        continue;
      }
      const result = execute(step.command, step.args.map((arg) => arg === packageDir ? snapshot : arg), { cwd: snapshot, encoding: "utf8", windowsHide: true, stdio: "inherit" });
      if (result.error) {
        if (result.error.code === "ENOENT") {
          throw new Error(`${step.command} not found -- run publish swift on macOS/Linux with the Swift toolchain installed`);
        }
        throw result.error;
      }
      if (result.status !== 0) throw new Error(`${step.label} failed with exit code ${result.status}`);
    }
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
  return { name, version, published: !options.dryRun };
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    const options = parseSwiftPublishArgs(process.argv.slice(2));
    const packageDir = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, "$1")), "../../swift/RightKitSwift");
    runSwiftPublish({ ...options, packageDir });
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
