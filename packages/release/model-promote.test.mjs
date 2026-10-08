import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  RUNTIME_ARTIFACT_TRUSTED_KEY_ID,
  RuntimeArtifactPromotionError,
  defaultWindowsRuntimeArtifactSigningKeyFile,
  loadRuntimeArtifactSigningKey,
  promoteRuntimeArtifact,
  runtimeArtifactUploadEnv,
} from "./model-promote.mjs";
import { RIGHT_SUITE_APP_KEYS, verifyRuntimeArtifactEnvelope } from "./runtime-artifact-manifest.mjs";
const { privateKey, publicKey } = generateKeyPairSync("ed25519");
const artifactDigest = "a".repeat(64);
const evidenceDigest = "b".repeat(64);
const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const candidateCommit = "a".repeat(40);

// Source identity as build/upload/promote read it: the exact commit, plus the
// porcelain status that must be empty for the promotion to run.
function verificationFixture(commit = candidateCommit, status = "") {
  return { candidateCommit: commit, status };
}

function request(overrides = {}) {
  return {
    authority: "heardright",
    privateKey,
    keyId: RUNTIME_ARTIFACT_TRUSTED_KEY_ID,
    artifactFile: "C:/fixtures/parakeet.onnx",
    evidenceFile: "C:/fixtures/eval.json",
    config: {
      artifact: {
        filename: "parakeet.onnx",
        sha256: artifactDigest,
        sizeBytes: 42,
      },
      evidence: { sha256: evidenceDigest },
      manifest: {
        artifactKind: "asr-model",
        entitlement: { appKey: "heardright", tier: "pro" },
        distribution: { delivery: "private-r2", bucket: "rightapps-updates" },
        target: { os: "windows", arch: "x86_64" },
        versions: {
          runtime: "rightkit-asr-0.1.0",
          model: "parakeet-tdt-0.6b-v3",
          tokenizer: "sentencepiece-2026-07-14",
          preprocessing: "rightkit-asr-0.1.0",
          license: "cc-by-4.0",
          provenance: "heardright-approved-2026-07-14",
        },
        provenance: {
          source: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
          sourceRevision: "0123456789abcdef",
          licenseId: "CC-BY-4.0",
          noticeSha256: "c".repeat(64),
        },
        promotion: {
          promotionId: "hr-asr-2026-07-14-001",
          promotedAt: "2026-07-14T12:00:00.000Z",
        },
      },
    },
    verification: verificationFixture(),
    ...overrides,
  };
}

function boundaries(events) {
  return {
    metadata: async (file) => file.endsWith("eval.json")
      ? { sha256: evidenceDigest, sizeBytes: 10 }
      : { sha256: artifactDigest, sizeBytes: 42 },
    upload: async ({ purpose }) => { events.push(`upload:${purpose}`); },
    register: async () => { events.push("register"); },
  };
}

test("uploads immutable object, registers signed envelope, then replaces stable pointer", async () => {
  const events = [];
  const result = await promoteRuntimeArtifact(request(), boundaries(events));

  assert.deepEqual(events, ["upload:object", "register", "upload:pointer"]);
  assert.equal(result.envelope.signature.keyId, RUNTIME_ARTIFACT_TRUSTED_KEY_ID);
  assert.equal(result.envelope.manifest.object.sha256, artifactDigest);
  assert.equal(result.envelope.manifest.promotion.evidenceSha256, evidenceDigest);
  assert.equal(result.envelope.manifest.entitlement.appKey, "heardright");
  assert.equal(result.envelope.manifest.promotion.authorityAppKey, "heardright");
  assert.match(result.envelope.manifest.pointerKey, /\/current\/manifest\.json$/);
});

test("dry-run validates and signs without invoking process or network boundaries", async () => {
  const events = [];
  const result = await promoteRuntimeArtifact(request({ dryRun: true }), boundaries(events));
  assert.deepEqual(events, []);
  assert.deepEqual(result.steps, ["upload immutable object", "register signed envelope", "replace stable pointer"]);
  assert.equal(result.envelope.signature.algorithm, "Ed25519");
});

test("every registered app can dry-run promotion of its own entitled artifacts", async () => {
  for (const appKey of RIGHT_SUITE_APP_KEYS) {
    const input = request({ authority: appKey, dryRun: true });
    input.config.manifest.entitlement.appKey = appKey;
    if (appKey === "genright") input.config.manifest.artifactKind = "tts-model";
    const events = [];
    const result = await promoteRuntimeArtifact(input, boundaries(events));
    const manifest = verifyRuntimeArtifactEnvelope(result.envelope, publicKey);
    assert.equal(manifest.entitlement.appKey, appKey);
    assert.equal(manifest.promotion.authorityAppKey, appKey);
    assert.ok(manifest.pointerKey.startsWith(`${appKey}/runtime-artifacts/`));
    assert.deepEqual(events, []);
  }
});

test("unknown authority or entitlement app and authority mismatch have typed errors before file access", async () => {
  for (const [authority, appKey, code] of [
    ["unknown", "heardright", "RUNTIME_ARTIFACT_UNKNOWN_APP"],
    ["heardright", "unknown", "RUNTIME_ARTIFACT_UNKNOWN_APP"],
    ["genright", "heardright", "RUNTIME_ARTIFACT_AUTHORITY_MISMATCH"],
    ["heardright", "genright", "RUNTIME_ARTIFACT_AUTHORITY_MISMATCH"],
  ]) {
    const input = request({ authority, dryRun: true });
    input.config.manifest.entitlement.appKey = appKey;
    await assert.rejects(promoteRuntimeArtifact(input, {
      metadata: async () => assert.fail("invalid authority must fail before file access"),
      upload: async () => assert.fail("unexpected upload"),
      register: async () => assert.fail("unexpected registration"),
    }), (error) => error instanceof RuntimeArtifactPromotionError && error.code === code);
  }
});

test("model promotion refuses a dirty working tree", async () => {
  const events = [];
  const verification = verificationFixture(candidateCommit, " M packages/release/model-promote.mjs\0");
  await assert.rejects(
    promoteRuntimeArtifact(request({ verification, dryRun: true }), boundaries(events)),
    /clean working tree/,
  );
  assert.deepEqual(events, []);
});

test("model promotion requires source identity", async () => {
  const events = [];
  await assert.rejects(
    promoteRuntimeArtifact(request({ verification: undefined, dryRun: true }), boundaries(events)),
    /source identity is required/,
  );
  assert.deepEqual(events, []);
});

test("never registers or publishes a pointer after immutable upload failure", async () => {
  const events = [];
  const deps = boundaries(events);
  deps.upload = async ({ purpose }) => {
    events.push(`upload:${purpose}`);
    throw new Error("R2 failed");
  };
  await assert.rejects(promoteRuntimeArtifact(request(), deps), /R2 failed/);
  assert.deepEqual(events, ["upload:object"]);
});

test("never publishes a pointer when RightApps registration fails", async () => {
  const events = [];
  const deps = boundaries(events);
  deps.register = async () => {
    events.push("register");
    throw new Error("registration failed");
  };
  await assert.rejects(promoteRuntimeArtifact(request(), deps), /registration failed/);
  assert.deepEqual(events, ["upload:object", "register"]);
});

test("fails before mutation on digest, authority, key-id, or lane contradiction", async () => {
  const badRequests = [
    request({ authority: "scraperight" }),
    request({ keyId: "untrusted-key" }),
    request({ config: { ...request().config, artifact: { ...request().config.artifact, sha256: "d".repeat(64) } } }),
    request({ config: {
      ...request().config,
      manifest: {
        ...request().config.manifest,
        entitlement: { appKey: "scraperight", tier: "public" },
        distribution: { delivery: "public-r2", bucket: "rightapps-downloads" },
      },
    } }),
    request({ config: {
      ...request().config,
      manifest: {
        ...request().config.manifest,
        promotion: {
          ...request().config.manifest.promotion,
          authorityAppKey: "scraperight",
        },
      },
    } }),
  ];
  for (const input of badRequests) {
    const events = [];
    await assert.rejects(promoteRuntimeArtifact(input, boundaries(events)));
    assert.deepEqual(events, []);
  }
});

test("right-release routes model promote to the portable promotion lane", () => {
  const result = spawnSync(process.execPath, [path.join(packageRoot, "cli/right-release.mjs"), "model", "promote", "--authority", "heardright"], {
    cwd: packageRoot,
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /model promote requires --config/);
  assert.doesNotMatch(result.stderr, /unknown command/);
});

test("model promote help documents the portable config and secure key inputs", () => {
  const result = spawnSync(process.execPath, [path.join(packageRoot, "cli/right-release.mjs"), "model", "promote", "--help"], {
    cwd: packageRoot,
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /artifact\.file/);
  assert.match(result.stdout, /evidence\.sha256/);
  assert.match(result.stdout, /RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE/);
  assert.match(result.stdout, /RIGHTAPPS_RELEASE_TOKEN/);
  assert.match(result.stdout, /--authority <app-key>/);
  assert.match(result.stdout, /genright/);
  assert.match(result.stdout, /must equal manifest\.entitlement\.appKey/);
});

test("portable CLI promotes from a clean checkout, refuses a dirty one, and rejects injected trust flags", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "rightkit-model-promote-test-"));
  try {
    const artifact = Buffer.from("tiny model fixture");
    const evidence = Buffer.from('{"certified":true}');
    const testKeys = generateKeyPairSync("ed25519");
    const keyPem = testKeys.privateKey.export({ type: "pkcs8", format: "pem" }).toString();
    await Promise.all([
      writeFile(path.join(root, "model.onnx"), artifact),
      writeFile(path.join(root, "evidence.json"), evidence),
      writeFile(path.join(root, "key.pem"), keyPem, { mode: 0o600 }),
    ]);
    spawnSync("git", ["init"], { cwd: root, encoding: "utf8" });
    spawnSync("git", ["add", "."], { cwd: root, encoding: "utf8" });
    const committed = spawnSync("git", ["-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-m", "fixture"], { cwd: root, encoding: "utf8" });
    assert.equal(committed.status, 0, committed.stderr);
    const config = {
      artifact: {
        file: "model.onnx",
        filename: "model.onnx",
        sha256: createHash("sha256").update(artifact).digest("hex"),
        sizeBytes: artifact.length,
      },
      evidence: {
        file: "evidence.json",
        sha256: createHash("sha256").update(evidence).digest("hex"),
      },
      manifest: {
        ...request().config.manifest,
        artifactKind: "tts-model",
        entitlement: { appKey: "genright", tier: "pro" },
      },
    };
    await writeFile(path.join(root, "promotion.json"), JSON.stringify(config));
    spawnSync("git", ["add", "."], { cwd: root, encoding: "utf8" });
    spawnSync("git", ["-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-m", "config"], { cwd: root, encoding: "utf8" });

    const promoteArgs = [
      path.join(packageRoot, "cli/right-release.mjs"), "model", "promote",
      "--authority", "genright",
      "--config", path.join(root, "promotion.json"),
      "--signing-key-file", path.join(root, "key.pem"),
      "--dry-run",
    ];
    const result = spawnSync(process.execPath, promoteArgs, { cwd: root, encoding: "utf8", windowsHide: true });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /\[dry-run\] runtime artifact promotion validated/);
    assert.match(result.stdout, /app\/kind: genright\/tts-model/);
    assert.match(result.stdout, /pointer: genright\/runtime-artifacts\/tts-model\/windows\/x86_64\/current\/manifest\.json/);

    for (const [authority, code] of [
      ["unknown", "RUNTIME_ARTIFACT_UNKNOWN_APP"],
      ["heardright", "RUNTIME_ARTIFACT_AUTHORITY_MISMATCH"],
    ]) {
      const invalidArgs = [...promoteArgs];
      invalidArgs[invalidArgs.indexOf("--authority") + 1] = authority;
      invalidArgs[invalidArgs.indexOf("--signing-key-file") + 1] = path.join(root, "missing-key.pem");
      const invalid = spawnSync(process.execPath, invalidArgs, { cwd: root, encoding: "utf8", windowsHide: true });
      assert.equal(invalid.status, 1);
      assert.ok(invalid.stderr.includes(`[${code}]`), invalid.stderr);
      assert.doesNotMatch(invalid.stderr, /ENOENT|BEGIN PRIVATE KEY/);
    }

    await writeFile(path.join(root, "untracked.txt"), "dirty");
    const dirty = spawnSync(process.execPath, promoteArgs, { cwd: root, encoding: "utf8", windowsHide: true });
    assert.equal(dirty.status, 1);
    assert.match(dirty.stderr, /clean working tree/);

    const attacker = spawnSync(process.execPath, [
      path.join(packageRoot, "cli/right-release.mjs"), "model", "promote",
      "--authority", "heardright", "--config", path.join(root, "promotion.json"),
      "--verification-public-key", path.join(root, "verification-public.pem"),
    ], { cwd: root, encoding: "utf8", windowsHide: true });
    assert.equal(attacker.status, 1);
    assert.match(attacker.stderr, /unknown model promote argument: --verification-public-key/);
    assert.doesNotMatch(`${result.stdout}${result.stderr}`, /BEGIN PRIVATE KEY/);
    assert.doesNotMatch(`${result.stdout}${result.stderr}`, /Uploading|POST https:/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("uses the canonical Windows signing-key path and supports a Keychain-fed PEM environment value", async () => {
  assert.equal(
    defaultWindowsRuntimeArtifactSigningKeyFile({ APPDATA: "C:/Users/test/AppData/Roaming" }),
    path.join("C:/Users/test/AppData/Roaming", "RightKit", "runtime-artifact-signing-key.pem"),
  );
  assert.equal(
    await loadRuntimeArtifactSigningKey({ env: { RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY: "line1\\nline2" }, platform: "darwin" }),
    "line1\nline2",
  );
});

test("non-Windows signing requires an explicit protected key input", async () => {
  await assert.rejects(loadRuntimeArtifactSigningKey({ env: {}, platform: "darwin" }), /RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE is required/);
});

test("does not forward signing or RightApps credentials into the Wrangler uploader process", () => {
  const env = runtimeArtifactUploadEnv({
    CLOUDFLARE_API_TOKEN: "cloudflare-token",
    RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY: "private-pem",
    RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE: "key.pem",
    RIGHTAPPS_RELEASE_TOKEN: "release-token",
    RIGHTAPPS_RELEASE_TOKEN_FILE: "release.token",
  });
  assert.equal(env.CLOUDFLARE_API_TOKEN, "cloudflare-token");
  assert.equal(env.RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY, undefined);
  assert.equal(env.RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE, undefined);
  assert.equal(env.RIGHTAPPS_RELEASE_TOKEN, undefined);
  assert.equal(env.RIGHTAPPS_RELEASE_TOKEN_FILE, undefined);
});
