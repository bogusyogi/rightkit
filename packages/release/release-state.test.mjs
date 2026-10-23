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
import { registryEvidenceFixture } from "./test-support/admission.mjs";

const expectedPnpmVersion = JSON.parse(readFileSync(new URL("./rightkit-versions.json", import.meta.url), "utf8")).packageManager.replace(/^pnpm@/, "");

test("progress watcher observes writes in nested Cargo target directories", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-watch-"));
  const nested = path.join(root, "release", "build", "openssl");
  mkdirSync(nested, { recursive: true });
  const readyFile = path.join(root, "watch-ready");
  const nestedFile = path.join(nested, "object.lib");
  let resolveReady;
  let resolveProgress;
  const ready = new Promise((resolve) => { resolveReady = resolve; });
  const progress = new Promise((resolve) => { resolveProgress = resolve; });
  const close = watchProgress([root], (_eventType, filename) => {
    const eventName = String(filename ?? "");
    if (eventName === path.basename(readyFile)) resolveReady();
    if (eventName.endsWith(path.basename(nestedFile))) resolveProgress();
  });
  try {
    await new Promise((resolve) => setTimeout(resolve, 50));
    await writeFile(readyFile, "ready");
    await waitForEvent(ready, "watcher readiness event missing");
    await writeFile(nestedFile, "progress");
    await waitForEvent(progress, "nested progress event missing");
  } finally {
    close();
  }
});

test("fallback progress watchers cap open handles when recursive watching is unavailable", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-watch-cap-"));
  for (let index = 0; index < 10; index += 1) mkdirSync(path.join(root, `dir-${index}`));
  const opened = [];
  const close = watchProgress([root], () => {}, {
    maxFallbackWatchers: 3,
    watchFactory: (directory, options) => {
      if (options?.recursive) throw new Error("recursive watch unsupported");
      opened.push(directory);
      return { close: () => {} };
    },
  });
  try {
    assert.equal(opened.length, 3);
  } finally {
    close();
  }
});

async function waitForEvent(event, message) {
  let timer;
  try {
    await Promise.race([
      event,
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), 5_000); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

test("portable command capture resolves Windows command shims", { skip: process.platform !== "win32" }, () => {
  assert.equal(commandOutputPortable("pnpm", ["--version"]), expectedPnpmVersion);
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

test("pre-seal hook runs after hardening and before immutable sealing", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-pre-seal-"));
  const calls = [];
  const result = await runBuildStateMachine({
    root,
    app: "fixture",
    version: "1.0.0",
    commit: "abcdef1234567890",
    platform: "mac",
    ops: {
      preflight: async () => calls.push("preflight"),
      prepare: async () => calls.push("prepare"),
      build: async () => calls.push("build"),
      sign: async () => calls.push("sign"),
      harden: async () => calls.push("harden"),
      preSeal: async ({ sealedDir }) => {
        calls.push(`pre-seal:${path.basename(sealedDir)}`);
      },
      seal: async () => calls.push("seal"),
    },
  });
  assert.equal(result.status, "sealed");
  assert.deepEqual(calls, ["preflight", "prepare", "build", "sign", "harden", "pre-seal:mac", "seal"]);
  assert.equal(calls.at(-2), "pre-seal:mac");
  assert.equal(calls.at(-3), "harden");
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
  assert.match(env.CARGO_TARGET_DIR, /RightSuite[\\/]release[\\/]targets[\\/]mac[\\/]fixture[\\/]abcdef0123456789$/);
  assert.match(env.CARGO_HOME, /RightSuite[\\/]release[\\/]cargo-home$/);
  assert.equal(env.RIGHT_RELEASE_CACHE_OWNER, "rightkit-v2");
  assert.equal(env.SCCACHE_CACHE_SIZE, "32G");
  // A dropped sccache server connection must fall through to plain rustc
  // instead of failing the build; seen on Windows as a bare error 10054.
  assert.equal(env.SCCACHE_IGNORE_SERVER_IO_ERROR, "1");
});

test("shared release environment honors the cache policy override", () => {
  const env = releaseEnvironment({
    root: "/suite/.right-release",
    cacheRoot: "/cache",
    platform: "mac",
    architecture: "aarch64",
    app: "fixture",
    appRoot: "/suite/fixture",
    cacheKey: "abcdef0123456789",
    mode: "shared",
    env: { RIGHT_RELEASE_SCCACHE_MAX_BYTES: String(48 * 1024 ** 3) },
  });
  assert.equal(env.SCCACHE_CACHE_SIZE, "48G");
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
    ...registryEvidenceFixture(),
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

test("a version already sealed from another commit cannot be rebuilt", async () => {
  const fx = fixture();
  let prepared = false;
  await assert.rejects(
    runBuildStateMachine({
      root: fx.root,
      app: "fixture",
      version: fx.manifest.version,
      commit: "1234567890abcdef",
      platform: "win",
      requiredInputs: [],
      ops: {
        preflight: async () => {},
        prepare: async () => { prepared = true; },
        build: async () => {},
        sign: async () => {},
        harden: async () => {},
        seal: async () => {},
      },
    }),
    /version 1\.2\.3 is already sealed from commit abcdef1234567890/,
  );
  assert.equal(prepared, false);
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

// ----- RightKit registry evidence: absence is refused on every seal/upload path; there is no global escape.
function rewriteManifest(fx, change) {
  const file = path.join(fx.sealedDir, "release-manifest.json");
  const manifest = JSON.parse(readFileSync(file, "utf8"));
  writeFileSync(file, `${JSON.stringify(change(manifest), null, 2)}\n`);
}
const stripRegistry = ({ rightkitRegistryEvidence, rightkitRegistry, ...manifest }) => manifest;
const stripEverything = ({ rightReleaseVersion, ...manifest }) => stripRegistry(manifest);

test("verifySealedRelease accepts a new seal with valid versioned evidence and refuses a deleted marker, a deleted block and a tampered snapshot", () => {
  const fx = fixture();
  assert.equal(verifySealedRelease(fx.sealedDir).manifest.rightkitRegistryEvidence, 1);
  rewriteManifest(fx, ({ rightkitRegistryEvidence, ...m }) => m);
  assert.throws(() => verifySealedRelease(fx.sealedDir), /marker must be 1, got <missing>/);
  rewriteManifest(fx, (m) => ({ ...m, rightkitRegistryEvidence: 1, rightkitRegistry: undefined }));
  assert.throws(() => verifySealedRelease(fx.sealedDir), /evidence is missing/);
  rewriteManifest(fx, (m) => ({ ...m, rightkitRegistryEvidence: 1, rightkitRegistry: { ...evidenceFixtureBlock(), snapshot: { ...evidenceFixtureBlock().snapshot, observedAt: "2020-01-01T00:00:00.000Z" } } }));
  assert.throws(() => verifySealedRelease(fx.sealedDir), /digest/);
});
const evidenceFixtureBlock = () => registryEvidenceFixture().rightkitRegistry;

test("a stripped NEW seal is refused even with the legacy release acknowledged; the upload and build resume paths agree", async () => {
  const fx = fixture(); // carries rightReleaseVersion 0.6.0 and evidence
  rewriteManifest(fx, stripRegistry);
  const named = [fx.manifest.releaseId];
  assert.throws(() => verifySealedRelease(fx.sealedDir), /produced by @rightkit\/release 0\.6\.0 but carries no RightKit registry evidence/);
  assert.throws(() => verifySealedRelease(fx.sealedDir, { legacyReleaseIds: named }), /a stripped new seal is never a legacy seal/);
  const previous = process.env.RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES;
  process.env.RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES = named.join(",");
  try {
    await assert.rejects(runUploadStateMachine({ root: fx.root, releaseId: fx.manifest.releaseId, platform: "win", tier: "patch", ops: {} }), /stripped new seal is never a legacy seal/);
    await assert.rejects(runBuildStateMachine({ root: fx.root, app: "fixture", version: "1.2.3", commit: fx.manifest.commit, platform: "win", requiredInputs: [], ops: {} }), /stripped new seal is never a legacy seal/);
  } finally {
    if (previous === undefined) delete process.env.RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES; else process.env.RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES = previous;
  }
});

test("only an explicitly identified pre-0.6.0 release is exempt: no recorded tool version and its release id named; a lower recorded version too", () => {
  const fx = fixture();
  rewriteManifest(fx, stripEverything);
  assert.throws(() => verifySealedRelease(fx.sealedDir), /only a pre-0\.6\.0 release named in RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES is exempt/);
  assert.throws(() => verifySealedRelease(fx.sealedDir, { legacyReleaseIds: ["another-release"] }), /is exempt/);
  assert.equal(verifySealedRelease(fx.sealedDir, { legacyReleaseIds: [fx.manifest.releaseId] }).manifest.app, "fixture");
  rewriteManifest(fx, (m) => ({ ...m, rightReleaseVersion: "0.5.1" }));
  assert.equal(verifySealedRelease(fx.sealedDir, { legacyReleaseIds: [fx.manifest.releaseId] }).manifest.app, "fixture");
  assert.throws(() => verifySealedRelease(fx.sealedDir), /is exempt/);
});
