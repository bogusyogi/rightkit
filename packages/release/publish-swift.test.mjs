import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { DEFAULT_SWIFT_GIT_URL, parseSwiftPublishArgs, buildSwiftPublishPlan, readPackageIdentity } from "./publish-swift.mjs";

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
