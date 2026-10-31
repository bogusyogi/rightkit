import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawn } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  ANDROID_ENV_SECRET_REFUSED, ANDROID_RECEIPT_NAME, ANDROID_RELEASE_CANCELLED, ANDROID_SECRET_MISSING, ANDROID_SIGNING_CONTRACT, ANDROID_SIGNING_PROPERTIES_ENV,
  androidEnvSecretNames, androidSealEntry, createAndroidCredentialStore, createSigningProperties, escapeJavaProperty,
  readSecretFile, refuseAndroidEnvSecrets, resolveAndroidSecret, runAndroidRelease, validateAndroidTarget, verifyAndroidReceipt, verifyAabSigner,
} from "./android-release.mjs";
import { createNativeRunner } from "./build-launch.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";
import { NATIVE_SIGNING_FILE_ENV } from "./native-adapters.mjs";
import { FIXTURE_CERT_SHA256, aab, fakeCertificate, markedLib } from "./android-test-fixtures.mjs";

const PASSWORD = "S3cret pass=word#\u00e9";
const AAB_FILE = "android/app/build/outputs/bundle/release/app-release.aab";
const posix = process.platform !== "win32";

function workspace(t) {
  const base = mkdtempSync(path.join(tmpdir(), "rk-android-release-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const root = path.join(base, "app"); const keys = path.join(base, "keys"); const scratch = path.join(base, "scratch");
  for (const dir of [root, keys, scratch]) mkdirSync(dir, { recursive: true });
  const keystore = path.join(keys, "upload.jks");
  writeFileSync(keystore, "fixture keystore bytes");
  return { base, root, keys, scratch, keystore };
}

function targetConfig(ws, over = {}) {
  return {
    signed: true,
    signingContract: ANDROID_SIGNING_CONTRACT,
    gradle: { cmd: "./gradlew", args: [":app:bundleRelease"], cwd: "android" },
    aab: { file: AAB_FILE },
    artifacts: [AAB_FILE],
    signing: { keystore: { path: ws.keystore, alias: "upload" }, storePassword: { credential: "fixture-store" }, expectedCertSha256: FIXTURE_CERT_SHA256 },
    ...over,
  };
}
const withSigning = (ws, signing) => targetConfig(ws, { signing: { ...targetConfig(ws).signing, ...signing } });
const store = () => createAndroidCredentialStore({ env: { USER: "tester" }, platform: "darwin", readKeychain: ({ service }) => (service === "rightkit-android-fixture-store" ? `${PASSWORD}\n` : undefined) });

test("a valid target normalises; the AAB, task and pin are carried", (t) => {
  const ws = workspace(t);
  const valid = validateAndroidTarget(targetConfig(ws), { root: ws.root });
  assert.equal(valid.task, ":app:bundleRelease");
  assert.equal(valid.aab.file, AAB_FILE);
  assert.equal(valid.signing.expectedCertSha256, FIXTURE_CERT_SHA256);
  assert.equal(valid.signing.keystorePath, ws.keystore);
  const pinned = validateAndroidTarget(withSigning(ws, { expectedCertSha256: FIXTURE_CERT_SHA256.toUpperCase().replace(/(..)(?!$)/g, "$1:") }), { root: ws.root });
  assert.equal(pinned.signing.expectedCertSha256, FIXTURE_CERT_SHA256);
  assert.equal(validateAndroidTarget(targetConfig(ws, { gradle: { cmd: "gradle", args: ["--no-daemon", "bundleProdRelease"] } }), { root: ws.root }).task, "bundleProdRelease");
});

test("validation refuses what the release tier refuses", (t) => {
  const ws = workspace(t);
  const refused = (config, pattern) => assert.throws(() => validateAndroidTarget(config, { root: ws.root }), pattern);
  refused(targetConfig(ws, { signed: false }), /signed: true/);
  refused(targetConfig(ws, { signingContract: "other" }), /signingContract/);
  refused(targetConfig(ws, { gradle: undefined }), /gradle/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew bundleRelease", args: ["x"] } }), /one executable/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:bundleDebug"] } }), /debug build/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:assembleRelease"] } }), /bundle<Variant>Release/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:bundleRelease", "-PstorePassword=hunter2"] } }), /password, signing or debuggable/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:bundleRelease", "-Pandroid.injected.signing.store.file=x"] } }), /password, signing or debuggable/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:bundleRelease", "-Pandroid.debuggable=true"] } }), /debuggable/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:bundleRelease"], env: { KEYSTORE_PASSWORD: "x" } } }), /gradle\.env\.KEYSTORE_PASSWORD/);
  refused(targetConfig(ws, { gradle: { cmd: "./gradlew", args: [":app:bundleRelease"], envFiles: { A: "b" } } }), /envFiles/);
  refused(targetConfig(ws, { aab: { file: "out/app.apk" } }), /\.aab/);
  refused(targetConfig(ws, { artifacts: ["other.aab"] }), /artifacts must list aab\.file/);
  refused(targetConfig(ws, { signing: undefined }), /signing/);
});

test("validation refuses inline secrets, unknown signing keys, in-repo and debug keystores, and a missing pin", (t) => {
  const ws = workspace(t);
  const refused = (signing, pattern) => assert.throws(() => validateAndroidTarget(withSigning(ws, signing), { root: ws.root }), pattern);
  refused({ storePassword: "hunter2" }, /inline secret is refused/);
  refused({ storePassword: { value: "hunter2" } }, /unsupported key/);
  refused({ storePassword: { credential: "a", file: "b" } }, /exactly one|unsupported/);
  refused({ keyPassword: "hunter2" }, /inline secret is refused/);
  refused({ storePassword: { credential: "Has Spaces" } }, /lowercase name/);
  refused({ password: "hunter2" }, /unsupported key\(s\) password/);
  refused({ keystore: { path: path.join(ws.root, "release.jks"), alias: "upload" } }, /outside the repository/);
  refused({ keystore: { path: path.join(ws.keys, "debug.keystore"), alias: "upload" } }, /debug keystore/);
  refused({ keystore: { path: ws.keystore, alias: "androiddebugkey" } }, /debug keystore/);
  refused({ keystore: { path: ws.keystore, alias: "upload", storepass: "x" } }, /only path and alias/);
  refused({ expectedCertSha256: undefined }, /expectedCertSha256/);
  refused({ expectedCertSha256: "abc" }, /64 hex/);
});

test("ambient signing passwords are refused on every host, and the message names only the variable", () => {
  const secret = "hunter2-value";
  for (const name of ["ANDROID_KEYSTORE_PASSWORD", "KEYSTORE_PASSWORD", "KEY_PASSWORD", "RELEASE_STORE_PASSWORD", "ORG_GRADLE_PROJECT_storePassword", "SIGNING_KEY_PASSWORD", "ANDROID_KEY_PASS"]) {
    assert.throws(() => refuseAndroidEnvSecrets({ [name]: secret }), (error) => {
      assert.equal(error.code, ANDROID_ENV_SECRET_REFUSED);
      assert.match(error.message, new RegExp(name));
      assert.doesNotMatch(`${error.message}\n${error.stack}`, new RegExp(secret));
      return true;
    }, name);
  }
  assert.deepEqual(androidEnvSecretNames({ GRADLE_OPTS: "-Pandroid.injected.signing.store.password=abc" }), ["GRADLE_OPTS"]);
  assert.deepEqual(androidEnvSecretNames({ JAVA_TOOL_OPTIONS: "-Dfile.encoding=UTF-8" }), []);
  assert.doesNotThrow(() => refuseAndroidEnvSecrets({ ANDROID_KEYSTORE_PASSWORD: "  ", PATH: "/usr/bin", [ANDROID_SIGNING_PROPERTIES_ENV]: "/tmp/x/signing.properties", ANDROID_HOME: "/sdk" }));
});

test("credential store: Keychain on macOS, token file elsewhere, and a missing secret names the store command only", () => {
  assert.equal(store().read("fixture-store"), PASSWORD);
  assert.throws(() => store().read("absent"), (error) => {
    assert.equal(error.code, ANDROID_SECRET_MISSING);
    assert.match(error.message, /security add-generic-password -a "\$USER" -s rightkit-android-absent -U -w/);
    return true;
  });
  const seen = [];
  const windows = createAndroidCredentialStore({ env: { APPDATA: "C:\\Users\\t\\AppData\\Roaming" }, platform: "win32", readFile: (file) => { seen.push(file); return " win-secret \r\n"; } });
  assert.equal(windows.read("fixture-store"), "win-secret");
  assert.equal(seen[0], "C:\\Users\\t\\AppData\\Roaming\\RightKit\\android-fixture-store.token");
  const linux = createAndroidCredentialStore({ env: { XDG_CONFIG_HOME: "/cfg" }, platform: "linux", readFile: (file) => { seen.push(file); return undefined; } });
  assert.throws(() => linux.read("fixture-store"), /umask 077/);
  assert.equal(seen[1], path.join("/cfg", "RightKit", "android-fixture-store.token"));
});

test("file secrets: read from outside the repository, trimmed; empty, missing or in-repo files are refused", (t) => {
  const ws = workspace(t);
  const file = path.join(ws.keys, "key.pass");
  writeFileSync(file, `${PASSWORD}\n`, { mode: 0o600 });
  assert.equal(resolveAndroidSecret({ file }, { root: ws.root, store: store(), label: "k" }), PASSWORD);
  assert.throws(() => resolveAndroidSecret({ file: path.join(ws.keys, "none") }, { root: ws.root, store: store(), label: "k" }), /missing or empty/);
  writeFileSync(path.join(ws.keys, "empty.pass"), "\n", { mode: 0o600 });
  assert.throws(() => resolveAndroidSecret({ file: path.join(ws.keys, "empty.pass") }, { root: ws.root, store: store(), label: "k" }), /missing or empty/);
  const inRepo = path.join(ws.root, "key.pass");
  writeFileSync(inRepo, "x", { mode: 0o600 });
  assert.throws(() => resolveAndroidSecret({ file: inRepo }, { root: ws.root, store: store(), label: "k" }), /outside the repository/);
  assert.equal(resolveAndroidSecret({ credential: "fixture-store" }, { root: ws.root, store: store(), label: "k" }), PASSWORD);
});

test("a group- or world-readable secret file is refused on POSIX", { skip: !posix }, (t) => {
  const ws = workspace(t);
  const file = path.join(ws.keys, "loose.pass");
  writeFileSync(file, "x");
  chmodSync(file, 0o644);
  assert.throws(() => readSecretFile(file), /chmod 600/);
  chmodSync(file, 0o600);
  assert.equal(readSecretFile(file), "x");
});

test("Java properties escaping covers separators, backslashes, controls and non-Latin-1 characters", () => {
  assert.equal(escapeJavaProperty("a b=c:d#e!f"), "a\\ b\\=c\\:d\\#e\\!f");
  assert.equal(escapeJavaProperty("C:\\keys\\u.jks"), "C\\:\\\\keys\\\\u.jks");
  assert.equal(escapeJavaProperty("line1\nline2\r\t"), "line1\\nline2\\r\\t");
  assert.equal(escapeJavaProperty("\u00e9\u20ac"), "\\u00e9\\u20ac");
});

test("transient signing properties: private file with the four keys, removed (and overwritten) on cleanup", (t) => {
  const ws = workspace(t);
  const props = createSigningProperties({ keystore: ws.keystore, alias: "upload", storePassword: PASSWORD, keyPassword: "key pass", tmpRoot: ws.scratch });
  const text = readFileSync(props.file, "utf8");
  assert.match(text, /^storeFile=/m);
  assert.ok(text.split("\n").includes(`storePassword=${escapeJavaProperty(PASSWORD)}`));
  assert.match(text, /^keyAlias=upload$/m);
  assert.match(text, /^keyPassword=key\\ pass$/m);
  assert.equal(path.dirname(path.dirname(props.file)), ws.scratch);
  if (posix) assert.equal(statSync(props.file).mode & 0o077, 0);
  props.cleanup();
  assert.equal(existsSync(props.file), false);
  assert.equal(existsSync(props.dir), false);
  assert.doesNotThrow(() => props.cleanup());
});

function runner(ws, config, hooks = {}) {
  const target = validateAndroidTarget(config, { root: ws.root });
  const aabPath = path.join(ws.root, AAB_FILE);
  const record = { logs: [], scans: [], gradleCalls: [] };
  const run = (over = {}) => runAndroidRelease({
    root: ws.root, app: "fixture", target, env: { PATH: "/usr/bin" }, platform: "darwin", credentialStore: store(), tmpRoot: ws.scratch,
    receiptDir: path.join(ws.root, ".right-release", "receipts"), log: (line) => record.logs.push(line),
    now: () => new Date("2026-10-16T00:00:00Z"),
    runGradle: async (gradle, extraEnv) => {
      record.gradleCalls.push({ gradle, extraEnv });
      const file = extraEnv[ANDROID_SIGNING_PROPERTIES_ENV];
      record.propsDuringBuild = readFileSync(file, "utf8");
      record.propsFile = file;
      if (hooks.gradle) await hooks.gradle({ aabPath });
      else { mkdirSync(path.dirname(aabPath), { recursive: true }); writeFileSync(aabPath, aab()); }
    },
    resolveArtifact: (declared) => path.resolve(ws.root, declared),
    scanFiles: (label, files, options) => { record.scans.push({ label, files: [...files], options, present: files.map((f) => existsSync(f)) }); },
    ...over,
  });
  return { run, record, aabPath, target };
}

test("release lane: builds through the configured command, scans, pins the signer and writes the receipt", async (t) => {
  const ws = workspace(t);
  const { run, record, aabPath } = runner(ws, targetConfig(ws));
  mkdirSync(path.dirname(aabPath), { recursive: true });
  writeFileSync(aabPath, "stale bundle from an earlier build");
  const result = await run();

  assert.equal(record.gradleCalls.length, 1);
  assert.equal(record.gradleCalls[0].gradle.cmd, "./gradlew");
  // Only a path reaches Gradle; the secret is in the private file during the build and nowhere else.
  assert.deepEqual(Object.keys(record.gradleCalls[0].extraEnv), [ANDROID_SIGNING_PROPERTIES_ENV]);
  assert.equal(Object.values(record.gradleCalls[0].extraEnv).some((value) => value.includes("S3cret")), false);
  assert.match(record.propsDuringBuild, /storePassword=S3cret\\ pass\\=word\\#\\u00e9/);
  assert.equal(existsSync(record.propsFile), false, "signing properties are removed after the build");
  assert.equal(readdirSync(ws.scratch).filter((name) => name.startsWith("rightkit-android-")).length, 0, "no temp directory is left behind");

  // The stale bundle was replaced; the same shared scan the desktop lanes use ran over the AAB and over its .so files.
  assert.deepEqual(record.scans.map((scan) => scan.label), ["android AAB", "android AAB native libraries"]);
  assert.deepEqual(record.scans[0].options, { requireBinary: true, prePack: true });
  assert.equal(record.scans[1].files.length, 2);
  assert.deepEqual(record.scans[1].present, [true, true]);

  const bytes = readFileSync(aabPath);
  assert.equal(result.receipt.file.sha256, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(result.receipt.file.sizeBytes, bytes.length);
  assert.equal(result.receipt.signer.certSha256, FIXTURE_CERT_SHA256);
  assert.equal(result.receipt.scan.nativeLibs.length, 2);
  assert.deepEqual(result.sealEntry, { role: "aab", name: "app-release.aab", sha256: result.receipt.file.sha256, sizeBytes: bytes.length });
  assert.deepEqual(androidSealEntry(result.receipt), result.sealEntry);
  assert.equal(path.basename(result.receiptPath), ANDROID_RECEIPT_NAME);
  assert.equal(verifyAndroidReceipt(result.receiptPath, aabPath).file.sha256, result.receipt.file.sha256);

  const everything = `${record.logs.join("\n")}\n${readFileSync(result.receiptPath, "utf8")}`;
  assert.doesNotMatch(everything, /S3cret|pass=word/);
});

test("release lane: key password from a file, store password from the credential store", async (t) => {
  const ws = workspace(t);
  const keyFile = path.join(ws.keys, "key.pass");
  writeFileSync(keyFile, "separate-key-pass\n", { mode: 0o600 });
  const { run, record } = runner(ws, withSigning(ws, { keyPassword: { file: keyFile } }));
  await run();
  assert.match(record.propsDuringBuild, /^storePassword=S3cret/m);
  assert.match(record.propsDuringBuild, /^keyPassword=separate-key-pass$/m);
});

test("release lane: a failing Gradle step still removes the signing properties", async (t) => {
  const ws = workspace(t);
  const { run, record } = runner(ws, targetConfig(ws), { gradle: async () => { throw new Error("gradle failed"); } });
  await assert.rejects(run(), /gradle failed/);
  assert.equal(existsSync(record.propsFile), false);
  assert.equal(readdirSync(ws.scratch).length, 0);
});

test("release lane: refuses before reading any secret when the environment carries a signing password", async (t) => {
  const ws = workspace(t);
  let read = false;
  const { run, record } = runner(ws, targetConfig(ws));
  await assert.rejects(run({ env: { KEYSTORE_PASSWORD: "hunter2" }, credentialStore: { read: () => { read = true; return "x"; } } }), (error) => error.code === ANDROID_ENV_SECRET_REFUSED);
  assert.equal(read, false);
  assert.equal(record.gradleCalls.length, 0);
});

test("release lane: a missing keystore or secret stops before Gradle", async (t) => {
  const ws = workspace(t);
  const { run, record } = runner(ws, targetConfig(ws));
  await assert.rejects(run({ credentialStore: createAndroidCredentialStore({ env: {}, platform: "darwin", readKeychain: () => undefined }) }), (error) => error.code === ANDROID_SECRET_MISSING);
  rmSync(ws.keystore);
  await assert.rejects(run(), /keystore file is missing/);
  assert.equal(record.gradleCalls.length, 0);
});

test("release lane: the release-tier refusals reject the bundle and no receipt is written", async (t) => {
  const cases = [
    ["QA control marker in a library", aab({ libs: [{ name: "base/lib/arm64-v8a/libapp.so", data: markedLib() }] }), /contains RIGHTKIT-QA-CONTROL-SERVER-PRESENT/],
    ["no native library", aab({ libs: [{ name: "base/dex/classes.dex", data: Buffer.from("dex") }] }), /no native library/],
    ["unsigned bundle", aab({ signed: false }), /not signed/],
    ["browser-QA mock in the web assets", aab({ extra: [{ name: "base/assets/m.js", data: Buffer.from("tauriQaMock") }] }), /browser-QA mock marker/],
  ];
  for (const [name, bytes, pattern] of cases) {
    const ws = workspace(t);
    const { run } = runner(ws, targetConfig(ws), { gradle: async ({ aabPath }) => { mkdirSync(path.dirname(aabPath), { recursive: true }); writeFileSync(aabPath, bytes); } });
    await assert.rejects(run(), pattern, name);
    assert.equal(existsSync(path.join(ws.root, ".right-release", "receipts", ANDROID_RECEIPT_NAME)), false, name);
  }
});

test("release lane: a bundle signed by another key, or the debug key, is refused", async (t) => {
  for (const [name, certificate, code] of [
    ["other key", fakeCertificate("CN=Someone Else"), "ANDROID_AAB_SIGNER_MISMATCH"],
    ["debug key", fakeCertificate("CN=Android Debug,O=Android,C=US"), "ANDROID_AAB_DEBUG_SIGNED"],
  ]) {
    const ws = workspace(t);
    const { run } = runner(ws, targetConfig(ws), { gradle: async ({ aabPath }) => { mkdirSync(path.dirname(aabPath), { recursive: true }); writeFileSync(aabPath, aab({ certificate })); } });
    await assert.rejects(run(), (error) => error.code === code, name);
  }
  assert.throws(() => verifyAabSigner({ certSha256: FIXTURE_CERT_SHA256, debugCertificate: true }, { expectedCertSha256: FIXTURE_CERT_SHA256 }), /debug certificate/);
  assert.throws(() => verifyAabSigner({ certSha256: FIXTURE_CERT_SHA256, debugCertificate: false, notAfter: Date.parse("2020-01-01") }, { expectedCertSha256: FIXTURE_CERT_SHA256 }, new Date("2026-10-16")), /expired/);
  assert.throws(() => verifyAabSigner(null, { expectedCertSha256: FIXTURE_CERT_SHA256 }), /no readable upload-key signature/);
});

test("release lane: Gradle that produces no bundle fails", async (t) => {
  const ws = workspace(t);
  const { run } = runner(ws, targetConfig(ws), { gradle: async () => {} });
  await assert.rejects(run(), /was not produced/);
});

test("release lane dry run: validates, never reads a secret, never scans, writes nothing", async (t) => {
  const ws = workspace(t);
  let read = false; let gradleEnv;
  const { run, record } = runner(ws, targetConfig(ws));
  const result = await run({ credentialStore: { read: () => { read = true; return "x"; } }, dryRun: true, runGradle: async (_gradle, extraEnv) => { gradleEnv = extraEnv; } });
  assert.equal(result.dryRun, true);
  assert.equal(read, false);
  assert.deepEqual(gradleEnv, {});
  assert.equal(record.scans.length, 0);
  assert.equal(existsSync(path.join(ws.root, ".right-release")), false);
});

test("a receipt no longer matches a changed bundle", async (t) => {
  const ws = workspace(t);
  const { run, aabPath } = runner(ws, targetConfig(ws));
  const { receiptPath } = await run();
  assert.doesNotThrow(() => verifyAndroidReceipt(receiptPath, aabPath));
  writeFileSync(aabPath, aab({ extra: [{ name: "base/assets/late.txt", data: Buffer.from("added after signing") }] }));
  assert.throws(() => verifyAndroidReceipt(receiptPath, aabPath), /differ from the build receipt/);
});

// ---- RA-08: the signing file's lifetime and the door's verdict on its handoff --------------------------------------------------------------------
// Real host descriptions of createNativeRunner (build-launch.mjs), never an executor: `plan` maps and spawns nothing. A direct Linux host has no broker (its own Gradle gets the path);
// a Mac host is a broker route, where bundle<Variant>Release is refused by the grammar and the signing file has no capability yet (native-signing-file-v1; the signed phase is OPEN and off).
const directHost = (root) => createNativeRunner({ cwd: root, env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [], readFile: () => "" });
const brokerHost = (root) => {
  const env = { PATH: "/usr/bin", HOME: "/Users/rk" };
  const client = managedRuntimeClient(env, "darwin", () => true);
  return createNativeRunner({ cwd: root, env, platform: "darwin", exists: (file) => file === client, which: (command) => (command === "rustright" ? ["/elsewhere/on/path/rustright"] : []), readFile: () => "" });
};

test("release lane: the broker route refuses the signed phase before any secret is read or any file is written", async (t) => {
  const ws = workspace(t);
  let read = false;
  const { run, record } = runner(ws, targetConfig(ws));
  await assert.rejects(run({ native: brokerHost(ws.root), credentialStore: { read: () => { read = true; return "x"; } } }),
    (error) => error.code === "RK_NATIVE_TOOL_REFUSED" && /bundleRelease/.test(error.message) && /no secret was read and no signing file was written/.test(error.message) && !error.message.includes(PASSWORD));
  assert.equal(read, false);
  assert.equal(record.gradleCalls.length, 0);
  assert.equal(readdirSync(ws.scratch).length, 0, "no signing directory, not even the probe path, was created");
  assert.equal(existsSync(path.join(ws.root, ".right-release")), false);
});

test("release lane: a direct host's door carries the handoff, and the lane still hands Gradle only the path", async (t) => {
  const ws = workspace(t);
  const { run, record } = runner(ws, targetConfig(ws));
  const result = await run({ native: directHost(ws.root) });
  assert.equal(record.gradleCalls.length, 1);
  assert.deepEqual(Object.keys(record.gradleCalls[0].extraEnv), [NATIVE_SIGNING_FILE_ENV]);
  assert.equal(NATIVE_SIGNING_FILE_ENV, ANDROID_SIGNING_PROPERTIES_ENV);
  assert.equal(existsSync(record.propsFile), false);
  assert.equal(readdirSync(ws.scratch).length, 0);
  assert.equal(result.receipt.signer.certSha256, FIXTURE_CERT_SHA256);
});

test("release lane dry run on a broker route reports the door's refusal and still reads no secret", async (t) => {
  const ws = workspace(t);
  let read = false;
  const { run, record } = runner(ws, targetConfig(ws));
  const result = await run({ dryRun: true, native: brokerHost(ws.root), credentialStore: { read: () => { read = true; return "x"; } }, runGradle: async () => {} });
  assert.equal(result.dryRun, true);
  assert.equal(read, false);
  assert.ok(record.logs.some((line) => /dry-run: the door refuses this Gradle step.*RK_NATIVE_TOOL_REFUSED/.test(line)));
  assert.equal(readdirSync(ws.scratch).length, 0);
});

test("release lane: cancelling before the signing phase reads no secret; cancelling during Gradle removes the signing file and writes no receipt", async (t) => {
  const ws = workspace(t);
  let read = false;
  const early = new AbortController(); early.abort();
  const first = runner(ws, targetConfig(ws));
  await assert.rejects(first.run({ signal: early.signal, credentialStore: { read: () => { read = true; return "x"; } } }), (error) => error.code === ANDROID_RELEASE_CANCELLED);
  assert.equal(read, false);
  assert.equal(first.record.gradleCalls.length, 0);

  const during = new AbortController();
  const second = runner(ws, targetConfig(ws), { gradle: async ({ aabPath }) => { mkdirSync(path.dirname(aabPath), { recursive: true }); writeFileSync(aabPath, aab()); during.abort(); } });
  await assert.rejects(second.run({ signal: during.signal }), (error) => error.code === ANDROID_RELEASE_CANCELLED);
  assert.equal(existsSync(second.record.propsFile), false);
  assert.equal(readdirSync(ws.scratch).length, 0);
  assert.equal(existsSync(path.join(ws.root, ".right-release", "receipts", ANDROID_RECEIPT_NAME)), false);
});

test("release lane: the owned signing file is private, its directory is private, and only its path is handed to Gradle", { skip: !posix }, async (t) => {
  const ws = workspace(t);
  let modes;
  const { run } = runner(ws, targetConfig(ws), { gradle: async ({ aabPath }) => {
    const file = readdirSync(ws.scratch).map((name) => path.join(ws.scratch, name, "signing.properties"))[0];
    modes = { file: statSync(file).mode & 0o777, dir: statSync(path.dirname(file)).mode & 0o777 };
    mkdirSync(path.dirname(aabPath), { recursive: true }); writeFileSync(aabPath, aab());
  } });
  await run();
  assert.deepEqual(modes, { file: 0o600, dir: 0o700 });
});

// A cancelling signal ends the process by its default action; the signing file must be gone by then. Real child process, real signal (the handlers are holdSigningFile's).
for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  test(`the signing file is removed when ${signal} cancels the process, and the process still dies of the signal`, { skip: !posix }, async (t) => {
    const ws = workspace(t);
    const module = pathToFileURL(fileURLToPath(new URL("./android-release.mjs", import.meta.url))).href;
    const code = `import { createSigningProperties, holdSigningFile } from ${JSON.stringify(module)};
      const props = createSigningProperties({ keystore: "/keys/upload.jks", alias: "upload", storePassword: "pw", keyPassword: "pw", tmpRoot: process.env.RK_SCRATCH });
      holdSigningFile(props); process.stdout.write(props.file + "\\n"); setInterval(() => {}, 1000);`;
    const child = spawn(process.execPath, ["--input-type=module", "-e", code], { cwd: path.dirname(fileURLToPath(import.meta.url)), env: { PATH: process.env.PATH, RK_SCRATCH: ws.scratch }, stdio: ["ignore", "pipe", "inherit"] });
    t.after(() => child.kill("SIGKILL"));
    const file = await new Promise((resolve, reject) => { let out = ""; child.stdout.on("data", (chunk) => { out += chunk; if (out.includes("\n")) resolve(out.trim()); }); child.once("error", reject); child.once("exit", () => reject(new Error("child ended early"))); });
    assert.equal(existsSync(file), true, "the file exists while the phase is held");
    const ended = new Promise((resolve) => child.once("exit", (status, sig) => resolve({ status, sig })));
    child.kill(signal);
    assert.deepEqual(await ended, { status: null, sig: signal });
    assert.equal(existsSync(file), false);
    assert.equal(existsSync(path.dirname(file)), false);
  });
}

test("ndk.jniLibs with a .git path segment is refused (the directory is emptied before every build)", (t) => {
  const ws = workspace(t);
  const refused = (jniLibs) => assert.throws(() => validateAndroidTarget(targetConfig(ws, { ndk: { jniLibs } }), { root: ws.root }), /\.git path segment/);
  refused(".git/jniLibs");
  refused("android/.GIT/app/jniLibs");
  refused("android\\.git\\jniLibs");
  assert.doesNotThrow(() => validateAndroidTarget(targetConfig(ws, { ndk: { jniLibs: "android/app/src/main/jniLibs" } }), { root: ws.root }));
  assert.doesNotThrow(() => validateAndroidTarget(targetConfig(ws, { ndk: { jniLibs: "android/.github-libs/jniLibs" } }), { root: ws.root }));
});
