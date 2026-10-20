import assert from "node:assert/strict";
import test from "node:test";
import { downloadUrl } from "./provision-chrome.mjs";
import { PROFILE_PATH, profileCommands, profileText, validateContext, validateInstall } from "./chrome-linux-sandbox.mjs";

const sha256 = "a".repeat(64);
const pins = () => ({ version: "1.2.3.4", platforms: { linux64: { sha256 } } });
const marker = () => ({ version: "1.2.3.4", platform: "linux64", files: [{ kind: "chrome", url: downloadUrl("1.2.3.4", "linux64"), sha256 }] });
const fixture = () => ({
  platform: "linux", arch: "x64",
  event: { repository: { full_name: "bogusyogi/rightkit", private: false, default_branch: "main" } },
  env: {
    GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted", RUNNER_OS: "Linux", RUNNER_ARCH: "X64",
    GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_EVENT_NAME: "workflow_dispatch",
    GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/publish.yml@refs/heads/main", GITHUB_SHA: "a".repeat(40),
    RUNNER_TEMP: "/home/runner/work/_temp",
    RIGHTKIT_CHROME_FOR_TESTING_ROOT: "/home/runner/work/_temp/chrome-for-testing",
    RIGHTKIT_CHROME_FOR_TESTING: "/home/runner/work/_temp/chrome-for-testing/current/chrome-linux64/chrome",
  },
});

test("only hosted Linux main public qualification & diagnostic identities qualify", () => {
  assert.equal(validateContext(fixture()).executable, fixture().env.RIGHTKIT_CHROME_FOR_TESTING);
  const diagnostic = fixture();
  diagnostic.env.GITHUB_WORKFLOW_REF = "bogusyogi/rightkit/.github/workflows/chrome-native-diagnostic.yml@refs/heads/main";
  diagnostic.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT = "/home/runner/work/_temp/rightkit-chrome-managed";
  diagnostic.env.RIGHTKIT_CHROME_FOR_TESTING = `${diagnostic.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT}/current/chrome-linux64/chrome`;
  assert.equal(validateContext(diagnostic).root, diagnostic.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT);
  for (const [key, value] of Object.entries({
    GITHUB_ACTIONS: "false", RUNNER_ENVIRONMENT: "self-hosted", RUNNER_OS: "Windows", RUNNER_ARCH: "ARM64",
    GITHUB_REPOSITORY: "someone/rightkit", GITHUB_REF: "refs/pull/1/merge", GITHUB_EVENT_NAME: "pull_request_target",
    GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/other.yml@refs/heads/main", GITHUB_SHA: "unbound",
    RUNNER_TEMP: "/home/runner/../temp", RIGHTKIT_CHROME_FOR_TESTING_ROOT: "/tmp/chrome", RIGHTKIT_CHROME_FOR_TESTING: "/usr/bin/google-chrome",
  })) {
    const input = fixture(); input.env[key] = value;
    assert.throws(() => validateContext(input), /refused/, key);
  }
  for (const mutate of [
    (f) => { f.platform = "darwin"; }, (f) => { f.arch = "arm64"; },
    (f) => { f.event.repository.private = true; }, (f) => { delete f.event.repository.private; },
    (f) => { f.event.repository.full_name = "someone/rightkit"; }, (f) => { f.event.repository.default_branch = "topic"; },
  ]) { const input = fixture(); mutate(input); assert.throws(() => validateContext(input), /refused/); }
});

test("each approved workflow accepts only its own provisioner's exact root", () => {
  const publish = fixture();
  assert.equal(validateContext(publish).root, "/home/runner/work/_temp/chrome-for-testing");
  publish.env.GITHUB_WORKFLOW_REF = "bogusyogi/rightkit/.github/workflows/chrome-native-diagnostic.yml@refs/heads/main";
  assert.throws(() => validateContext(publish), /exact managed Chrome path/);
  publish.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT = "/home/runner/work/_temp/rightkit-chrome-managed";
  publish.env.RIGHTKIT_CHROME_FOR_TESTING = `${publish.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT}/current/chrome-linux64/chrome`;
  assert.equal(validateContext(publish).root, publish.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT);
  publish.env.GITHUB_WORKFLOW_REF = "bogusyogi/rightkit/.github/workflows/publish.yml@refs/heads/main";
  assert.throws(() => validateContext(publish), /exact managed Chrome path/);
});

test("provisioner marker must exactly match pinned Linux version, URL & nonempty hash", () => {
  validateInstall(pins(), marker());
  for (const mutate of [
    (m) => { m.version = "2.3.4.5"; }, (m) => { m.platform = "win64"; },
    (m) => { m.files[0].sha256 = "b".repeat(64); }, (m) => { m.files[0].url = "https://example.org/chrome.zip"; },
    (m) => { m.files.push({ ...m.files[0] }); }, (m) => { m.files[0].kind = "headless_shell"; },
  ]) { const recorded = marker(); mutate(recorded); assert.throws(() => validateInstall(pins(), recorded), /does not match/); }
  const unpinned = pins(); unpinned.platforms.linux64.sha256 = "";
  assert.throws(() => validateInstall(unpinned, marker()), /nonempty pinned/);
});

test("profile permits userns for one exact quoted path & escapes pattern syntax", () => {
  assert.equal(profileText("/tmp/Chrome/chrome"), '# Managed by RightKit CI; grants userns only to pinned Chrome\'s exact path.\nprofile rightkit-ci-chrome "/tmp/Chrome/chrome" flags=(unconfined) {\n  userns,\n}\n');
  const metacharacters = '/tmp/a b*?[x]{y}\\"/chrome';
  assert.ok(profileText(metacharacters).includes('"/tmp/a b\\*\\?\\[x\\]\\{y\\}\\\\\\"/chrome"'));
  assert.throws(() => profileText('/tmp/chrome\nuserns,'), /control characters/);
  assert.throws(() => profileText('relative/chrome'), /absolute profile/);
});

test("privileged argv install root-owned profile & load it without shell or global settings", () => {
  assert.deepEqual(profileCommands("/tmp/staging/profile"), [
    ["/usr/bin/sudo", ["-n", "/usr/bin/install", "-T", "-o", "root", "-g", "root", "-m", "0644", "--", "/tmp/staging/profile", PROFILE_PATH]],
    ["/usr/bin/sudo", ["-n", "/usr/sbin/apparmor_parser", "-r", PROFILE_PATH]],
  ]);
});
