import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import path from "node:path";
import test from "node:test";

import { buildArchive, checksum, packageTree, packageVersion } from "./archive-package.mjs";

const PACKAGE_DIR = path.resolve(import.meta.dirname, "../RightKitSwift");
const git = (...args) => execFileSync("git", ["-C", PACKAGE_DIR, ...args], { encoding: "utf8" }).trim();

test("the same commit always produces the same archive bytes", () => {
  const first = buildArchive();
  const second = buildArchive();

  assert.ok(first.length > 0, "archive is empty");
  assert.equal(checksum(first), checksum(second));
  assert.deepEqual(first, second);
});

test("the archive is cut from the commit, not the working tree", () => {
  // A scratch edit must not reach a release. Compare the two most recent commits that changed the
  // package: same tree-ish, same bytes; a different package tree, different bytes.
  const [latest, previous] = git("log", "-2", "--format=%H", "--", ".").split("\n");
  assert.ok(previous, "need two commits touching the package");

  assert.notEqual(packageTree(latest), packageTree(previous));
  assert.notEqual(checksum(buildArchive(latest)), checksum(buildArchive(previous)), "different package trees hashed the same");
  assert.equal(checksum(buildArchive(latest)), checksum(buildArchive(latest)));
});

test("the archive tree is derived from the package prefix, not a hard-coded repository root", () => {
  const prefix = git("rev-parse", "--show-prefix").replace(/\/$/, "");
  assert.equal(packageTree("HEAD"), git("rev-parse", `HEAD:${prefix}`));
});

test("the archive holds the complete package closure of the commit it was cut from", () => {
  const listing = execFileSync("tar", ["-tf", "-"], { input: buildArchive(), maxBuffer: 64 * 1024 * 1024, encoding: "utf8" })
    .split("\n").filter(Boolean);
  const manifest = git("show", `${packageTree()}:Package.swift`);
  assert.ok(listing.includes("RightKitSwift/Package.swift"), "Package.swift missing");
  assert.ok(listing.includes("RightKitSwift/VERSION"), "VERSION missing");

  const targets = [...manifest.matchAll(/\.(?:target|testTarget)\(\s*name:\s*"([^"]+)"/g)].map((match) => match[1]);
  assert.ok(targets.length >= 14, `expected the full target set, found ${targets.length}`);
  for (const name of targets) {
    const root = name.endsWith("Tests") ? "Tests" : "Sources";
    assert.ok(listing.some((entry) => entry.startsWith(`RightKitSwift/${root}/${name}/`)), `${root}/${name} missing from archive`);
  }
});

test("the version is semver", () => {
  assert.match(packageVersion(), /^\d+\.\d+\.\d+$/);
});
