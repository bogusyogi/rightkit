import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { parseSwiftPublishArgs, buildSwiftPublishPlan, readPackageIdentity } from "./publish-swift.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageDir = path.resolve(here, "../../swift/RightKitSwift");

test("dry-run needs no url; publish requires one", () => {
  assert.deepEqual(parseSwiftPublishArgs(["--dry-run"], {}), {
    scope: "rightsuite", url: "", dryRun: true, allowDirty: false,
  });
  assert.throws(() => parseSwiftPublishArgs([], {}), /needs --url/);
  assert.equal(parseSwiftPublishArgs(["--url", "https://reg.local"], {}).url, "https://reg.local");
});

test("env supplies scope and url; --allow-dirty is dry-run only", () => {
  const parsed = parseSwiftPublishArgs([], { RIGHTKIT_SWIFT_REGISTRY_URL: "https://e", RIGHTKIT_SWIFT_REGISTRY_SCOPE: "acme" });
  assert.deepEqual(parsed, { scope: "acme", url: "https://e", dryRun: false, allowDirty: false });
  assert.throws(() => parseSwiftPublishArgs(["--allow-dirty"], { RIGHTKIT_SWIFT_REGISTRY_URL: "https://e" }), /only valid with --dry-run/);
  assert.throws(() => parseSwiftPublishArgs(["--bogus"], {}), /unexpected argument/);
});

test("reads the real package identity from VERSION + Package.swift", () => {
  const { name, version } = readPackageIdentity(packageDir);
  assert.equal(name, "RightKitSwift");
  assert.match(version, /^\d+\.\d+\.\d+/);
});

test("dry-run plan stops before publish; real plan ends with publish", () => {
  const base = { packageDir, scope: "rightsuite", name: "RightKitSwift", version: "0.1.0", url: "https://reg.local" };
  const dry = buildSwiftPublishPlan({ ...base, dryRun: true });
  assert.equal(dry[0].internal, "secret-scan");
  assert.ok(dry.some((s) => s.label === "tests" && s.command === "swift"));
  assert.ok(!dry.some((s) => s.label === "registry publish"), "dry-run must not publish");

  const real = buildSwiftPublishPlan({ ...base, dryRun: false });
  const publish = real.at(-1);
  assert.equal(publish.label, "registry publish");
  assert.deepEqual(publish.args, [
    "package-registry", "publish", "rightsuite.RightKitSwift", "0.1.0", "--url", "https://reg.local", "--package-path", packageDir,
  ]);
});
