import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { downloadUrl, executable, shellDownloadUrl, shellExecutable } from "./provision-chrome.mjs";
import { resolveR2CftEnv } from "./provision-r2-cft-env.mjs";

const KEY = "mac-arm64";
const WIN = "win64";
const digest = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const SHA = digest("verified Chrome archive bytes");
const SHELL_SHA = digest("verified headless shell archive bytes");
const CHROME_BYTES = "extracted Chrome executable bytes";
const SHELL_BYTES = "extracted headless shell executable bytes";
const PINS = {
  version: "1.2.3.4",
  platforms: { [KEY]: { sha256: SHA, executableSha256: digest(CHROME_BYTES) }, [WIN]: { sha256: SHA, executableSha256: digest(CHROME_BYTES) } },
  headlessShell: { [WIN]: { sha256: SHELL_SHA, executableSha256: digest(SHELL_BYTES) } },
};
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-r2-cft-test-"));

function install(root, { sha = SHA, shellSha = SHELL_SHA, key = KEY, withShell = false } = {}) {
  const dir = path.join(root, "current");
  const exe = executable(dir, key);
  fs.mkdirSync(path.dirname(exe), { recursive: true });
  fs.writeFileSync(exe, CHROME_BYTES);
  const files = [{ kind: "chrome", url: downloadUrl("1.2.3.4", key), sha256: sha }];
  if (withShell) {
    const shellExe = shellExecutable(dir, key);
    fs.mkdirSync(path.dirname(shellExe), { recursive: true });
    fs.writeFileSync(shellExe, SHELL_BYTES);
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

test("a verified archive marker exports extracted Chrome bytes hash without a shell var", () => {
  const { temp, root } = staged();
  try {
    install(root);
    assert.deepEqual(resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: KEY }), {
      RK_R2_CFT_ROOT: fs.realpathSync(root),
      RK_R2_CFT_SHA256: digest(CHROME_BYTES),
    });
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("a verified Windows install exports extracted Chrome & headless shell bytes hashes", () => {
  const { temp, root } = staged();
  try {
    install(root, { key: WIN, withShell: true });
    assert.deepEqual(resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: WIN }), {
      RK_R2_CFT_ROOT: fs.realpathSync(root),
      RK_R2_CFT_SHA256: digest(CHROME_BYTES),
      RK_R2_HEADLESS_SHELL_SHA256: digest(SHELL_BYTES),
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

test("a headless shell marker for another archive hash is not trusted", () => {
  const { temp, root } = staged();
  try {
    install(root, { key: WIN, withShell: true, shellSha: "c".repeat(64) });
    assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: WIN }), /does not match the pinned headless shell/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

test("current cannot link to an install outside managed root", () => {
  const { temp, root } = staged();
  const outside = path.join(temp, "outside");
  try {
    const dir = install(outside);
    fs.symlinkSync(dir, path.join(root, "current"), process.platform === "win32" ? "junction" : "dir");
    assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: KEY }), /marker must be inside managed root/);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
});

for (const kind of ["chrome", "headless_shell"]) {
  test(`${kind} executable cannot resolve outside managed root`, () => {
    const { temp, root } = staged();
    try {
      const current = install(root, { key: WIN, withShell: true });
      const exe = kind === "chrome" ? executable(current, WIN) : shellExecutable(current, WIN);
      const directory = path.dirname(exe);
      const outside = path.join(temp, "outside");
      fs.renameSync(directory, outside);
      fs.symlinkSync(outside, directory, process.platform === "win32" ? "junction" : "dir");
      assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: WIN }), /executable must be inside managed root/);
    } finally { fs.rmSync(temp, { recursive: true, force: true }); }
  });
}

for (const kind of ["chrome", "headless_shell"]) {
  test(`${kind} cached bytes changed behind a matching archive marker are refused`, () => {
    const { temp, root } = staged();
    try {
      const current = install(root, { key: WIN, withShell: true });
      const marker = fs.readFileSync(path.join(current, "install.json"), "utf8");
      const exe = kind === "chrome" ? executable(current, WIN) : shellExecutable(current, WIN);
      fs.writeFileSync(exe, "tampered executable bytes");
      assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins: PINS, key: WIN }), /executable sha256 does not match the pinned executable/);
      assert.equal(fs.readFileSync(path.join(current, "install.json"), "utf8"), marker);
    } finally { fs.rmSync(temp, { recursive: true, force: true }); }
  });
}

for (const section of ["platforms", "headlessShell"]) {
  for (const executableSha256 of [undefined, "", "not-a-sha256"]) {
    test(`${section} refuses missing or malformed executable pin: ${String(executableSha256)}`, () => {
      const { temp, root } = staged();
      try {
        install(root, { key: WIN, withShell: true });
        const pins = { ...PINS, [section]: { ...PINS[section], [WIN]: { ...PINS[section][WIN], executableSha256 } } };
        assert.throws(() => resolveR2CftEnv({ managedRoot: root, runnerTemp: temp, pins, key: WIN }), /has no pinned executable sha256/);
      } finally { fs.rmSync(temp, { recursive: true, force: true }); }
    });
  }
}
