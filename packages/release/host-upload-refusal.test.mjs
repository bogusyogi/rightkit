import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { HOST_UPLOAD_REFUSED } from "./host-upload-refusal.mjs";
import { runLocalPublish } from "./local-publish.mjs";
import { runNpmPreflight } from "./publication-gate.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const node = (file, args, env = {}) => spawnSync(process.execPath, [path.join(here, file), ...args], {
  encoding: "utf8", windowsHide: true, env: { ...process.env, ...env },
});

// A caller-set GITHUB_ACTIONS=true (or CI=true) must never lift the refusal: CI uploads use scripts/ci/publish.mjs and
// reviewed-candidate.mjs, not right-release.
for (const env of [{}, { GITHUB_ACTIONS: "true" }, { GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" }]) {
  const label = JSON.stringify(env);
  for (const args of [
    ["publish", "cargo", "--crate", "rightkit-x"],
    ["publish", "cargo", "--crate", "rightkit-x", "--from-evidence", "gate.json"],
    ["publish", "cargo", "--crate", "rightkit-x", "--local"],
    ["publish", "npm", "--package", "@rightkit/x", "--publish", "--tarball", "x.tgz"],
    ["publish", "npm", "--package", "@rightkit/x", "--local"],
  ]) {
    test(`right-release ${args.join(" ")} is refused with env ${label}`, () => {
      const result = node("cli/right-release.mjs", args, env);
      // Without GITHUB_ACTIONS a compile=github-actions-only checkout stops it even earlier (exit 125); either way nothing uploads.
      if (env.GITHUB_ACTIONS === "true") { assert.equal(result.status, 2, result.stderr); assert.match(result.stderr, /host uploads are retired/); }
      else { assert.notEqual(result.status, 0, result.stderr); assert.match(result.stderr, /host uploads are retired|GitHub Actions only/); }
    });
  }
}

test("direct publish-cargo.mjs refuses every non-dry-run call, including --from-evidence, with GITHUB_ACTIONS=true", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-host-upload-"));
  try {
    fs.mkdirSync(path.join(root, "crates", "rightkit-x"), { recursive: true });
    for (const args of [["--crate", "rightkit-x", "--workspace", root], ["--crate", "rightkit-x", "--workspace", root, "--from-evidence", path.join(root, "gate.json")]]) {
      const result = node("publish-cargo.mjs", args, { GITHUB_ACTIONS: "true" });
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, new RegExp(HOST_UPLOAD_REFUSED + "|host uploads are retired"));
    }
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("direct publication-gate.mjs --publish is refused with GITHUB_ACTIONS=true", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-host-upload-"));
  try {
    fs.mkdirSync(path.join(root, "packages", "x"), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", "x", "package.json"), JSON.stringify({ name: "@rightkit/x", version: "1.0.0" }));
    const result = node("publication-gate.mjs", ["--package", "@rightkit/x", "--workspace", root, "--publish", "--tarball", path.join(root, "x.tgz")], { GITHUB_ACTIONS: "true" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /host uploads are retired/);
    assert.doesNotMatch(result.stderr, /submitted/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("runLocalPublish refuses every upload mode in-process and keeps --qualify-only", async () => {
  await assert.rejects(runLocalPublish("cargo", ["--local", "--crate", "rightkit-x", "--workspace", os.tmpdir()]), { code: HOST_UPLOAD_REFUSED });
  await assert.rejects(runLocalPublish("cargo", ["--local", "--crates", "rightkit-x,rightkit-y", "--workspace", os.tmpdir()]), { code: HOST_UPLOAD_REFUSED });
  await assert.rejects(runLocalPublish("npm", ["--local", "--package", "@rightkit/x", "--workspace", os.tmpdir()]), { code: HOST_UPLOAD_REFUSED });
  // --qualify-only is not refused as an upload; it proceeds to its own preconditions (a missing unit here).
  await assert.rejects(
    runLocalPublish("cargo", ["--local", "--crate", "rightkit-x", "--workspace", path.join(os.tmpdir(), "rk-no-such-ws"), "--qualify-only", "--bundle", path.join(os.tmpdir(), "rk-no-such-bundle")]),
    (error) => error.code !== HOST_UPLOAD_REFUSED,
  );
});

test("runNpmPreflight --publish is refused before any tarball or token is read", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-host-upload-"));
  try {
    fs.mkdirSync(path.join(root, "packages", "x"), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", "x", "package.json"), JSON.stringify({ name: "@rightkit/x", version: "1.0.0" }));
    assert.throws(() => runNpmPreflight(["--package", "@rightkit/x", "--publish", "--tarball", "x.tgz"], root, { readToken: () => assert.fail("token must not be read") }), { code: HOST_UPLOAD_REFUSED });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
