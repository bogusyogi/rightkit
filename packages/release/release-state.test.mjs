import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  cacheFingerprint,
  commandOutputPortable,
  watchProgress,
  resolveReleaseLayout,
  releaseEnvironment,
  runBuildStateMachine,
  runUploadStateMachine,
  verifySealedRelease,
} from "./release-state.mjs";

test("progress watcher observes writes in nested Cargo target directories", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-watch-"));
  const nested = path.join(root, "release", "build", "openssl");
  mkdirSync(nested, { recursive: true });
  let resolveProgress;
  const progress = new Promise((resolve) => { resolveProgress = resolve; });
  const close = watchProgress([root], resolveProgress);
  try {
    await writeFile(path.join(nested, "object.lib"), "progress");
    await Promise.race([
      progress,
      new Promise((_, reject) => setTimeout(() => reject(new Error("nested progress event missing")), 2_000)),
    ]);
  } finally {
    close();
  }
});

test("portable command capture resolves Windows command shims", { skip: process.platform !== "win32" }, () => {
  assert.match(commandOutputPortable("pnpm", ["--version"]), /^11\.12\.0$/);
});

test("nested app configs keep the vault at repo root and build from the app root", () => {
  const layout = resolveReleaseLayout({
    repoRoot: "D:/suite/heardright",
    configPath: "D:/suite/heardright/tauri-app-next/right-release.config.mjs",
  });
  assert.equal(layout.vaultRoot, path.resolve("D:/suite/heardright/.right-release"));
  assert.equal(layout.appRoot, path.resolve("D:/suite/heardright/tauri-app-next"));
  assert.equal(layout.targetLink, path.resolve("D:/suite/heardright/tauri-app-next/src-tauri/target"));
});

test("shared release environment is isolated from the app vault", () => {
  const env = releaseEnvironment({
    root: "/suite/.right-release",
    cacheRoot: "/Users/test/Library/Caches/RightSuite/release",
    platform: "mac",
    architecture: "aarch64",
    app: "fixture",
    appRoot: "/suite/fixture",
    cacheKey: "abcdef0123456789",
    mode: "shared",
  });
  assert.match(env.CARGO_TARGET_DIR, /RightSuite\/release\/targets\/mac\/fixture\/abcdef0123456789$/);
  assert.match(env.CARGO_HOME, /RightSuite\/release\/cargo-home$/);
  assert.equal(env.RIGHT_RELEASE_CACHE_OWNER, "rightkit-v2");
});

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-state-"));
  const sealedDir = path.join(root, ".right-release", "sealed", "fixture-1.2.3-abcdef12", "windows");
  mkdirSync(sealedDir, { recursive: true });
  const installer = path.join(sealedDir, "Fixture_x64-setup.exe");
  const signature = `${installer}.sig`;
  writeFileSync(installer, "signed-installer");
  writeFileSync(signature, "updater-signature");
  const manifest = {
    schema: 1,
    releaseId: "fixture-1.2.3-abcdef12",
    app: "fixture",
    version: "1.2.3",
    commit: "abcdef1234567890",
    platform: "win",
    files: [
      { role: "installer", name: path.basename(installer), sha256: sha256("signed-installer"), sizeBytes: 16 },
      { role: "updater-signature", name: path.basename(signature), sha256: sha256("updater-signature"), sizeBytes: 17 },
    ],
    routes: {
      patch: [{ role: "installer", bucket: "public", key: "fixture/installers/windows/current/Fixture_x64-setup.exe" }],
      update: [{ role: "installer", bucket: "private", key: "fixture/updates/windows/current/Fixture_x64-setup.exe" }],
    },
    checkpoints: ["preflight_complete", "build_complete", "signed", "hardened", "sealed"],
  };
  writeFileSync(path.join(sealedDir, "release-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  return { root, sealedDir, installer, signature, manifest };
}

test("upload has no build operation and restores the previous stable object when registration fails", async () => {
  const fx = fixture();
  const calls = [];
  await assert.rejects(
    runUploadStateMachine({
      root: fx.root,
      releaseId: fx.manifest.releaseId,
      platform: "win",
      tier: "patch",
      ops: {
        verifyAuthenticode: async () => calls.push("verify-authenticode"),
        harden: async () => calls.push("harden"),
        backup: async () => calls.push("backup"),
        upload: async () => calls.push("upload"),
        register: async () => { calls.push("register"); throw new Error("R2/API unavailable"); },
        verifyRemote: async () => calls.push("verify-remote"),
        restore: async () => calls.push("restore"),
        discardBackup: async () => calls.push("discard-backup"),
      },
    }),
    /R2\/API unavailable/,
  );
  assert.deepEqual(calls, ["verify-authenticode", "harden", "backup", "upload", "register", "restore"]);
});
test("missing runtime input fails before build preparation and cannot touch an existing sealed installer", async () => {
  const fx = fixture();
  let prepared = false;
  await assert.rejects(
    runBuildStateMachine({
      root: fx.root,
      app: "fixture",
      version: "1.2.4",
      commit: "1234567890abcdef",
      platform: "win",
      requiredInputs: [path.join(fx.root, "missing-directml.dll")],
      ops: {
        preflight: async ({ requiredInputs }) => {
          for (const file of requiredInputs) readFileSync(file);
        },
        prepare: async () => { prepared = true; },
        build: async () => {},
        sign: async () => {},
        harden: async () => {},
        seal: async () => {},
      },
    }),
    /ENOENT/,
  );
  assert.equal(prepared, false);
  assert.equal(readFileSync(fx.installer, "utf8"), "signed-installer");
});

test("an interrupted build leaves every previously sealed release untouched", async () => {
  const fx = fixture();
  await assert.rejects(
    runBuildStateMachine({
      root: fx.root,
      app: "fixture",
      version: "1.2.4",
      commit: "1234567890abcdef",
      platform: "win",
      requiredInputs: [],
      ops: {
        preflight: async () => {},
        prepare: async () => {},
        build: async () => { throw new Error("SIGINT"); },
        sign: async () => {},
        harden: async () => {},
        seal: async () => {},
      },
    }),
    /SIGINT/,
  );
  assert.equal(readFileSync(fx.installer, "utf8"), "signed-installer");
});

test("a valid sealed release resumes without rebuilding", async () => {
  const fx = fixture();
  let builds = 0;
  const result = await runBuildStateMachine({
    root: fx.root,
    app: "fixture",
    version: fx.manifest.version,
    commit: fx.manifest.commit,
    platform: "win",
    requiredInputs: [],
    ops: {
      preflight: async () => {},
      prepare: async () => {},
      build: async () => { builds += 1; },
      sign: async () => {},
      harden: async () => {},
      seal: async () => {},
    },
  });
  assert.equal(result.status, "sealed");
  assert.equal(result.resumed, true);
  assert.equal(builds, 0);
});

test("sealed build resume rejects changed input or cache identity", async () => {
  const fx = fixture();
  fx.manifest.inputs = { "src-tauri/Cargo.lock": "old-hash" };
  fx.manifest.cacheKey = "old-cache";
  writeFileSync(path.join(fx.sealedDir, "release-manifest.json"), `${JSON.stringify(fx.manifest, null, 2)}\n`);

  await assert.rejects(
    runBuildStateMachine({
      root: fx.root,
      app: fx.manifest.app,
      version: fx.manifest.version,
      commit: fx.manifest.commit,
      platform: fx.manifest.platform,
      inputHashes: { "src-tauri/Cargo.lock": "new-hash" },
      cacheKey: fx.manifest.cacheKey,
      ops: {},
    }),
    /sealed release input identity mismatch/i,
  );
  await assert.rejects(
    runBuildStateMachine({
      root: fx.root,
      app: fx.manifest.app,
      version: fx.manifest.version,
      commit: fx.manifest.commit,
      platform: fx.manifest.platform,
      inputHashes: fx.manifest.inputs,
      cacheKey: "new-cache",
      ops: {},
    }),
    /sealed release cache identity mismatch/i,
  );
});

test("manifest or file tampering blocks upload before any mutation", async () => {
  const fx = fixture();
  writeFileSync(fx.installer, "changed");
  assert.throws(() => verifySealedRelease(fx.sealedDir), /hash mismatch/);
  let uploaded = false;
  await assert.rejects(
    runUploadStateMachine({
      root: fx.root,
      releaseId: fx.manifest.releaseId,
      platform: "win",
      tier: "patch",
      ops: {
        upload: async () => { uploaded = true; },
      },
    }),
    /hash mismatch/,
  );
  assert.equal(uploaded, false);
});

test("tier or non-current route mismatch fails before R2 backup", async () => {
  const fx = fixture();
  fx.manifest.routes.patch[0].bucket = "private";
  fx.manifest.routes.patch[0].key = "fixture/updates/windows/1.2.3/Fixture.exe";
  writeFileSync(path.join(fx.sealedDir, "release-manifest.json"), `${JSON.stringify(fx.manifest, null, 2)}\n`);
  let backedUp = false;
  await assert.rejects(
    runUploadStateMachine({
      root: fx.root,
      releaseId: fx.manifest.releaseId,
      platform: "win",
      tier: "patch",
      ops: { backup: async () => { backedUp = true; } },
    }),
    /patch route.*public.*installers.*current/i,
  );
  assert.equal(backedUp, false);
});

test("release ids and R2 keys reject traversal and shell metacharacters", async () => {
  const fx = fixture();
  await assert.rejects(
    runUploadStateMachine({ root: fx.root, releaseId: "../../outside", platform: "win", tier: "patch" }),
    /invalid release id/i,
  );
  fx.manifest.routes.patch[0].key = "fixture/installers/windows/current/Fixture.exe&whoami";
  writeFileSync(path.join(fx.sealedDir, "release-manifest.json"), `${JSON.stringify(fx.manifest, null, 2)}\n`);
  await assert.rejects(
    runUploadStateMachine({ root: fx.root, releaseId: fx.manifest.releaseId, platform: "win", tier: "patch" }),
    /unsafe R2 key/i,
  );
});

test("release and test Cargo targets are disjoint", () => {
  const root = "C:/repo/.right-release";
  const build = releaseEnvironment({ root, platform: "win", cacheKey: "deps123", kind: "release" });
  const testEnv = releaseEnvironment({ root, platform: "win", cacheKey: "deps123", kind: "test" });
  assert.notEqual(build.CARGO_TARGET_DIR, testEnv.CARGO_TARGET_DIR);
  assert.match(build.CARGO_TARGET_DIR, /cache[\\/]cargo-target[\\/]win[\\/]deps123/);
  assert.match(testEnv.CARGO_TARGET_DIR, /cache[\\/]test-target[\\/]win[\\/]deps123/);
  assert.match(build.SCCACHE_DIR, /cache[\\/]sccache/);
  assert.equal(build.RUSTC_WRAPPER, "sccache");
});

test("dependency cache survives source commits and invalidates only dependency/toolchain inputs", () => {
  const base = {
    cargoLockSha256: "lock-a",
    rustc: "rustc 1.90.0 host x86_64-pc-windows-msvc",
    target: "x86_64-pc-windows-msvc",
    features: ["bundled-sqlcipher-vendored-openssl"],
  };
  assert.equal(cacheFingerprint({ ...base, commit: "aaa" }), cacheFingerprint({ ...base, commit: "bbb" }));
  assert.notEqual(cacheFingerprint(base), cacheFingerprint({ ...base, cargoLockSha256: "lock-b" }));
  assert.notEqual(cacheFingerprint(base), cacheFingerprint({ ...base, rustc: "rustc 1.91.0" }));
  assert.notEqual(cacheFingerprint(base), cacheFingerprint({ ...base, features: ["bundled-sqlcipher"] }));
});
