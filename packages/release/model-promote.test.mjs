import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  RUNTIME_ARTIFACT_TRUSTED_KEY_ID,
  defaultWindowsRuntimeArtifactSigningKeyFile,
  loadRuntimeArtifactSigningKey,
  promoteRuntimeArtifact,
  runtimeArtifactUploadEnv,
} from "./model-promote.mjs";
import { canonicalJson, documentDigest } from "./verification-ticket.mjs";

const { privateKey } = generateKeyPairSync("ed25519");
const artifactDigest = "a".repeat(64);
const evidenceDigest = "b".repeat(64);
const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const candidateCommit = "a".repeat(40);
const candidateTree = "b".repeat(40);
const verificationKeys = generateKeyPairSync("ed25519");
const ticketDigest = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const canonicalDigest = (value) => ticketDigest(canonicalJson(value));
const fixedAdminTrust = {
  ticketKeys: { fixture: verificationKeys.publicKey, "bounded-admin-signer": verificationKeys.publicKey },
  hosts: {
    "mac-host": { platform: "mac", attestation_sha256: ticketDigest("mac"), receipt_key_id: "fixture", receipt_key_path: "/host/mac-receipt.pem", manifest_key_id: "fixture", manifest_key_path: "/host/mac-manifest.pem" },
    "windows-host": { platform: "windows", attestation_sha256: ticketDigest("windows"), receipt_key_id: "fixture", receipt_key_path: "/host/windows-receipt.pem", manifest_key_id: "fixture", manifest_key_path: "/host/windows-manifest.pem" },
  },
  hostKeys: { fixture: verificationKeys.publicKey },
};
function signVerification(value) { const signed = structuredClone(value); const payload = structuredClone(signed); delete payload.signing.signature; signed.signing.signature = sign(null, Buffer.from(canonicalJson(payload), "utf8"), verificationKeys.privateKey).toString("base64url"); return signed; }
function verificationFixture(commit = candidateCommit, tree = candidateTree) {
  const ticket = signVerification({ schema: "verification-ticket.v1", authority_id: "r7", request_envelope_sha256: ticketDigest("envelope"), request_sha256: ticketDigest("request"), manifest_sha256: ticketDigest("manifest"), experiment_id: "fixture", comparison_group_id: "fixture-group", nonce: "fixture-nonce-0001", issued_at: "2026-08-01T00:00:00.000Z", expires_at: "2026-09-01T00:00:00.000Z", candidate: { commit, tree, overlay_sha256: ticketDigest("overlay") }, runner: { id: "right-release", source_sha256: ticketDigest("source"), binary_sha256: ticketDigest("binary") }, bound_inputs: [{ kind: "runtime", id: "node", sha256: ticketDigest("node") }], execution: { command_id: "right-release.model-promote", argv: ["right-release", "model", "promote"], cwd: ".", environment: [], cadence_seconds: 1, timeout_seconds: 60, output_limit_bytes: 1000 }, required_outputs: [], hosts: { required_count: 2, identities: [{ id: "mac-host", platform: "mac", attestation_sha256: ticketDigest("mac") }, { id: "windows-host", platform: "windows", attestation_sha256: ticketDigest("windows") }] }, policy: { rubric_id: "r7", rubric_sha256: ticketDigest("rubric") }, registries: { command_registry_sha256: ticketDigest("commands"), skill_registry_sha256: ticketDigest("skills") }, signing: { key_id: "fixture", algorithm: "ed25519", signature: "" } });
  const identity = { candidate_commit: commit, candidate_tree: tree, overlay_sha256: ticket.candidate.overlay_sha256, runner_sha256: canonicalDigest(ticket.runner), bound_inputs_sha256: canonicalDigest(ticket.bound_inputs), execution_sha256: canonicalDigest(ticket.execution), policy_sha256: canonicalDigest(ticket.policy), registries_sha256: canonicalDigest(ticket.registries) };
  const receipts = ticket.hosts.identities.map((host) => signVerification({ schema: "verification-host-receipt.v1", status: "PASS", host, ticket_sha256: documentDigest(ticket), preflight: identity, started_at: "2026-08-02T00:00:00.000Z", ended_at: "2026-08-02T00:01:00.000Z", exit_status: 0, outputs: [], postflight: identity, signing: { key_id: "fixture", algorithm: "ed25519", signature: "" } }));
  return { ticket, hostReceipts: receipts, promotionReceipt: signVerification({ schema: "verification-promotion-receipt.v1", status: "PASS", candidate_commit: commit, candidate_tree: tree, ticket_sha256: documentDigest(ticket), host_receipts: receipts.map(documentDigest), policy: { ...ticket.policy, pass_count: 114 }, evidence_quality: { partial: 0, degraded: 0, stale: 0, waived: 0, model_only: 0, advisory: 0 }, sentinel_close: { ticket_sha256: documentDigest(ticket), ledger_sha256: ticketDigest("ledger"), state_sha256: ticketDigest("state"), candidate_commit: commit, candidate_tree: tree, authority: "host" }, signing: { key_id: "bounded-admin-signer", algorithm: "ed25519", signature: "" } }), trustedKeys: { fixture: verificationKeys.publicKey, "bounded-admin-signer": verificationKeys.publicKey }, candidateCommit: commit, candidateTree: tree };
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
        entitlement: { appKey: "scraperight", tier: "pro" },
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
    adminTrust: fixedAdminTrust,
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
  assert.match(result.envelope.manifest.pointerKey, /\/current\/manifest\.json$/);
});

test("dry-run validates and signs without invoking process or network boundaries", async () => {
  const events = [];
  const result = await promoteRuntimeArtifact(request({ dryRun: true }), boundaries(events));
  assert.deepEqual(events, []);
  assert.deepEqual(result.steps, ["upload immutable object", "register signed envelope", "replace stable pointer"]);
  assert.equal(result.envelope.signature.algorithm, "Ed25519");
});

test("model promotion ignores caller-supplied verification trust", async () => {
  const events = [];
  const verification = { ...verificationFixture(), trustedKeys: { attacker: "caller-controlled" } };
  await promoteRuntimeArtifact(request({ verification, dryRun: true }), boundaries(events));
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
});

test("portable CLI rejects caller-supplied verification keys without a host trust store", async () => {
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
    const fixtureCommit = spawnSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).stdout.trim();
    const fixtureTree = spawnSync("git", ["rev-parse", "HEAD^{tree}"], { cwd: root, encoding: "utf8" }).stdout.trim();
    const verification = verificationFixture(fixtureCommit, fixtureTree);
    const verificationPublicKey = verificationKeys.publicKey.export({ type: "spki", format: "pem" }).toString();
    await Promise.all([
      writeFile(path.join(root, "ticket.json"), JSON.stringify(verification.ticket)),
      writeFile(path.join(root, "mac-receipt.json"), JSON.stringify(verification.hostReceipts[0])),
      writeFile(path.join(root, "windows-receipt.json"), JSON.stringify(verification.hostReceipts[1])),
      writeFile(path.join(root, "promotion-receipt.json"), JSON.stringify(verification.promotionReceipt)),
      writeFile(path.join(root, "verification-public.pem"), verificationPublicKey),
    ]);
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
      manifest: request().config.manifest,
    };
    await writeFile(path.join(root, "promotion.json"), JSON.stringify(config));

    const result = spawnSync(process.execPath, [
      path.join(packageRoot, "cli/right-release.mjs"), "model", "promote",
      "--authority", "heardright",
      "--config", path.join(root, "promotion.json"),
      "--signing-key-file", path.join(root, "key.pem"),
      "--verification-ticket", path.join(root, "ticket.json"),
      "--host-receipt", path.join(root, "mac-receipt.json"),
      "--host-receipt", path.join(root, "windows-receipt.json"),
      "--promotion-receipt", path.join(root, "promotion-receipt.json"),
      "--dry-run",
    ], { cwd: root, encoding: "utf8", windowsHide: true });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /bounded admin trust (store|path) is unavailable/);
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
