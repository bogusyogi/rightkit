import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { downloadUrl, executable, shellDownloadUrl, shellExecutable } from "./provision-chrome.mjs";
import { resolveR2CftEnv } from "./provision-r2-cft-env.mjs";

const KEY = "mac-arm64";
const WIN = "win64";
const SHA = "a".repeat(64);
const SHELL_SHA = "b".repeat(64);
const PINS = {
  version: "1.2.3.4",
  platforms: { [KEY]: { sha256: SHA }, [WIN]: { sha256: SHA } },
  headlessShell: { [WIN]: { sha256: SHELL_SHA } },
};
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-r2-cft-test-"));

function install(root, { sha = SHA, shellSha = SHELL_SHA, key = KEY, withShell = false } = {}) {
  const dir = path.join(root, "current");
  const exe = executable(dir, key);
  fs.mkdirSync(path.dirname(exe), { recursive: true });
  fs.writeFileSync(exe, "");
  const files = [{ kind: "chrome", url: downloadUrl("1.2.3.4", key), sha256: sha }];
  if (withShell) {
    const shellExe = shellExecutable(dir, key);
    fs.mkdirSync(path.dirname(shellExe), { recursive: true });
    fs.writeFileSync(shellExe, "");
    files.push({ kind: "headless_shell", url: shellDownloadUrl("1.2.3.4", key), sha256: shellSha });
  }
  fs.writeFileSync(path.join(dir, "install.json"), JSON.stringify({ version: "1.2.3.4", platform: key, files }));
  return dir;
}

function staged() {
  const temp = tmp();
  const root = path.join(temp, "chrome-for-testing");
  fs.mkdirSync(root);
  return { temp, root };
}

test("a matching provisioned install resolves the R2 CFT route without a shell var", () => {
  const { temp, root } = staged();
  try {
    install(root);
    assert.deepEqual(resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: KEY }), {
      RK_R2_CFT_ROOT: fs.realpathSync(root),
      RK_R2_CFT_SHA256: SHA,
    });
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a matching Windows install also resolves the pinned headless shell hash", () => {
  const { temp, root } = staged();
  try {
    install(root, { key: WIN, withShell: true });
    assert.deepEqual(resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: WIN }), {
      RK_R2_CFT_ROOT: fs.realpathSync(root),
      RK_R2_CFT_SHA256: SHA,
      RK_R2_HEADLESS_SHELL_SHA256: SHELL_SHA,
    });
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a marker for another hash is not trusted", () => {
  const { temp, root } = staged();
  try {
    install(root, { sha: "c".repeat(64) });
    assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: KEY }), /does not match the pinned/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a missing marker refuses before any consumer can read an empty root", () => {
  const { temp, root } = staged();
  try {
    assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: KEY }), /marker is missing/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a root outside RUNNER_TEMP is refused", () => {
  const { temp, root } = staged();
  const outside = tmp();
  try {
    install(root);
    assert.throws(() => resolveR2CftEnv({ managedRoot: outside, runnerTemp: temp, pins: PINS, key: KEY }), /inside RUNNER_TEMP/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); fs.rmSync(outside, { recursive: true, force: true }); }
});

test("an unpinned platform refuses instead of exporting an empty hash", () => {
  const { temp, root } = staged();
  try {
    install(root);
    assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: { version: "1.2.3.4", platforms: {} }, key: KEY }), /no entry for/);
    assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: { version: "1.2.3.4", platforms: { [KEY]: { sha256: "" } } }, key: KEY }), /no pinned sha256/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});
