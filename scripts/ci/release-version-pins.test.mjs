import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { satisfiesNpm, versionPinProblems } from "./release-version-pins.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..");
const LIVE = "/Volumes/D/claude/rightkit";
const FILES = ["packages/release/package.json", "packages/release/rightkit-versions.json", "packages/build-control-policy/src/ownership-session-versions.json", "ci/rk-pipeline.json", "packages/legal/package.json", "ci/consumers/npm/release/consumer.mjs"];

function tree(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pins-"));
  for (const [rel, text] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); fs.writeFileSync(path.join(root, rel), text); }
  return root;
}
/** The pin-relevant files of the live repository at <sha>, or null when this machine does not have that commit. */
function replay(sha) {
  if (spawnSync("git", ["-C", LIVE, "cat-file", "-e", `${sha}^{commit}`]).status !== 0) return null;
  const files = {};
  for (const f of FILES) {
    const r = spawnSync("git", ["-C", LIVE, "show", `${sha}:${f}`], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    assert.equal(r.status, 0, `${sha}:${f}`);
    files[f] = r.stdout;
  }
  return files;
}

test("npm ranges: caret/tilde/comparators/alternatives; unreadable ranges fail closed", () => {
  assert.equal(satisfiesNpm("0.6.3", "^0.4.1 || ^0.5.0 || ^0.6.0"), true);
  assert.equal(satisfiesNpm("0.7.0", "^0.4.1 || ^0.5.0 || ^0.6.0"), false);
  assert.equal(satisfiesNpm("0.6.3", "^0.6.4"), false);
  assert.equal(satisfiesNpm("1.4.0", "^1.2.3"), true);
  assert.equal(satisfiesNpm("2.0.0", "^1.2.3"), false);
  assert.equal(satisfiesNpm("0.0.3", "^0.0.3"), true);
  assert.equal(satisfiesNpm("0.0.4", "^0.0.3"), false);
  assert.equal(satisfiesNpm("1.2.9", "~1.2.3"), true);
  assert.equal(satisfiesNpm("1.3.0", "~1.2.3"), false);
  assert.equal(satisfiesNpm("1.5.0", ">=1.2.0 <2.0.0"), true);
  assert.equal(satisfiesNpm("2.0.0", ">=1.2.0 <2.0.0"), false);
  assert.equal(satisfiesNpm("1.2.3", "1.2.3"), true);
  assert.equal(satisfiesNpm("1.2.4", "1.2.3"), false);
  assert.equal(satisfiesNpm("1.2.4", "1.2.x"), true);
  assert.equal(satisfiesNpm("0.6.3", "workspace:*"), true);
  assert.equal(satisfiesNpm("0.6.3", "not a range"), false);
  assert.equal(satisfiesNpm("0.6.3-beta.1", "^0.6.0"), false, "a prerelease never satisfies a plain range");
});

test("the committed tree has no version-pin problem for @rightkit/release or any other workspace package", () => {
  assert.deepEqual(versionPinProblems(REPO, "@rightkit/release"), []);
  for (const dir of fs.readdirSync(path.join(REPO, "packages"))) {
    const file = path.join(REPO, "packages", dir, "package.json");
    if (!fs.existsSync(file)) continue;
    const name = JSON.parse(fs.readFileSync(file, "utf8")).name;
    assert.deepEqual(versionPinProblems(REPO, name), [], name);
  }
});

test("the release consumer carries no version literal; the version arrives through RIGHTKIT_CONSUMER_EXPECTED_VERSION", () => {
  const text = fs.readFileSync(path.join(REPO, "ci/consumers/npm/release/consumer.mjs"), "utf8");
  assert.doesNotMatch(text, /assert\.equal\(version, '\d+\.\d+\.\d+'\)/);
  assert.match(text, /required\('RIGHTKIT_CONSUMER_EXPECTED_VERSION'\)/);
  assert.match(text, /assert\.equal\(installed\.version, version\)/);
});

test("replay 035a439e: the 0.6.3 bump left the version feed unstaged", (t) => {
  const files = replay("035a439e");
  if (!files) return t.skip("commit not available on this machine");
  const problems = versionPinProblems(tree(files), "@rightkit/release");
  assert.ok(problems.some((p) => /stagedNpm\[@rightkit\/release\] is "0\.6\.2" but .* is 0\.6\.3/.test(p)), problems.join("\n"));
});

test("replay 85ea11d4: the feed is staged but the consumer recipe still asserts 0.6.2", (t) => {
  const files = replay("85ea11d4");
  if (!files) return t.skip("commit not available on this machine");
  const problems = versionPinProblems(tree(files), "@rightkit/release");
  assert.ok(problems.some((p) => /consumer\.mjs hard-codes version 0\.6\.2 but .* is 0\.6\.3/.test(p)), problems.join("\n"));
  assert.ok(!problems.some((p) => /stagedNpm/.test(p)), "the feed was staged at 85ea11d4");
});

test("each pin is checked on its own: ownership copy, dispatch exception, dependent range, missing files", () => {
  const good = () => ({
    "packages/release/package.json": JSON.stringify({ name: "@rightkit/release", version: "0.6.3" }),
    "packages/release/rightkit-versions.json": '{"stagedNpm":{"@rightkit/release":"0.6.3"}}\n',
    "packages/build-control-policy/src/ownership-session-versions.json": '{"stagedNpm":{"@rightkit/release":"0.6.3"}}\n',
    "ci/rk-pipeline.json": JSON.stringify({ dispatch: { exceptions: { "@rightkit/release": { version: "0.6.3" } } } }),
    "packages/legal/package.json": JSON.stringify({ name: "@rightkit/legal", version: "1.0.0", peerDependencies: { "@rightkit/release": "^0.5.0 || ^0.6.0" } }),
  });
  assert.deepEqual(versionPinProblems(tree(good()), "@rightkit/release"), []);
  assert.match(versionPinProblems(tree({ ...good(), "packages/build-control-policy/src/ownership-session-versions.json": '{"stagedNpm":{"@rightkit/release":"0.6.3"}} \n' }), "@rightkit/release").join("\n"), /not byte-identical/);
  assert.match(versionPinProblems(tree({ ...good(), "ci/rk-pipeline.json": JSON.stringify({ dispatch: { exceptions: { "@rightkit/release": { version: "0.6.2" } } } }) }), "@rightkit/release").join("\n"), /dispatch\.exceptions\[@rightkit\/release\]\.version is "0\.6\.2"/);
  assert.match(versionPinProblems(tree({ ...good(), "packages/legal/package.json": JSON.stringify({ name: "@rightkit/legal", version: "1.0.0", peerDependencies: { "@rightkit/release": "^0.4.1 || ^0.5.0" } }) }), "@rightkit/release").join("\n"), /packages\/legal\/package\.json peerDependencies.* does not include 0\.6\.3/);
  const noFeed = good(); delete noFeed["packages/release/rightkit-versions.json"];
  assert.match(versionPinProblems(tree(noFeed), "@rightkit/release").join("\n"), /rightkit-versions\.json cannot be read/);
  assert.match(versionPinProblems(tree({}), "@rightkit/release").join("\n"), /packages\/ is missing|not a workspace package/);
});

test("cargo units: the consumer's =x.y.z and the version feed must both equal the crate version", () => {
  const files = (pin, staged = "0.4.7") => ({
    "crates/rightkit-fs/Cargo.toml": '[package]\nname = "rightkit-fs"\nversion = "0.4.7"\n',
    "ci/consumers/rightkit-fs/Cargo.toml": `[dependencies]\nrightkit-fs = { version = "=${pin}" }\n`,
    "packages/release/rightkit-versions.json": JSON.stringify({ cargo: { "rightkit-fs": "0.1.0" }, stagedCargo: staged ? { "rightkit-fs": staged } : {} }),
  });
  assert.deepEqual(versionPinProblems(tree(files("0.4.7")), "rightkit-fs"), []);
  assert.match(versionPinProblems(tree(files("0.4.6")), "rightkit-fs").join("\n"), /pins =0\.4\.6 but crates\/rightkit-fs\/Cargo\.toml is 0\.4\.7/);
  assert.match(versionPinProblems(tree(files("0.4.7", "0.4.6")), "rightkit-fs").join("\n"), /lists rightkit-fs 0\.4\.6 \(stagedCargo\) but crates\/rightkit-fs\/Cargo\.toml is 0\.4\.7/);
  assert.match(versionPinProblems(tree(files("0.4.7", null)), "rightkit-fs").join("\n"), /lists rightkit-fs 0\.1\.0 \(cargo\)/);
  assert.match(versionPinProblems(tree({ ...files("0.4.7"), "packages/release/rightkit-versions.json": "{}" }), "rightkit-fs").join("\n"), /neither stagedCargo nor cargo/);
  assert.match(versionPinProblems(tree({}), "rightkit-fs").join("\n"), /Cargo\.toml is missing|cannot be read/);
});

test("replay 422699d7: rightkit-fs 0.4.7 against a feed that still stages 0.4.6", (t) => {
  if (spawnSync("git", ["-C", LIVE, "cat-file", "-e", "422699d7^{commit}"]).status !== 0) return t.skip("commit not available on this machine");
  const files = {};
  for (const f of ["crates/rightkit-fs/Cargo.toml", "ci/consumers/rightkit-fs/Cargo.toml", "packages/release/rightkit-versions.json"]) files[f] = spawnSync("git", ["-C", LIVE, "show", `422699d7:${f}`], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }).stdout;
  const problems = versionPinProblems(tree(files), "rightkit-fs");
  assert.ok(problems.some((p) => /lists rightkit-fs 0\.4\.6 \(stagedCargo\) but .* is 0\.4\.7/.test(p)), problems.join("\n"));
  assert.ok(!problems.some((p) => /pins =/.test(p)), "the consumer was already at 0.4.7");
});
