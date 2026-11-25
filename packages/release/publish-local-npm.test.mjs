import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { publishLocalNpm, toolCommand } from "./publish-local-npm.mjs";

const SHA = "a".repeat(40);
const manifest = { name: "@rightkit/ci", version: "9.9.9" };

// A fake host: git answers for one commit on origin/main, the registry says 404 before the publish and serves the packed shasum after it.
function fakeHost({ packedManifest = manifest } = {}) {
  const calls = [];
  let published = false; let sha1 = null;
  const run = (command, args, options = {}) => {
    calls.push({ command, args, env: options.env });
    if (command === "git") {
      if (args[0] === "rev-parse") return { status: 0, stdout: `${SHA}\n` };
      if (args[0] === "show") return { status: 0, stdout: JSON.stringify(manifest) };
      return { status: 0, stdout: "" };
    }
    if (command === "curl") {
      if (!published) return { status: 0, stdout: "{}\n404" };
      return { status: 0, stdout: `${JSON.stringify({ version: manifest.version, dist: { shasum: sha1 } })}\n200` };
    }
    if (command === process.execPath) return { status: 0 };
    if (command === "tar") return { status: 0, stdout: JSON.stringify(packedManifest) };
    const inner = command.endsWith("cmd.exe") ? args.slice(3) : [command, ...args];
    if (inner[0] === "npm" && inner[1] === "publish") {
      published = true;
      sha1 = sha1Of(fs.readFileSync(inner[2]));
      return { status: 0, stdout: "" };
    }
    return { status: 0, stdout: "" };
  };
  return { run, calls };
}
const sha1Of = (bytes) => crypto.createHash("sha1").update(bytes).digest("hex");

function archive() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "pln-"));
  const file = path.join(dir, "rightkit-ci-9.9.9.tgz");
  fs.writeFileSync(file, "packed bytes");
  return file;
}

test("on Windows npm and pnpm start through cmd.exe; elsewhere and for other programs they start as they are", () => {
  assert.deepEqual(toolCommand("npm", ["publish", "x.tgz"], "win32", "C:\\Windows\\system32\\cmd.exe"), ["C:\\Windows\\system32\\cmd.exe", ["/d", "/s", "/c", "npm", "publish", "x.tgz"]]);
  assert.deepEqual(toolCommand("npm", ["publish"], "darwin"), ["npm", ["publish"]]);
  assert.deepEqual(toolCommand("git", ["status"], "win32"), ["git", ["status"]]);
});

test("Windows publishes with the account's own npm sign-in file and never reads a token", () => {
  const host = fakeHost();
  let tokenRead = false;
  const out = publishLocalNpm(["npm", "--package", "@rightkit/ci", "--commit", SHA, "--archive", archive()], {
    run: host.run, log: () => {}, platform: "win32", npmrc: () => "C:\\Users\\someone\\.npmrc", exists: () => true,
    readToken: () => { tokenRead = true; return "secret"; }, verifyTries: 1, verifyDelaySeconds: 0,
  });
  assert.equal(tokenRead, false);
  const pub = host.calls.find((c) => c.args.includes("publish"));
  assert.ok(pub.command.endsWith("cmd.exe"));
  assert.equal(pub.args[pub.args.indexOf("--userconfig") + 1], "C:\\Users\\someone\\.npmrc");
  assert.equal(pub.env, undefined);
  assert.equal(out.version, "9.9.9");
});

test("Windows without an npm sign-in, and any host other than the Mac or Windows, publish nothing", () => {
  assert.throws(() => publishLocalNpm(["npm", "--package", "@rightkit/ci", "--commit", SHA], { run: fakeHost().run, platform: "win32", npmrc: () => "C:\\none\\.npmrc", exists: () => false }), /does not exist: run npm login/);
  assert.throws(() => publishLocalNpm(["npm", "--package", "@rightkit/ci", "--commit", SHA], { run: fakeHost().run, platform: "linux" }), /runs on the Mac .* or Windows/);
});

test("--archive must be the commit's package at its version", () => {
  const host = fakeHost({ packedManifest: { name: "@rightkit/ci", version: "9.9.8" } });
  assert.throws(() => publishLocalNpm(["npm", "--package", "@rightkit/ci", "--commit", SHA, "--archive", archive()], { run: host.run, log: () => {}, platform: "win32", npmrc: () => "x", exists: () => true }), /is @rightkit\/ci@9\.9\.8, not @rightkit\/ci@9\.9\.9/);
  assert.ok(!host.calls.some((c) => c.args.includes("publish")));
});

test("the gate package is refused before any credential is used when no acceptance receipt admits the archive", () => {
  const gate = { name: "@rightkit/release", version: "9.9.9" };
  const host = fakeHost({ packedManifest: gate });
  const run = (command, args, options) => (command === "git" && args[0] === "show" ? { status: 0, stdout: JSON.stringify(gate) } : host.run(command, args, options));
  const saved = process.env.RK_GATE_CONTRACT_RECEIPT; delete process.env.RK_GATE_CONTRACT_RECEIPT;
  try {
    assert.throws(() => publishLocalNpm(["npm", "--package", "@rightkit/release", "--commit", SHA, "--archive", archive()], { run, log: () => {}, platform: "win32", npmrc: () => "x", exists: () => true }), /NN31/);
  } finally { if (saved !== undefined) process.env.RK_GATE_CONTRACT_RECEIPT = saved; }
  assert.ok(!host.calls.some((c) => c.args.includes("publish")));
});
