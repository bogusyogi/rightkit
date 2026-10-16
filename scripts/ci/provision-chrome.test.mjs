import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { executable, grantSandboxRead, needsChrome, provision, provisionForTests, provisionHeadlessShell, shellExecutable, shellPinFor } from "./provision-chrome.mjs";

const KEY = "mac-arm64";
const PINS = (sha256) => ({ version: "1.2.3.4", platforms: { [KEY]: { sha256 } } });
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-prov-test-"));
const noFetch = async () => { throw new Error("fetch must not be called"); };

function install(root, sha256) {
  const dir = path.join(root, "1.2.3.4");
  const exe = executable(dir, KEY);
  fs.mkdirSync(path.dirname(exe), { recursive: true });
  fs.writeFileSync(exe, "");
  fs.writeFileSync(path.join(dir, "install.json"), JSON.stringify({ sha256 }));
  return exe;
}

test("needsChrome selects only units that drive Chrome", () => {
  assert.equal(needsChrome({ npm: { test: ["packages/qa"] }, rust: { test: [] } }), true);
  assert.equal(needsChrome({ npm: { test: [] }, rust: { test: ["rightkit-browser"] } }), true);
  assert.equal(needsChrome({ npm: { test: ["packages/ax"] }, rust: { test: ["rightkit-file-watch"] } }), false);
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
    const pins = { version: "1.2.3.4", platforms: {}, headlessShell: { "win64": { sha256: sha } } };
    assert.match(shellPinFor(pins, "win64").url, /\/1\.2\.3\.4\/win64\/chrome-headless-shell-win64\.zip$/);
    const dir = path.join(root, "1.2.3.4");
    const exe = shellExecutable(dir, "win64");
    fs.mkdirSync(path.dirname(exe), { recursive: true });
    fs.writeFileSync(exe, "");
    fs.writeFileSync(path.join(dir, "install-headless-shell.json"), JSON.stringify({ sha256: sha }));
    assert.deepEqual(await provisionHeadlessShell({ root, pins, key: "win64", fetchImpl: noFetch, log: () => {} }), { exe, downloaded: false });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
