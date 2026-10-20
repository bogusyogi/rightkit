import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { parseYaml } from "./workflow-yaml.mjs";
import { downloadUrl, shellDownloadUrl, executable, grantSandboxRead, needsChrome, provision, provisionForTests, provisionHeadlessShell, shellExecutable, shellPinFor } from "./provision-chrome.mjs";

const KEY = "mac-arm64";
const PINS = (sha256) => ({ version: "1.2.3.4", platforms: { [KEY]: { sha256 } } });
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-prov-test-"));
const noFetch = async () => { throw new Error("fetch must not be called"); };

function install(root, sha256) {
  const dir = path.join(root, "current");
  const exe = executable(dir, KEY);
  fs.mkdirSync(path.dirname(exe), { recursive: true });
  fs.writeFileSync(exe, "");
  fs.writeFileSync(path.join(dir, "install.json"), JSON.stringify({ version: "1.2.3.4", platform: KEY, files: [{ kind: "chrome", url: downloadUrl("1.2.3.4", KEY), sha256 }] }));
  return exe;
}

test("needsChrome selects only units that drive Chrome", () => {
  assert.equal(needsChrome({ npm: { test: ["packages/qa"] }, rust: { test: [] } }), true);
  assert.equal(needsChrome({ npm: { test: ["packages/qa-chrome"] }, rust: { test: [] } }), true);
  assert.equal(needsChrome({ npm: { test: ["packages/settings-ui"] }, rust: { test: [] } }), true);
  assert.equal(needsChrome({ npm: { test: ["packages/onboarding"] }, rust: { test: [] } }), true);
  assert.equal(needsChrome({ npm: { test: [] }, rust: { test: ["rightkit-browser"] } }), true);
  assert.equal(needsChrome({ npm: { test: ["packages/ax", "packages/legal-ui"] }, rust: { test: ["rightkit-file-watch"] } }), false);
  assert.equal(needsChrome({ npm: { test: [] }, rust: { test: [] } }), false);
});

test("an install already marked with the pinned hash is reused without a download", async () => {
  const root = tmp();
  try {
    const sha = "a".repeat(64);
    const exe = install(root, sha);
    const result = await provision({ root, pins: PINS(sha), key: KEY, fetchImpl: noFetch, log: () => {} });
    assert.deepEqual(result, { exe, downloaded: false });
    const env = (await provisionForTests({ root, pins: PINS(sha), key: KEY, fetchImpl: noFetch, log: () => {} })).env;
    assert.equal(env.RIGHTKIT_CHROME_FOR_TESTING_ROOT, root);
    assert.equal(env.RIGHTKIT_CHROME_FOR_TESTING, exe);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("an install with a different recorded hash is not trusted: the pin is downloaded and a digest mismatch is refused", async () => {
  const root = tmp();
  try {
    install(root, "b".repeat(64));
    const bytes = Buffer.from("not chrome");
    let calls = 0;
    const fetchImpl = async () => { calls++; return { ok: true, status: 200, arrayBuffer: async () => bytes }; };
    await assert.rejects(provision({ root, pins: PINS(crypto.createHash("sha256").update("other").digest("hex")), key: KEY, fetchImpl, log: () => {} }), /is not the pinned/);
    assert.equal(calls, 1);
    assert.deepEqual(fs.readdirSync(root).filter((n) => n.startsWith(".staging-")), []);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("an unpinned platform is never provisioned by the gate", async () => {
  await assert.rejects(provision({ root: tmp(), pins: PINS(""), key: KEY, fetchImpl: noFetch, log: () => {} }), /no pinned sha256/);
});

test("on Windows the install is granted to Chrome's sandbox, and a failed grant fails the provision", () => {
  const calls = [];
  const ok = (file, args) => { calls.push([file, args]); return { status: 0 }; };
  assert.equal(grantSandboxRead("C:\\tools\\chrome\\1.2.3.4", { platform: "win32", run: ok }), true);
  assert.match(calls[0][0], /icacls\.exe$/);
  assert.deepEqual(calls[0][1], ["C:\\tools\\chrome\\1.2.3.4", "/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "*S-1-15-2-2:(OI)(CI)(RX)", "/T", "/C", "/Q"]);
  assert.throws(() => grantSandboxRead("C:\\x", { platform: "win32", run: () => ({ status: 5 }) }), /icacls exit 5/);
  // Other systems have no AppContainer: nothing runs.
  assert.equal(grantSandboxRead("/x", { platform: "darwin", run: () => { throw new Error("must not run"); } }), false);
});

test("the headless shell pin is optional per platform and reused when its marker matches", async () => {
  assert.equal(shellPinFor(PINS("a".repeat(64)), KEY), null);
  assert.equal(await provisionHeadlessShell({ root: tmp(), pins: PINS("a".repeat(64)), key: KEY, fetchImpl: noFetch, log: () => {} }), null);
  const root = tmp();
  try {
    const sha = "c".repeat(64);
    const pins = { version: "1.2.3.4", platforms: { win64: { sha256: "a".repeat(64) } }, headlessShell: { "win64": { sha256: sha } } };
    assert.match(shellPinFor(pins, "win64").url, /\/1\.2\.3\.4\/win64\/chrome-headless-shell-win64\.zip$/);
    const dir = path.join(root, "current");
    const exe = shellExecutable(dir, "win64");
    for (const file of [exe, executable(dir, "win64")]) {
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, "");
    }
    fs.writeFileSync(path.join(dir, "install.json"), JSON.stringify({ version: pins.version, platform: "win64", files: [
      { kind: "chrome", url: downloadUrl(pins.version, "win64"), sha256: "a".repeat(64) },
      { kind: "headless_shell", url: shellDownloadUrl(pins.version, "win64"), sha256: sha },
    ] }));
    assert.deepEqual(await provisionHeadlessShell({ root, pins, key: "win64", fetchImpl: noFetch, log: () => {} }), { exe, downloaded: false });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

for (const key of ["mac-arm64", "win64", "linux64"]) {
  test(`${key} provisions the frozen install marker and all executables before cache reuse`, async () => {
    const root = tmp();
    try {
      const chrome = Buffer.from(`chrome ${key}`), shell = Buffer.from(`shell ${key}`);
      const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
      const pins = { version: "1.2.3.4", platforms: { [key]: { sha256: hash(chrome) } },
        ...(key === "win64" ? { headlessShell: { [key]: { sha256: hash(shell) } } } : {}) };
      const urls = [];
      const fetchImpl = async (url) => { urls.push(url); return { ok: true, arrayBuffer: async () => url.includes("headless-shell") ? shell : chrome }; };
      const extract = (zip, out) => {
        const exe = zip.endsWith("headless_shell.zip") ? shellExecutable(out, key) : executable(out, key);
        fs.mkdirSync(path.dirname(exe), { recursive: true }); fs.writeFileSync(exe, "verified executable");
        return { status: 0 };
      };
      // A different recorded pin cannot suppress verified installation.
      install(root, "b".repeat(64));
      const result = await provision({ root, pins, key, fetchImpl, extract, log: () => {} });
      assert.equal(result.exe, executable(path.join(root, "current"), key));
      assert.equal(result.downloaded, true);
      const files = [{ kind: "chrome", url: downloadUrl(pins.version, key), sha256: hash(chrome) },
        ...(key === "win64" ? [{ kind: "headless_shell", url: shellDownloadUrl(pins.version, key), sha256: hash(shell) }] : [])];
      assert.deepEqual(JSON.parse(fs.readFileSync(path.join(root, "current", "install.json"), "utf8")), { version: pins.version, platform: key, files });
      assert.deepEqual(urls, files.map((file) => file.url));
      assert.equal(fs.existsSync(path.join(root, "current.previous", "install.json")), true);
      assert.equal((await provision({ root, pins, key, fetchImpl: noFetch, log: () => {} })).downloaded, false);
      if (key === "win64") {
        fs.rmSync(shellExecutable(path.join(root, "current"), key));
        assert.equal((await provision({ root, pins, key, fetchImpl, extract, log: () => {} })).downloaded, true);
      }
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });
}

test("a rejected companion archive leaves the prior current install and marker untouched", async () => {
  const root = tmp();
  try {
    const exe = install(root, "a".repeat(64));
    const marker = fs.readFileSync(path.join(root, "current", "install.json"), "utf8");
    const bytes = Buffer.from("verified chrome");
    const pins = { version: "1.2.3.4", platforms: { win64: { sha256: crypto.createHash("sha256").update(bytes).digest("hex") } }, headlessShell: { win64: { sha256: "b".repeat(64) } } };
    let extracted = 0;
    await assert.rejects(provision({ root, pins, key: "win64", fetchImpl: async () => ({ ok: true, arrayBuffer: async () => bytes }), extract: () => { extracted++; return { status: 0 }; }, log: () => {} }), /headless_shell download.*is not the pinned/);
    assert.equal(extracted, 1);
    assert.equal(fs.existsSync(exe), true);
    assert.equal(fs.readFileSync(path.join(root, "current", "install.json"), "utf8"), marker);
    assert.deepEqual(fs.readdirSync(root).filter((n) => n.startsWith(".staging-")), []);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("both sealed-artifact verification jobs inherit the provisioned managed root", () => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8"));
  for (const name of ["verify", "verify-linux"]) {
    const steps = doc.jobs[name].steps;
    const provision = steps.find((step) => step.run === "node scripts/ci/provision-chrome.mjs");
    const verify = steps.find((step) => step.run === "node scripts/ci/publish.mjs verify");
    assert.equal(provision.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT, "${{ runner.temp }}/chrome-for-testing");
    assert.equal(verify.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT, provision.env.RIGHTKIT_CHROME_FOR_TESTING_ROOT);
  }
});

test("CI's Linux digest reuses the frozen installer pin without changing version", () => {
  const pins = JSON.parse(fs.readFileSync(new URL("../../ci/chrome-for-testing.json", import.meta.url), "utf8"));
  const installer = fs.readFileSync(new URL("../../packages/qa-chrome/src/install.ts", import.meta.url), "utf8");
  const installerVersion = installer.match(/export const PINNED_CHROME_FOR_TESTING: PinnedChrome = \{\s*version: '([^']+)'/)[1];
  const installerDigest = installer.match(/'linux64': '([0-9a-f]{64})'/)[1];
  assert.equal(pins.version, installerVersion);
  assert.equal(pins.platforms.linux64.sha256, installerDigest);
});
