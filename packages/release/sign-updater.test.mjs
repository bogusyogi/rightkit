import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import test from "node:test";
import path from "node:path";
import { runSignUpdater } from "./sign-updater.mjs";

const helper = fileURLToPath(new URL("./sign-updater.mjs", import.meta.url));

test("dry-run signs the final OS-signed artifact with Tauri minisign", () => {
  const result = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--dry-run"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tauri signer sign .*Fixture\.exe/);
});

test("no key-file interface: --key-file is refused and the key comes from the credential provider", { skip: process.platform !== "win32" }, () => {
  const env = { ...process.env, TAURI_SIGNING_PRIVATE_KEY: "", RIGHT_UPDATER_KEY_FILE: "" };
  const none = spawnSync(process.execPath, [helper, "--file", "Fixture.exe"], { encoding: "utf8", env });
  assert.notEqual(none.status, 0);
  assert.match(none.stderr, /updater artifact not found/);
  assert.doesNotMatch(none.stderr, /key not found/);
  const refused = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--key-file", "Z:\\no\\updater.key"], { encoding: "utf8", env });
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /--key-file is removed/);
});

test("sign-updater source never reads a key file into the environment", async () => {
  const { readFileSync } = await import("node:fs");
  const source = readFileSync(helper, "utf8");
  assert.doesNotMatch(source, /RIGHT_UPDATER_KEY_FILE|readFileSync\(key/);
  assert.match(source, /execWithUpdaterCredential/);
  assert.doesNotMatch(source, /cmd\.exe|ComSpec|shell: true/);
});

test("provider signs with absolute Node & literal pnpm argv, including shell metacharacters", () => {
  const calls = [];
  const cwd = process.cwd();
  const file = "Fixture & echo injected.exe";
  runSignUpdater(["--file", file], { platform: "win32", env: {}, cwd,
    exists: () => true, remove: () => {}, log: () => {},
    resolvePnpm: () => path.join(cwd, "pnpm.cjs"),
    execCredential: (...args) => { calls.push(args); return { status: 0 }; },
  });
  assert.equal(calls[0][0], process.execPath);
  assert.deepEqual(calls[0][1], [path.join(cwd, "pnpm.cjs"), "exec", "tauri", "signer", "sign", path.resolve(cwd, file)]);
  assert.equal(calls[0][2].env.TAURI_SIGNING_PRIVATE_KEY, undefined);
});

test("provider failure fails closed & preserves its child exit", () => {
  assert.throws(() => runSignUpdater(["--file", "fixture.exe"], {
    env: {}, platform: "win32", exists: () => true, remove: () => {}, log: () => {},
    resolvePnpm: () => "fixture-pnpm.cjs", execCredential: () => ({ status: 23 }),
  }), (error) => error.exitCode === 23 && /credential provider/.test(error.message));
});

test("inherited signing secrets never bypass provider on unmanaged hosts", () => {
  for (const secret of ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "tauri_signing_private_key"]) {
    let calls = 0;
    assert.throws(() => runSignUpdater(["--file", "fixture.exe"], {
      env: { [secret]: "fixture-secret" }, platform: "win32", execCredential: () => { calls++; },
    }), /OS credential provider/);
    assert.equal(calls, 0);
  }
});

test("macOS retains existing session provider & receives no inherited key", () => {
  let seen;
  runSignUpdater(["--file", "fixture.tar.gz"], {
    env: {}, platform: "darwin", exists: () => true, remove: () => {}, log: () => {},
    signMac: (files, options) => { seen = { files, options }; },
    execCredential: () => assert.fail("Windows provider must not replace macOS session"),
  });
  assert.equal(seen.options.env.TAURI_SIGNING_PRIVATE_KEY, undefined);
  assert.equal(seen.files.length, 1);
});

test("sign-updater.mjs hard-codes no app name", async () => {
  const { readFileSync } = await import("node:fs");
  assert.doesNotMatch(readFileSync(helper, "utf8"), /ViewRight/);
});
