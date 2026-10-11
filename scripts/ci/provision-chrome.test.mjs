import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { executable, needsChrome, provision, provisionForTests } from "./provision-chrome.mjs";

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
  assert.equal(needsChrome({ npm: { test: ["packages/ax"] }, rust: { test: ["rightkit-watch"] } }), false);
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
