// Publish RightKitSwift by the ONE Swift distribution contract: a plain semver git tag on this
// repository (github.com/Orthic-Labs/rightkit), resolved by apps through the root Package.swift
// (docs/RIGHTKIT-SHARED-PACKAGES.md, Swift section). There is no mirror and no second repository.
//
// `right-release publish swift [--dry-run] [--evidence <file>] [--workspace <dir>]` verifies, in order:
//   1. VERSION (swift/RightKitSwift/VERSION) is plain semver (X.Y.Z, no "v", no prerelease);
//   2. the checkout is on main, has a clean tree, and HEAD is reachable from origin/main;
//   3. the root Package.swift and the nested manifest agree (scripts/swift-root-manifest.test.mjs);
//   4. the tag <VERSION> exists neither locally nor on origin;
//   5. the publication gate evidence is bound to HEAD and to the package tree at HEAD;
// then creates the tag on HEAD and pushes it:
//   git tag <VERSION> <HEAD sha>
//   git push origin refs/tags/<VERSION>:refs/tags/<VERSION>
// A dry-run performs every check and prints those commands without running them.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { enforcePublicationGate, defaultEvidencePath } from "./publication-gate.mjs";

export const SWIFT_DISTRIBUTION_URL = "https://github.com/Orthic-Labs/rightkit";
const DRIFT_TEST = path.join("scripts", "swift-root-manifest.test.mjs");

export function parseSwiftPublishArgs(argv) {
  const allowed = new Set(["--dry-run", "--evidence", "--workspace"]);
  let dryRun = false;
  let evidence;
  let workspace;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!allowed.has(arg)) throw new Error(`unexpected argument: ${arg}`);
    if (arg === "--dry-run") { dryRun = true; continue; }
    const value = argv[(index += 1)];
    if (value === undefined) throw new Error(`${arg} needs a value`);
    if (arg === "--evidence") evidence = value;
    else workspace = value;
  }
  return { dryRun, evidence, workspace };
}

// VERSION is the single source of truth; the tag is exactly its text.
export function readPackageIdentity(packageDir) {
  const manifest = path.join(packageDir, "Package.swift");
  if (!fs.existsSync(manifest)) throw new Error(`Package.swift not found: ${manifest}`);
  const versionFile = path.join(packageDir, "VERSION");
  if (!fs.existsSync(versionFile)) throw new Error(`VERSION not found: ${versionFile}`);
  const version = fs.readFileSync(versionFile, "utf8").trim();
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`VERSION is not plain semver (X.Y.Z): "${version}"`);
  const nameMatch = fs.readFileSync(manifest, "utf8").match(/name:\s*"([^"]+)"/);
  if (!nameMatch) throw new Error("could not read package name from Package.swift");
  return { name: nameMatch[1], version };
}

// The exact commands a real publish runs after every check passes.
export function swiftTagCommands(version, sha) {
  return [
    ["git", ["tag", version, sha]],
    ["git", ["push", "origin", `refs/tags/${version}:refs/tags/${version}`]],
  ];
}

function git(args, cwd, execute) {
  const result = execute("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${(result.stderr ?? "").trim()}`);
  return (result.stdout ?? "").trim();
}

// Evidence binds this exact git-archive of the package tree at HEAD.
export function verifySwiftSourceArchive(bytes, evidence) {
  if (!/^[a-f0-9]{64}$/.test(evidence?.artifactSha256 ?? "")) throw new Error("Swift source archive hash evidence is required");
  if (digestBytes(bytes) !== evidence.artifactSha256) throw new Error("Swift source archive sha256 does not match evidence artifactSha256");
  return bytes;
}

export function runSwiftPublish(options, execute = spawnSync) {
  const packageDir = options.packageDir;
  const { name, version } = readPackageIdentity(packageDir);
  const toplevel = git(["rev-parse", "--show-toplevel"], packageDir, execute);
  const out = (message) => process.stdout.write(`[right-release] swift ${message}\n`);

  const branch = git(["symbolic-ref", "--short", "-q", "HEAD"], toplevel, execute);
  if (branch !== "main") throw new Error(`publish swift runs only on main (current: ${branch || "detached HEAD"})`);
  if (git(["status", "--porcelain", "--untracked-files=normal"], toplevel, execute) !== "") throw new Error("working tree is not clean: commit or remove changes before tagging");
  git(["fetch", "--quiet", "origin", "main"], toplevel, execute);
  const sha = git(["rev-parse", "HEAD"], toplevel, execute);
  const ancestor = execute("git", ["merge-base", "--is-ancestor", sha, "origin/main"], { cwd: toplevel, encoding: "utf8", windowsHide: true });
  if (ancestor.error) throw ancestor.error;
  if (ancestor.status !== 0) throw new Error(`HEAD ${sha} is not on origin/main: push it and let CI pass first`);
  out("branch, clean tree, HEAD on origin/main: ok");

  const drift = execute(process.execPath, ["--test", DRIFT_TEST], { cwd: toplevel, encoding: "utf8", windowsHide: true });
  if (drift.error) throw drift.error;
  if (drift.status !== 0) throw new Error(`root Package.swift drifted from swift/RightKitSwift/Package.swift:\n${drift.stdout ?? ""}${drift.stderr ?? ""}`);
  out("root manifest matches nested manifest: ok");

  if (git(["tag", "-l", version], toplevel, execute) !== "") throw new Error(`tag ${version} already exists locally: bump VERSION`);
  if (git(["ls-remote", "--tags", "origin", `refs/tags/${version}`], toplevel, execute) !== "") throw new Error(`tag ${version} already exists on origin: bump VERSION`);
  out(`tag ${version} is free: ok`);

  const workspaceRoot = path.resolve(options.workspace ?? path.join(packageDir, "..", ".."));
  const gate = enforcePublicationGate({
    kind: "swift", name, version, packagePath: packageDir,
    evidencePath: path.resolve(options.evidence ?? defaultEvidencePath(workspaceRoot, "swift", name)),
  }, { dryRun: options.dryRun });
  if (gate.ok) {
    if (gate.evidence?.commit !== sha) throw new Error(`publication evidence commit ${gate.evidence?.commit} is not HEAD ${sha}`);
    const prefix = git(["rev-parse", "--show-prefix"], packageDir, execute).replace(/\/$/, "");
    const tree = git(["rev-parse", prefix ? `${sha}:${prefix}` : `${sha}^{tree}`], packageDir, execute);
    // `git archive` filters by the current subdirectory, so archive the tree id from the toplevel.
    const archive = execute("git", ["archive", "--format=tar", "--mtime=1970-01-01T00:00:00Z", tree], { cwd: toplevel, windowsHide: true, maxBuffer: 128 * 1024 * 1024 });
    if (archive.error || archive.status !== 0 || !archive.stdout?.length) throw new Error("Swift package tree archive creation failed");
    verifySwiftSourceArchive(Buffer.from(archive.stdout), gate.evidence);
  }

  const commands = swiftTagCommands(version, sha);
  if (options.dryRun) {
    for (const [command, args] of commands) out(`dry-run would run: ${command} ${args.join(" ")}`);
    return { name, version, commit: sha, published: false };
  }
  const [tagCommand, pushCommand] = commands;
  git(tagCommand[1], toplevel, execute);
  try { git(pushCommand[1], toplevel, execute); }
  catch (error) { git(["tag", "-d", version], toplevel, execute); throw error; }
  const remote = git(["ls-remote", "--tags", "origin", `refs/tags/${version}`], toplevel, execute);
  if (!remote.startsWith(sha)) throw new Error(`origin does not show tag ${version} at ${sha}`);
  out(`tagged ${version} at ${sha} on ${SWIFT_DISTRIBUTION_URL}`);
  return { name, version, commit: sha, published: true };
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    const options = parseSwiftPublishArgs(process.argv.slice(2));
    const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../swift/RightKitSwift");
    runSwiftPublish({ ...options, packageDir });
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
