// rightkit#319: every @rightkit/release must accept its own version. The app contract check
// (validateRightKitPackageContract) reads rightkit-versions.json shipped inside the package, so a feed that
// only records the PREVIOUS release rejects the release that ships it.
import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { rightKitVersionAllowed } from "./rightkit-version-policy.mjs";
import { stageSourceVersions } from "./rightkit-sync.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const readJson = (file) => JSON.parse(readFileSync(file, "utf8"));

// The same union release.mjs builds as RIGHTKIT_ALLOWED["@rightkit/release"].
function allowedFor(feed, name) {
  const allowed = new Set();
  for (const value of [feed.npm?.[name], feed.stagedNpm?.[name], feed.legacyNpm?.[name], feed.registry?.npm?.[name]]) {
    for (const version of Array.isArray(value) ? value : [value]) if (version) allowed.add(version);
  }
  return allowed;
}

test("package.json version is in RIGHTKIT_ALLOWED for @rightkit/release", () => {
  const { version } = readJson(path.join(here, "package.json"));
  const allowed = allowedFor(readJson(path.join(here, "rightkit-versions.json")), "@rightkit/release");
  assert.ok(allowed.has(version), `@rightkit/release ${version} is not listed in rightkit-versions.json (${[...allowed].slice(-3).join(", ")}); stage it before packing`);
  assert.ok(rightKitVersionAllowed(allowed, version));
});

test("stageSourceVersions lists a release's own version in the feed it ships, keeping the published pin", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-self-version-"));
  try {
    const release = path.join(root, "packages", "release");
    const policy = path.join(root, "packages", "build-control-policy", "src");
    mkdirSync(release, { recursive: true });
    mkdirSync(policy, { recursive: true });
    for (const file of ["rightkit-ownership.json", "ownership-repositories.json", "rightkit-drift.mjs", "background-process.mjs", "ownership-dependency-allowlist.json", "ownership-dependency-allowlist.mjs", "ownership-source.mjs", "ownership-availability.mjs"]) cpSync(path.join(here, file), path.join(release, file));
    const feedFile = path.join(release, "rightkit-versions.json");
    const stale = { npm: {}, stagedNpm: { "@rightkit/release": "0.2.10" }, legacyNpm: {}, registry: { npm: { "@rightkit/release": "0.2.10" }, cargo: {} } };
    writeFileSync(feedFile, `${JSON.stringify(stale, null, 2)}\n`);
    assert.ok(!allowedFor(readJson(feedFile), "@rightkit/release").has("0.2.11"));
    stageSourceVersions([{ registry: "npm", name: "@rightkit/release", version: "0.2.11" }], { file: feedFile, root, log: () => {} });
    const allowed = allowedFor(readJson(feedFile), "@rightkit/release");
    assert.ok(allowed.has("0.2.11"), "own version is listed");
    assert.ok(allowed.has("0.2.10"), "the published pin stays allowed");
    // Never lowers a staged version.
    stageSourceVersions([{ registry: "npm", name: "@rightkit/release", version: "0.2.9" }], { file: feedFile, root, log: () => {} });
    assert.equal(readJson(feedFile).stagedNpm["@rightkit/release"], "0.2.11");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
