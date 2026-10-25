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

// A Tauri .sig that only carries a key id: sign-updater reads the id and never verifies the signature.
function signatureWithKeyId(keyIdHex) {
  const raw = Buffer.concat([Buffer.from("Ed"), Buffer.from(keyIdHex, "hex").reverse(), Buffer.alloc(64, 7)]);
  return Buffer.from(`untrusted comment: signature from tauri secret key\n${raw.toString("base64")}\ntrusted comment: t\n${Buffer.alloc(64, 9).toString("base64")}\n`).toString("base64");
}

const SUITE_KEY_ID = "29395F9FF466261D";
const OTHER_KEY_ID = "0123456789ABCDEF";

function signWith({ args, platform = "win32", sigs, signMac }) {
  const cwd = process.cwd();
  const calls = { signed: [], read: [], logs: [], mac: 0 };
  const result = runSignUpdater(args, {
    platform, env: {}, cwd, exists: () => true, remove: () => {},
    log: (line) => calls.logs.push(line),
    resolvePnpm: () => path.join(cwd, "pnpm.cjs"),
    execCredential: (command, argv) => { calls.signed.push(argv.at(-1)); return { status: 0 }; },
    signMac: (files) => { calls.mac++; signMac?.(files); },
    read: (file) => {
      calls.read.push(file);
      if (!(file in sigs)) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
      return sigs[file];
    },
  });
  return { result, calls };
}

test("--key-id accepts a file whose produced signature carries that key id", () => {
  const first = path.resolve("Fixture.exe");
  const second = path.resolve("Other.exe");
  const { result, calls } = signWith({ args: ["--file", "Fixture.exe", "--key-id", SUITE_KEY_ID, "--file", "Other.exe"], sigs: { [`${first}.sig`]: signatureWithKeyId(SUITE_KEY_ID), [`${second}.sig`]: signatureWithKeyId(SUITE_KEY_ID) } });
  assert.deepEqual(result.files, [first, second]);
  assert.deepEqual(calls.signed, [first, second]);
  assert.deepEqual(calls.read, [`${first}.sig`, `${second}.sig`]);
});

test("--key-id refuses a signature by another key, naming the file, the expected and the found id", () => {
  const file = path.resolve("Fixture.exe");
  assert.throws(
    () => signWith({ args: ["--key-id", SUITE_KEY_ID, "--file", "Fixture.exe"], sigs: { [`${file}.sig`]: signatureWithKeyId(OTHER_KEY_ID) } }),
    { message: `updater signature ${file}.sig was not made by updater key ${SUITE_KEY_ID} (found ${OTHER_KEY_ID})` },
  );
});

test("--key-id stops after the first wrong signature instead of signing the rest", () => {
  const signed = [];
  const cwd = process.cwd();
  assert.throws(() => runSignUpdater(["--key-id", SUITE_KEY_ID, "--file", "a.exe", "--file", "b.exe"], {
    platform: "win32", env: {}, cwd, exists: () => true, remove: () => {}, log: () => {}, resolvePnpm: () => "pnpm.cjs",
    execCredential: (command, argv) => { signed.push(path.basename(argv.at(-1))); return { status: 0 }; },
    read: () => signatureWithKeyId(OTHER_KEY_ID),
  }), /was not made by updater key/);
  assert.deepEqual(signed, ["a.exe"]);
});

test("--key-id refuses a missing, unreadable or malformed signature as an unreadable key id", () => {
  const file = path.resolve("Fixture.exe");
  for (const sigs of [{}, { [`${file}.sig`]: "not a signature" }, { [`${file}.sig`]: "" }]) {
    assert.throws(() => signWith({ args: ["--file", "Fixture.exe", "--key-id", SUITE_KEY_ID], sigs }), (error) => error.message === `updater signature ${file}.sig was not made by updater key ${SUITE_KEY_ID} (found unreadable)`);
  }
});

test("--key-id checks the signatures the macOS session wrote, after it signed", () => {
  const file = path.resolve("Fixture.app.tar.gz");
  const args = ["--file", "Fixture.app.tar.gz", "--key-id", SUITE_KEY_ID];
  const { calls } = signWith({ platform: "darwin", args, sigs: { [`${file}.sig`]: signatureWithKeyId(SUITE_KEY_ID) } });
  assert.equal(calls.mac, 1);
  assert.deepEqual(calls.read, [`${file}.sig`]);
  let signedBeforeRefusal = false;
  assert.throws(() => signWith({ platform: "darwin", args, sigs: { [`${file}.sig`]: signatureWithKeyId(OTHER_KEY_ID) }, signMac: () => { signedBeforeRefusal = true; } }), /found 0123456789ABCDEF/);
  assert.equal(signedBeforeRefusal, true);
});

test("--key-id dry run prints the expected id and neither signs nor reads a signature", () => {
  const cwd = process.cwd();
  const logs = [];
  runSignUpdater(["--file", "Fixture.exe", "--key-id", SUITE_KEY_ID, "--dry-run"], {
    platform: "win32", env: {}, cwd, log: (line) => logs.push(line),
    exists: () => assert.fail("dry run must not touch the artifact"), remove: () => assert.fail("dry run must not remove a signature"),
    execCredential: () => assert.fail("dry run must not sign"), signMac: () => assert.fail("dry run must not sign"), read: () => assert.fail("dry run must not read a signature"),
  });
  assert.equal(logs.length, 2);
  assert.match(logs[0], /tauri signer sign .*Fixture\.exe/);
  assert.equal(logs[1], `[dry-run] ${path.resolve(cwd, "Fixture.exe")}.sig must be made by updater key ${SUITE_KEY_ID}`);
});

test("without --key-id no signature is read back and nothing else changes", () => {
  const cwd = process.cwd();
  const logs = [];
  runSignUpdater(["--file", "Fixture.exe"], {
    platform: "win32", env: {}, cwd, exists: () => true, remove: () => {}, log: (line) => logs.push(line), resolvePnpm: () => "pnpm.cjs",
    execCredential: () => ({ status: 0 }), read: () => assert.fail("no key id was requested"),
  });
  assert.equal(logs.length, 1);
  runSignUpdater(["--file", "Fixture.exe", "--dry-run"], { platform: "win32", env: {}, cwd, log: (line) => logs.push(line) });
  assert.equal(logs.length, 2, "a dry run without --key-id prints no id line");
});

test("--key-id must be 16 upper-case hex digits and may be given once", () => {
  const run = (...args) => runSignUpdater(["--file", "Fixture.exe", "--dry-run", ...args], { env: {}, log: () => {} });
  run("--key-id", SUITE_KEY_ID);
  for (const value of [SUITE_KEY_ID.toLowerCase(), SUITE_KEY_ID.slice(1), `${SUITE_KEY_ID}0`, "", "G395F9FF466261D0", "--dry-run"]) {
    assert.throws(() => run("--key-id", value), /--key-id requires the 16 upper-case hex digits/, JSON.stringify(value));
  }
  assert.throws(() => run("--key-id"), /--key-id requires the 16 upper-case hex digits/);
  assert.throws(() => run("--key-id", SUITE_KEY_ID, "--key-id", SUITE_KEY_ID), /--key-id may be given only once/);
  assert.throws(() => run("--key-id", SUITE_KEY_ID, "--key-id", OTHER_KEY_ID), /--key-id may be given only once/);
});

test("the sign-updater command refuses a malformed --key-id and prints the expected one on a dry run", () => {
  const bad = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--key-id", "29395f9ff466261d", "--dry-run"], { encoding: "utf8" });
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /sign-updater: --key-id requires the 16 upper-case hex digits/);
  const ok = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--key-id", SUITE_KEY_ID, "--dry-run"], { encoding: "utf8" });
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, new RegExp(`Fixture\\.exe\\.sig must be made by updater key ${SUITE_KEY_ID}`));
});
