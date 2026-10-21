import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  HELPER_ACCESS_GROUP,
  HELPER_APP,
  HELPER_BUNDLE_ID,
  HELPER_EXECUTABLE,
  HELPER_NAME,
  HELPER_PLIST,
  HELPER_PROFILE,
  HELPER_TEAM_ID,
  SIGNED_FILES,
  resolveCredentialsHelperEnv,
} from "./provision-credentials-helper-env.mjs";

const GITHUB = {
  repository: "Orthic-Labs/rightkit",
  sha: "0".repeat(40),
  runId: "123",
  runAttempt: "1",
  workflowRef: "Orthic-Labs/rightkit/.github/workflows/publish.yml@refs/heads/main",
};
const BINARY = Buffer.from("signed-helper-bytes");
const DIGEST = crypto.createHash("sha256").update(BINARY).digest("hex");
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-cred-helper-test-"));

function layout(root, { digest = DIGEST, job = "sign-credentials-helper", runId = GITHUB.runId, extra = false, symlink = false, dropProfile = false } = {}) {
  const app = path.join(root, HELPER_APP);
  fs.mkdirSync(path.join(app, "Contents", "MacOS"), { recursive: true });
  fs.writeFileSync(path.join(app, HELPER_EXECUTABLE), BINARY);
  fs.writeFileSync(path.join(app, HELPER_PLIST),
    `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>` +
    `<key>CFBundleIdentifier</key><string>${HELPER_BUNDLE_ID}</string>` +
    `<key>CFBundleExecutable</key><string>${HELPER_NAME}</string></dict></plist>`);
  if (!dropProfile) fs.writeFileSync(path.join(app, HELPER_PROFILE), "profile-bytes");
  fs.writeFileSync(path.join(root, "helper.sha256"), `${digest}  ${HELPER_NAME}\n`);
  const receipt = [
    `repository=${GITHUB.repository}`,
    `source=${GITHUB.sha}`,
    `run=${runId}`,
    `attempt=${GITHUB.runAttempt}`,
    `workflow=${GITHUB.workflowRef}`,
    "host=macOS",
    "arch=ARM64",
    `job=${job}`,
    `${digest}  ${HELPER_NAME}`,
  ].join("\n");
  fs.writeFileSync(path.join(root, "source-receipt.txt"), `${receipt}\n`);
  fs.writeFileSync(path.join(root, "unsigned-source-receipt.txt"), "unsigned\n");
  fs.writeFileSync(path.join(root, "entitlements.plist"), "entitlements\n");
  fs.writeFileSync(path.join(root, "source-inputs.json"), "{}\n");
  if (extra) fs.writeFileSync(path.join(root, "stowaway"), "x");
  if (symlink) fs.symlinkSync(path.join(root, "helper.sha256"), path.join(root, "link"));
  return root;
}

function staged(options) {
  const temp = tmp();
  const root = path.join(temp, "credentials-helper-signed");
  fs.mkdirSync(root);
  layout(root, options);
  return { temp, root };
}

test("a sealed same-run handover resolves the helper route", () => {
  const { temp, root } = staged();
  try {
    assert.deepEqual(resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), {
      RK_SOURCE_HELPER_APP: path.join(fs.realpathSync(root), HELPER_APP),
      RK_CONSUMER_HELPER_NAME: HELPER_NAME,
      RK_CONSUMER_HELPER_TEAM_ID: HELPER_TEAM_ID,
    });
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("pinned helper identity constants match the qualification fixture", () => {
  assert.equal(HELPER_NAME, "RightKitCredentialsQualification");
  assert.equal(HELPER_TEAM_ID, "6KLGD3LLKF");
  assert.equal(HELPER_BUNDLE_ID, "org.rightkit.qualification.credentials");
  assert.equal(HELPER_ACCESS_GROUP, "6KLGD3LLKF.org.rightkit.qualification.credentials");
  assert.deepEqual(SIGNED_FILES, [HELPER_APP, "entitlements.plist", "helper.sha256", "source-inputs.json", "source-receipt.txt", "unsigned-source-receipt.txt"]);
});

test("a rewritten executable is not trusted", () => {
  const { temp, root } = staged({ digest: "f".repeat(64) });
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), /does not match the sealed hash/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a receipt from another job is not trusted", () => {
  const { temp, root } = staged({ job: "build-credentials-helper" });
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), /names another job/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a receipt from another run is not trusted", () => {
  const { temp, root } = staged({ runId: "999" });
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), /does not match this run/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a stowaway file in the handover is not trusted", () => {
  const { temp, root } = staged({ extra: true });
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), /exactly the sealed helper files/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a symlink in the handover is not trusted", () => {
  const { temp, root } = staged({ symlink: true });
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), /must not contain symlinks|exactly the sealed helper files/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a handover outside RUNNER_TEMP is not trusted", () => {
  const inner = tmp();
  const outer = tmp();
  const root = path.join(outer, "credentials-helper-signed");
  fs.mkdirSync(root);
  layout(root);
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: inner, github: GITHUB }), /must be inside RUNNER_TEMP/);
  } finally { fs.rmSync(inner, { recursive: true, force: true }); fs.rmSync(outer, { recursive: true, force: true }); }
});

test("a handover without an embedded profile is not trusted", () => {
  const { temp, root } = staged({ dropProfile: true });
  try {
    assert.throws(() => resolveCredentialsHelperEnv({ signedRoot: root, runnerTemp: temp, github: GITHUB }), /provisioning profile/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});
