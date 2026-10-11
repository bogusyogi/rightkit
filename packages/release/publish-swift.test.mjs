import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { parseSwiftPublishArgs, readPackageIdentity, runSwiftPublish, swiftTagCommands, verifySwiftSourceArchive, SWIFT_DISTRIBUTION_URL } from "./publish-swift.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const realPackageDir = path.resolve(here, "../../swift/RightKitSwift");

test("the only flags are --dry-run, --evidence and --workspace; there is no repo or dirty override", () => {
  assert.deepEqual(parseSwiftPublishArgs(["--dry-run"]), { dryRun: true, evidence: undefined, workspace: undefined });
  assert.deepEqual(parseSwiftPublishArgs(["--evidence", "e.json", "--workspace", "w"]), { dryRun: false, evidence: "e.json", workspace: "w" });
  for (const flag of [["--repo", "https://example.com/r.git"], ["--allow-dirty"], ["--url", "x"]]) {
    assert.throws(() => parseSwiftPublishArgs(flag), /unexpected argument/);
  }
  assert.throws(() => parseSwiftPublishArgs(["--evidence"]), /needs a value/);
});

test("the distribution URL is this repository and the tag commands are plain-semver tag + push", () => {
  assert.equal(SWIFT_DISTRIBUTION_URL, "https://github.com/Orthic-Labs/rightkit");
  assert.deepEqual(swiftTagCommands("0.7.0", "a".repeat(40)), [
    ["git", ["tag", "0.7.0", "a".repeat(40)]],
    ["git", ["push", "origin", "refs/tags/0.7.0:refs/tags/0.7.0"]],
  ]);
});

test("reads the real package identity from VERSION + Package.swift; VERSION must be plain semver", () => {
  const { name, version } = readPackageIdentity(realPackageDir);
  assert.equal(name, "RightKitSwift");
  assert.match(version, /^\d+\.\d+\.\d+$/);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swift-id-"));
  try {
    fs.writeFileSync(path.join(dir, "Package.swift"), 'let package = Package(name: "X")\n');
    for (const bad of ["v1.0.0", "1.0.0-rc.1", "1.0", ""]) {
      fs.writeFileSync(path.join(dir, "VERSION"), `${bad}\n`);
      assert.throws(() => readPackageIdentity(dir), /plain semver/, bad);
    }
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("Swift source-archive digest requires evidence and exact bytes", () => {
  const bytes = Buffer.from("source archive");
  assert.throws(() => verifySwiftSourceArchive(bytes, {}), /evidence is required/);
  assert.throws(() => verifySwiftSourceArchive(bytes, { artifactSha256: "f".repeat(64) }), /does not match/);
  assert.equal(verifySwiftSourceArchive(bytes, { artifactSha256: digestBytes(bytes) }), bytes);
});

const IDENTITY = ["-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid"];

// A bare origin + a working clone laid out like this repository: swift/RightKitSwift/{Package.swift,VERSION}.
function fixture({ version = "1.0.0" } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swift-tag-"));
  const origin = path.join(root, "origin.git");
  const work = path.join(root, "work");
  const git = (args, cwd = work) => {
    const result = spawnSync("git", args, { cwd, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  fs.mkdirSync(origin);
  git(["init", "-q", "--bare", "-b", "main"], origin);
  fs.mkdirSync(path.join(work, "swift", "RightKitSwift"), { recursive: true });
  const packageDir = path.join(work, "swift", "RightKitSwift");
  fs.writeFileSync(path.join(packageDir, "Package.swift"), '// swift-tools-version: 6.1\nimport PackageDescription\nlet package = Package(name: "RightKitSwift")\n');
  fs.writeFileSync(path.join(packageDir, "VERSION"), `${version}\n`);
  git(["init", "-q", "-b", "main"]);
  git(["remote", "add", "origin", origin]);
  git(["add", "."]);
  git([...IDENTITY, "commit", "-q", "-m", "source"]);
  git(["push", "-q", "origin", "main"]);
  const commit = git(["rev-parse", "HEAD"]);
  const tree = git(["rev-parse", "HEAD:swift/RightKitSwift"]);
  const archive = spawnSync("git", ["archive", "--format=tar", "--mtime=1970-01-01T00:00:00Z", tree], { cwd: work }).stdout;
  const evidence = path.join(root, "evidence.json");
  const sha = digestBytes(archive);
  fs.writeFileSync(evidence, JSON.stringify({ schema: 1, kind: "swift", name: "RightKitSwift", version, commit, artifactSha256: sha, runs: [{ host: { os: "darwin", arch: "arm64" }, commit, features: ["default"], artifactSha256: sha, command: "swift test", passed: 1, failed: 0, operator: "fixture", timestamp: "2026-10-08T00:00:00Z" }] }));
  return { root, origin, work, packageDir, evidence, commit, git, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

// Real git; the node --test drift run is stubbed so it never starts a nested test run.
const executor = (driftStatus = 0, seen = []) => (command, args, options = {}) => {
  seen.push([command, args]);
  if (command === process.execPath) return { status: driftStatus, stdout: "", stderr: driftStatus ? "drift" : "" };
  return spawnSync(command, args, options);
};

test("happy path: tags HEAD with the plain VERSION and pushes only that tag to origin", () => {
  const f = fixture();
  try {
    const seen = [];
    const result = runSwiftPublish({ packageDir: f.packageDir, evidence: f.evidence, dryRun: false }, executor(0, seen));
    assert.deepEqual(result, { name: "RightKitSwift", version: "1.0.0", commit: f.commit, published: true });
    assert.equal(f.git(["rev-parse", "1.0.0"]), f.commit);
    assert.match(f.git(["ls-remote", "--tags", "origin", "refs/tags/1.0.0"]), new RegExp(`^${f.commit}\\s+refs/tags/1.0.0$`));
    assert.ok(seen.some(([c, a]) => c === process.execPath && a.join(" ") === `--test ${path.join("scripts", "swift-root-manifest.test.mjs")}`), "drift check ran");
    assert.ok(seen.some(([c, a]) => c === "git" && a.join(" ") === "push origin refs/tags/1.0.0:refs/tags/1.0.0"));
  } finally { f.cleanup(); }
});

test("dry-run checks everything but creates no tag locally or remotely", () => {
  const f = fixture();
  try {
    assert.equal(runSwiftPublish({ packageDir: f.packageDir, evidence: f.evidence, dryRun: true }, executor()).published, false);
    assert.equal(f.git(["tag", "-l"]), "");
    assert.equal(f.git(["ls-remote", "--tags", "origin"]), "");
  } finally { f.cleanup(); }
});

const refusals = {
  "off main": (f) => f.git(["checkout", "-q", "-b", "topic"]),
  "dirty tree": (f) => fs.writeFileSync(path.join(f.work, "stray.txt"), "x"),
  "HEAD not on origin/main": (f) => { fs.writeFileSync(path.join(f.packageDir, "extra.txt"), "x"); f.git(["add", "."]); f.git([...IDENTITY, "commit", "-q", "-m", "unpushed"]); },
  "tag already local": (f) => f.git(["tag", "1.0.0"]),
  "tag already on origin": (f) => { f.git(["tag", "1.0.0"]); f.git(["push", "-q", "origin", "refs/tags/1.0.0"]); f.git(["tag", "-d", "1.0.0"]); },
};
const expected = { "off main": /only on main/, "dirty tree": /not clean/, "HEAD not on origin/main": /not on origin\/main/, "tag already local": /already exists locally/, "tag already on origin": /already exists on origin/ };
for (const [mode, arrange] of Object.entries(refusals)) {
  test(`refuses: ${mode}`, () => {
    const f = fixture();
    try {
      arrange(f);
      const before = f.git(["ls-remote", "--tags", "origin"]);
      assert.throws(() => runSwiftPublish({ packageDir: f.packageDir, evidence: f.evidence, dryRun: false }, executor()), expected[mode]);
      assert.equal(f.git(["ls-remote", "--tags", "origin"]), before, "origin tags unchanged");
    } finally { f.cleanup(); }
  });
}

test("refuses when the root manifest drifted from the nested manifest", () => {
  const f = fixture();
  try {
    assert.throws(() => runSwiftPublish({ packageDir: f.packageDir, evidence: f.evidence, dryRun: false }, executor(1)), /drifted/);
    assert.equal(f.git(["tag", "-l"]), "");
  } finally { f.cleanup(); }
});

test("refuses without publication evidence, and with evidence for another commit or tree", () => {
  const f = fixture();
  try {
    const missing = path.join(f.root, "missing.json");
    assert.throws(() => runSwiftPublish({ packageDir: f.packageDir, evidence: missing, dryRun: false }, executor()), /publication gate/);
    const stale = JSON.parse(fs.readFileSync(f.evidence, "utf8"));
    stale.artifactSha256 = "f".repeat(64);
    stale.runs[0].artifactSha256 = "f".repeat(64);
    fs.writeFileSync(f.evidence, JSON.stringify(stale));
    assert.throws(() => runSwiftPublish({ packageDir: f.packageDir, evidence: f.evidence, dryRun: false }, executor()), /does not match|publication gate/);
    assert.equal(f.git(["tag", "-l"]), "");
  } finally { f.cleanup(); }
});
