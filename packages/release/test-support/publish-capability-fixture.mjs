// Test-only: a publish capability for the publisher-internals tests (publishGatedTarball, runCargoPublish).
//
// There is no shortcut here. mintTestCapability builds a structurally complete admission record and the approval entries it
// needs, supplies the CI environment of the run the admission names for the duration of the call, and goes through the real
// createPublishCapability validator. A test therefore holds a capability only for the exact (registry, name, version, sha256)
// it asks for, and the validator that refuses a forged or half-built admission in production refuses it here too.
// Not shipped: lives under test-support/, outside package.json "files".
import crypto from "node:crypto";
import { PUBLISH_REPOSITORY, createPublishCapability } from "../host-upload-refusal.mjs";

const hex = (seed, length) => crypto.createHash("sha256").update(String(seed)).digest("hex").slice(0, length);

/** A passing admission record for the given archives, as the credentialed job's hashed admission artifact holds it. */
export function fakeAdmission({ archives, kind = "semantic-admission", verifierSha = hex("verifier", 40), runId = "9001", runAttempt = "1", mutate } = {}) {
  const admission = {
    schema: 1, kind,
    original: { candidateId: "fixture-candidate", candidateSha256: hex("candidate", 64), sourceSha: hex("source", 40), manifestSha256: hex("manifest", 64), archives: archives.map(({ name, version, sha256 }) => ({ name, version, file: `${name.replace(/[@/]/g, "-")}-${version}.pkg`, sha256 })) },
    sourceTree: hex("tree", 40), sourceHashes: { "Cargo.lock": hex("lock", 64) }, verifierSha, runId, runAttempt,
    units: archives.map(({ name, version, sha256 }) => ({ name, version, archiveSha256: sha256, ok: true, failures: [], evidenceSha256: hex(`evidence ${name}`, 64) })),
  };
  mutate?.(admission);
  return admission;
}

/** The CI environment of the run an admission names. */
export const ciEnvironment = (admission) => ({ GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", GITHUB_REPOSITORY: PUBLISH_REPOSITORY, GITHUB_SHA: admission.verifierSha, GITHUB_RUN_ID: String(admission.runId), GITHUB_RUN_ATTEMPT: String(admission.runAttempt) });

/** Approval-commit entries (ci/approved-artifacts.json) for the archives. */
export const fakeApproval = (registry, archives) => archives.map(({ name, version, sha256 }) => ({ name, registry, reviewRef: "fixture", reviewer: "fixture", sha256, version }));

/** Runs `body` with `env` applied to process.env, restoring every touched variable afterwards. */
export function withEnvironment(env, body) {
  const before = Object.fromEntries(Object.keys(env).map((key) => [key, process.env[key]]));
  Object.assign(process.env, env);
  try { return body(); } finally { for (const [key, value] of Object.entries(before)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; } }
}

/** A capability for `archives` ([{ name, version, sha256 }]) on `registry`, created through the production validator. */
export function mintTestCapability({ registry, archives, admission = fakeAdmission({ archives }), approved = fakeApproval(registry, archives), env = ciEnvironment(admission) }) {
  return withEnvironment(env, () => createPublishCapability({ admission, approved, registry }));
}
