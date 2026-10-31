import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { ANDROID_SIGNING_CONTRACT, runAndroidRelease, validateAndroidTarget } from "./android-release.mjs";
import { androidNdkCommand, runAndroidNdk, validateAndroidNdk } from "./android-ndk.mjs";
import { FIXTURE_CERT_SHA256 } from "./android-test-fixtures.mjs";

const AAB_FILE = "android/app/build/outputs/bundle/release/app-release.aab";
const JNI = "android/app/src/main/jniLibs";

function workspace(t) {
  const base = mkdtempSync(path.join(tmpdir(), "rk-android-ndk-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const root = path.join(base, "app");
  mkdirSync(root, { recursive: true });
  const keystore = path.join(base, "upload.jks");
  writeFileSync(keystore, "fixture keystore bytes");
  return { base, root, keystore };
}

test("the release default is arm64-v8a with --release; the dev default adds the x86_64 emulator and no --release", (t) => {
  const { root } = workspace(t);
  const release = validateAndroidNdk({ jniLibs: JNI }, { root });
  assert.deepEqual(release.abis, ["arm64-v8a"]);
  const command = androidNdkCommand(release);
  assert.equal(command.cmd, "cargo");
  assert.deepEqual(command.args, ["ndk", "-t", "arm64-v8a", "-o", path.join(root, ...JNI.split("/")), "build", "--locked", "--release"]);
  const dev = validateAndroidNdk({ jniLibs: JNI }, { root, kind: "dev" });
  assert.deepEqual(dev.abis, ["arm64-v8a", "x86_64"]);
  const devArgs = androidNdkCommand(dev, { profile: "dev" }).args;
  assert.deepEqual(devArgs.filter((arg) => arg === "-t" || /^(arm64|x86)/.test(arg)), ["-t", "arm64-v8a", "-t", "x86_64"]);
  assert.ok(!devArgs.includes("--release"));
});

test("manifest, package, features and API level become fixed argument positions, never a shell string", (t) => {
  const { root } = workspace(t);
  const ndk = validateAndroidNdk({ abis: ["arm64-v8a", "x86_64"], jniLibs: JNI, manifestPath: "rust/Cargo.toml", package: "heardright-android", features: ["a", "b"], apiLevel: 26, cwd: "." }, { root });
  const { args } = androidNdkCommand(ndk);
  assert.deepEqual(args.slice(0, 2), ["ndk", "--platform"]);
  assert.equal(args[2], "26");
  assert.deepEqual(args.slice(-7), ["--release", "--manifest-path", "rust/Cargo.toml", "-p", "heardright-android", "--features", "a,b"]);
  assert.ok(args.every((arg) => typeof arg === "string"));
});

test("configuration mistakes are refused: ABIs, free-form arguments, paths outside the app, a jniLibs that is not generated", (t) => {
  const { root } = workspace(t);
  const bad = (ndk, pattern) => assert.throws(() => validateAndroidNdk(ndk, { root }), pattern);
  bad(null, /must be an object/);
  bad({ jniLibs: JNI, abis: [] }, /non-empty list/);
  bad({ jniLibs: JNI, abis: ["arm64-v8a", "arm64-v8a"] }, /distinct/);
  bad({ jniLibs: JNI, abis: ["mips"] }, /ABIs from/);
  bad({ jniLibs: JNI, args: ["--debug"] }, /not free-form/);
  bad({ jniLibs: JNI, profile: "dev" }, /not free-form/);
  bad({ abis: ["arm64-v8a"] }, /relative path inside the app/);
  bad({ jniLibs: path.join(root, "x", "jniLibs") }, /relative path inside the app/);
  bad({ jniLibs: "../jniLibs" }, /jniLibs inside the app/);
  bad({ jniLibs: "jniLibs/.." }, /jniLibs inside the app/);
  bad({ jniLibs: "android/libs" }, /called jniLibs/);
  bad({ jniLibs: "." }, /called jniLibs|relative path/);
  bad({ jniLibs: JNI, manifestPath: "../Cargo.toml" }, /manifestPath/);
  bad({ jniLibs: JNI, features: ["a b"] }, /feature names/);
  bad({ jniLibs: JNI, package: "x;rm" }, /package name/);
  bad({ jniLibs: JNI, apiLevel: 3 }, /API level/);
  bad({ jniLibs: JNI, cmd: "cargo ndk" }, /one executable/);
  bad({ jniLibs: JNI, cwd: ".." }, /cwd must stay inside/);
});

test("runAndroidNdk empties the generated directory first, then runs the one command; a dry run only plans", async (t) => {
  const { root } = workspace(t);
  const ndk = validateAndroidNdk({ jniLibs: JNI }, { root });
  const order = [];
  await runAndroidNdk({ ndk, clean: (dir) => order.push(["clean", dir]), run: async (command) => { order.push(["run", command.cmd]); }, log: () => {} });
  assert.deepEqual(order, [["clean", ndk.jniLibs], ["run", "cargo"]]);
  const dry = [];
  await runAndroidNdk({ ndk, dryRun: true, clean: () => dry.push("clean"), run: async () => dry.push("run"), log: (line) => dry.push(line) });
  assert.equal(dry.includes("clean"), false);
  assert.ok(dry.includes("run"));
  assert.ok(dry.some((line) => /dry-run: cargo-ndk arm64-v8a \(release\)/.test(line)));
  assert.throws(() => androidNdkCommand(ndk, { profile: "debug" }), /unsupported Android Cargo profile/);
});

test("an Android target carries ndk through validation, and an invalid ndk refuses the target", (t) => {
  const ws = workspace(t);
  const target = {
    signed: true, signingContract: ANDROID_SIGNING_CONTRACT,
    gradle: { cmd: "./gradlew", args: [":app:bundleRelease"], cwd: "android" },
    aab: { file: AAB_FILE }, artifacts: [AAB_FILE],
    signing: { keystore: { path: ws.keystore, alias: "upload" }, storePassword: { credential: "fixture-store" }, expectedCertSha256: FIXTURE_CERT_SHA256 },
  };
  assert.equal(validateAndroidTarget(target, { root: ws.root }).ndk, null);
  const valid = validateAndroidTarget({ ...target, ndk: { jniLibs: JNI } }, { root: ws.root });
  assert.deepEqual(valid.ndk.abis, ["arm64-v8a"]);
  assert.throws(() => validateAndroidTarget({ ...target, ndk: { jniLibs: JNI, abis: ["nope"] } }, { root: ws.root }), /ABIs from/);
});

test("the release lane builds the native libraries before Gradle and plans them in a dry run, with no secret read", async (t) => {
  const ws = workspace(t);
  const target = validateAndroidTarget({
    signed: true, signingContract: ANDROID_SIGNING_CONTRACT,
    gradle: { cmd: "./gradlew", args: [":app:bundleRelease"], cwd: "android" },
    ndk: { jniLibs: JNI, abis: ["arm64-v8a", "x86_64"] },
    aab: { file: AAB_FILE }, artifacts: [AAB_FILE],
    signing: { keystore: { path: ws.keystore, alias: "upload" }, storePassword: { credential: "fixture-store" }, expectedCertSha256: FIXTURE_CERT_SHA256 },
  }, { root: ws.root });
  const calls = [];
  const credentialStore = { read() { throw new Error("a dry run must not read a secret"); } };
  const result = await runAndroidRelease({
    root: ws.root, target, dryRun: true, env: {}, credentialStore, log: () => {},
    runGradle: async (command) => { calls.push(["gradle", command.cmd]); },
    runNdk: async (command, env) => { calls.push(["ndk", command.cmd, env]); },
  });
  assert.equal(result.dryRun, true);
  assert.deepEqual(calls.map((call) => call[0]), ["ndk", "gradle"]);
  assert.deepEqual(calls[0][2], {});
  const argv = JSON.stringify(calls);
  assert.ok(!/pass|secret/i.test(argv));
});

test("[GB-62] every ABI/profile/manifest form puts --locked at the inner Cargo boundary (right after `build`), keeping the outer ABI/API/output flags; the carried-argv rule agrees and never appends to the wrong parser", async (t) => {
  const { root } = workspace(t);
  const { lockedArgv } = await import("./snapshot-request.mjs");
  for (const kind of ["release", "dev"]) {
    for (const abis of [["arm64-v8a"], ["arm64-v8a", "x86_64"], ["armeabi-v7a", "x86"]]) {
      for (const extra of [{}, { manifestPath: "rust/Cargo.toml" }, { package: "heardright-android", features: ["a"] }, { apiLevel: 26, manifestPath: "rust/Cargo.toml", package: "p" }]) {
        const ndk = validateAndroidNdk({ abis, jniLibs: JNI, ...extra }, { root, kind });
        const { args } = androidNdkCommand(ndk, { profile: kind });
        const build = args.indexOf("build");
        assert.equal(args[build + 1], "--locked", `${kind} ${abis} ${JSON.stringify(extra)}: --locked is the first inner argument`);
        assert.equal(args.filter((a) => a === "--locked").length, 1);
        assert.deepEqual(args.slice(0, build).filter((a) => a === "-t").length, abis.length, "outer ABI flags intact");
        assert.ok(!args.slice(0, build).includes("--locked"), "never before `build`: cargo-ndk's own grammar");
        assert.deepEqual(lockedArgv(args), args, "an argv that already carries it is sent as it is (no second flag)");
      }
    }
  }
  // The carried-call rule for an argv WITHOUT the flag: inserted after the inner subcommand, after every outer option and its value.
  const unlocked = ["ndk", "--platform", "26", "-t", "arm64-v8a", "-o", "app/jniLibs", "build", "--release", "-p", "x"];
  assert.deepEqual(lockedArgv(unlocked), ["ndk", "--platform", "26", "-t", "arm64-v8a", "-o", "app/jniLibs", "build", "--locked", "--release", "-p", "x"], "-p after `ndk` is cargo-ndk's platform, not a package: the flag lands after `build`");
  assert.deepEqual(lockedArgv(["ndk", "-t", "arm64-v8a", "build", "--", "--locked"]), ["ndk", "-t", "arm64-v8a", "build", "--locked", "--", "--locked"], "a --locked after `--` is the program's, not Cargo's");
  assert.deepEqual(lockedArgv(["ndk", "-t", "arm64-v8a", "build", "--frozen"]), ["ndk", "-t", "arm64-v8a", "build", "--frozen"]);
  assert.deepEqual(lockedArgv(["ndk", "-t", "arm64-v8a", "--no-strip", "check"]), ["ndk", "-t", "arm64-v8a", "--no-strip", "check", "--locked"]);
  assert.deepEqual(lockedArgv(["ndk", "-t", "arm64-v8a"]), ["ndk", "-t", "arm64-v8a"], "no inner command: nothing to lock");
  assert.deepEqual(lockedArgv(["ndk", "-t", "arm64-v8a", "metadata"]), ["ndk", "-t", "arm64-v8a", "metadata"], "an inner command that compiles nothing is left alone");
  assert.deepEqual(lockedArgv(["build", "-p", "x"]), ["build", "--locked", "-p", "x"], "plain cargo is unchanged");
});
