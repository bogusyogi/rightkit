import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import { spawnSync } from "./background-process.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { runSwiftPublish, verifySwiftSourceArchive, DEFAULT_SWIFT_GIT_URL, parseSwiftPublishArgs, buildSwiftPublishPlan, readPackageIdentity } from "./publish-swift.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageDir = path.resolve(here, "../../swift/RightKitSwift");

test("dry-run needs nothing; the repo defaults to the pinned distribution URL", () => {
  assert.deepEqual(parseSwiftPublishArgs(["--dry-run"], {}), {
    repo: DEFAULT_SWIFT_GIT_URL, dryRun: true, allowDirty: false, evidence: undefined, workspace: undefined,
  });
  assert.equal(parseSwiftPublishArgs(["--repo", "https://example.com/r.git"], {}).repo, "https://example.com/r.git");
  assert.throws(() => parseSwiftPublishArgs(["--repo", "ftp://nope"], {}), /invalid swift git url/);
});

test("env supplies the repo; --allow-dirty is dry-run only; registry flags are gone", () => {
  assert.equal(parseSwiftPublishArgs([], { RIGHTKIT_SWIFT_GIT_URL: "https://e/r.git" }).repo, "https://e/r.git");
  assert.throws(() => parseSwiftPublishArgs(["--allow-dirty"], {}), /only valid with --dry-run/);
  assert.throws(() => parseSwiftPublishArgs(["--url", "https://reg.local"], {}), /unexpected argument/);
});

test("reads the real package identity from VERSION + Package.swift", () => {
  const { name, version } = readPackageIdentity(packageDir);
  assert.equal(name, "RightKitSwift");
  assert.match(version, /^\d+\.\d+\.\d+/);
});

test("dry-run and real plans both prove exact-pin consumer resolution; only real pushes", () => {
  const base = { packageDir, name: "RightKitSwift", version: "0.3.0", repo: DEFAULT_SWIFT_GIT_URL };
  const dry = buildSwiftPublishPlan({ ...base, dryRun: true });
  assert.equal(dry[0].internal, "secret-scan");
  assert.ok(dry.some((s) => s.label === "tests" && s.command === "swift"));
  assert.ok(dry.some((s) => s.internal === "consume" && s.tag === "0.3.0"), "dry-run resolves a fresh consumer");
  assert.ok(!dry.some((s) => s.internal === "push"), "dry-run must not push");
  assert.equal(dry.find((s) => s.internal === "mirror").repo, null, "dry-run never touches the network");

  const real = buildSwiftPublishPlan({ ...base, dryRun: false });
  assert.equal(real.at(-1).internal, "push");
  assert.equal(real.find((s) => s.internal === "mirror").repo, DEFAULT_SWIFT_GIT_URL);
  assert.ok(real.findIndex((s) => s.internal === "consume") < real.findIndex((s) => s.internal === "push"), "resolve before push");
});

for (const mode of ["unsealed", "mismatch", "swap", "happy", "mutated snapshot", "mutated tag"]) {
  test(`publish swift: ${mode} binds archive evidence to immutable pushed tree`, () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-swift-bytes-"));
    const source = path.join(root, "source"); fs.mkdirSync(source);
    fs.writeFileSync(path.join(source, "Package.swift"), '// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: "RightKitSwift")\n');
    fs.writeFileSync(path.join(source, "VERSION"), '1.0.0\n');
    const git = (args, cwd = source) => {
      const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
      assert.equal(result.status, 0, result.stderr); return result.stdout.trim();
    };
    git(['init', '-q', '-b', 'main']); git(['add', '.']);
    git(['-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-q', '-m', 'source']);
    const tree = git(['rev-parse', 'HEAD^{tree}']), commit = git(['rev-parse', 'HEAD']);
    const archive = spawnSync('git', ['archive', '--format=tar', '--mtime=1970-01-01T00:00:00Z', tree], { cwd: source }).stdout;
    const evidence = path.join(root, 'evidence.json');
    if (mode !== 'unsealed') fs.writeFileSync(evidence, JSON.stringify({ schema: 1, kind: 'swift', name: 'RightKitSwift', version: '1.0.0', commit, artifactSha256: mode === 'mismatch' ? 'f'.repeat(64) : digestBytes(archive), runs: [{ host: { os: 'darwin' }, commit, command: 'swift test', passed: 1, failed: 0, operator: 'fixture', timestamp: '2026-10-08T00:00:00Z' }] }));
    const pushes = [], calls = [];
    let mirror;
    const execute = (command, args, options = {}) => {
      calls.push([command, args]);
      if (command === 'git' && args[0] === 'archive') {
        const result = spawnSync(command, args, options);
        if (mode === 'swap') fs.writeFileSync(path.join(source, 'VERSION'), '9.9.9\n');
        return result;
      }
      if (command === 'git' && args[0] === 'clone') {
        mirror = args[2]; fs.mkdirSync(mirror); git(['init', '-q', '-b', 'main'], mirror); return { status: 0, stdout: '' };
      }
      if (command === 'git' && args[0] === 'push') { pushes.push(args); return { status: 0, stdout: '' }; }
      if (command === 'swift') {
        if (args[0] === 'test' && mode === 'mutated snapshot') fs.writeFileSync(path.join(options.cwd, 'VERSION'), '9.9.9\n');
        if (args[0] === 'package') {
          fs.writeFileSync(path.join(options.cwd, 'Package.resolved'), '{"version" : "1.0.0"}');
          if (mode === 'mutated tag') {
            fs.writeFileSync(path.join(mirror, 'VERSION'), '9.9.9\n'); git(['add', '.'], mirror);
            git(['-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-q', '-m', 'changed'], mirror);
            git(['tag', '-f', '1.0.0'], mirror);
          }
        }
        return { status: 0 };
      }
      return spawnSync(command, args, options);
    };
    try {
      const invoke = () => runSwiftPublish({ packageDir: source, repo: 'https://example.invalid/rightkit-swift.git', evidence, dryRun: false }, execute);
      if (['happy', 'swap'].includes(mode)) {
        assert.equal(invoke().published, true);
        assert.equal(pushes.length, 1); assert.equal(pushes[0][1], '--atomic');
        assert.match(pushes[0][3], /^[a-f0-9]{40}:refs\/heads\/main$/);
        assert.equal(pushes[0][3].slice(0, 40), pushes[0][4].slice(0, 40));
      } else {
        assert.throws(invoke, /publication gate|archive sha256|mirror differs|tag changed/);
        assert.equal(pushes.length, 0);
        if (['unsealed', 'mismatch'].includes(mode)) assert.equal(calls.some(([command]) => command === 'swift'), false);
      }
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });
}
test('Swift source-archive digest requires evidence and exact bytes', () => {
  const bytes = Buffer.from('source archive');
  assert.throws(() => verifySwiftSourceArchive(bytes, {}), /evidence is required/);
  assert.throws(() => verifySwiftSourceArchive(bytes, { artifactSha256: 'f'.repeat(64) }), /does not match/);
  assert.equal(verifySwiftSourceArchive(bytes, { artifactSha256: digestBytes(bytes) }), bytes);
});
