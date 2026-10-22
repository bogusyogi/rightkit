// Dependency-free (no imports) refusal shared by every module that can put bytes in a registry. RightKit crates and @rightkit
// packages reach a registry from CI only (publish.yml qualification, the approval commit in ci/approved-artifacts.json,
// publish-reviewed.yml). No environment variable, flag or caller-set GITHUB_ACTIONS lifts this: CI uploads go through
// scripts/ci/publish.mjs and scripts/ci/reviewed-candidate.mjs, never through right-release.
export const HOST_UPLOAD_REFUSED = "RK_HOST_UPLOAD_REFUSED";
export function hostUploadRefusalMessage(what) {
  return `${what}: host uploads are retired; RightKit publishes only through CI's approved-hash path (publish.yml, approval in ci/approved-artifacts.json, publish-reviewed.yml). Use --dry-run, preflight or --local --qualify-only to qualify locally.`;
}
export function refuseHostUpload(what) {
  throw Object.assign(new Error(hostUploadRefusalMessage(what)), { code: HOST_UPLOAD_REFUSED });
}

// ---- publish capability -------------------------------------------------------------------------------------------------
// The publisher internals (publishGatedTarball in publication-gate.mjs, runCargoPublish in publish-cargo.mjs) are exported and read
// registry credentials, so being importable must not mean being able to publish. Each of them refuses (refuseHostUpload) unless
// it is handed a CAPABILITY, and a capability exists only when createPublishCapability accepted a validated admission:
//   - the hashed admission record the credentialed job builds (schema 1, "semantic-admission" or "local-admission": original
//     binding, source tree, verifier SHA, run and attempt, one passing verdict per archive);
//   - an approval commit entry (ci/approved-artifacts.json: registry, name, version, sha256, reviewer) for every archive in it;
//   - the CI run it names: GITHUB_ACTIONS=true on refs/heads/main of the repository, GITHUB_SHA equal to the admission's verifier
//     SHA, GITHUB_RUN_ID and GITHUB_RUN_ATTEMPT equal to its run.
// The capability is an object registered in a module-private WeakMap at creation: an object literal, a copy or a mutated clone is
// not registered and is refused. It is bound to the units of the admission (name, version, archive sha256, registry), so one
// capability publishes nothing else, and the publisher also compares the sha256 of the bytes it is about to send. Nothing here
// reads an environment variable to LIFT a refusal: the environment is one of the conditions the admission must match.
const HEX64 = /^[0-9a-f]{64}$/;
const HEX40 = /^[0-9a-f]{40}$/;
const POSITIVE = /^[1-9][0-9]*$/;
export const PUBLISH_REPOSITORY = "bogusyogi/rightkit";
const CAPABILITY_REGISTRIES = ["npm", "crates"];
const ADMISSION_KINDS = ["semantic-admission", "local-admission"];
const minted = new WeakMap();
const plain = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** Every reason `admission` is not a valid, passing admission record for `registry` in this CI run (empty means valid). */
export function admissionProblems(admission, { registry, env = process.env, approved = [] } = {}) {
  const problems = [];
  const bad = (text) => problems.push(text);
  if (!CAPABILITY_REGISTRIES.includes(registry)) bad(`registry ${JSON.stringify(registry)} is not npm or crates`);
  if (!plain(admission)) return [...problems, "the admission is not an object"];
  if (admission.schema !== 1 || !ADMISSION_KINDS.includes(admission.kind)) bad("the admission schema or kind is not a reviewed admission");
  const original = admission.original;
  if (!plain(original) || typeof original.candidateId !== "string" || !original.candidateId || !HEX64.test(original.candidateSha256 ?? "") || !HEX40.test(original.sourceSha ?? "") || !HEX64.test(original.manifestSha256 ?? "") || !Array.isArray(original.archives) || original.archives.length === 0) bad("the admission does not bind an original candidate, manifest and archives");
  if (!HEX40.test(admission.sourceTree ?? "") || !HEX40.test(admission.verifierSha ?? "") || !POSITIVE.test(String(admission.runId ?? "")) || !POSITIVE.test(String(admission.runAttempt ?? ""))) bad("the admission does not name its source tree, verifier commit and run");
  const archives = Array.isArray(original?.archives) ? original.archives : [];
  const units = Array.isArray(admission.units) ? admission.units : [];
  if (units.length !== archives.length) bad("the admission holds no verdict for exactly its archives");
  const names = new Set();
  for (const archive of archives) {
    const label = `${archive?.name}@${archive?.version}`;
    if (!plain(archive) || typeof archive.name !== "string" || !archive.name || typeof archive.version !== "string" || !archive.version || !HEX64.test(archive.sha256 ?? "")) { bad(`archive ${label} is malformed`); continue; }
    if (names.has(archive.name)) bad(`archive ${label} is listed twice`);
    names.add(archive.name);
    const verdict = units.find((row) => row?.name === archive.name);
    if (!verdict || verdict.version !== archive.version || verdict.archiveSha256 !== archive.sha256 || verdict.ok !== true || !Array.isArray(verdict.failures) || verdict.failures.length !== 0 || !HEX64.test(verdict.evidenceSha256 ?? "")) bad(`${label} has no passing verdict for its archive`);
    const entries = (Array.isArray(approved) ? approved : []).filter((entry) => entry?.registry === registry && entry.name === archive.name && entry.version === archive.version);
    if (entries.length === 0 || entries.some((entry) => entry.sha256 !== archive.sha256)) bad(`${label} is not approved in ci/approved-artifacts.json with this archive hash`);
  }
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF !== "refs/heads/main" || env.GITHUB_REPOSITORY !== PUBLISH_REPOSITORY) bad("not running in GitHub Actions on refs/heads/main of the RightKit repository");
  if (!HEX40.test(env.GITHUB_SHA ?? "") || env.GITHUB_SHA !== admission.verifierSha || env.GITHUB_RUN_ID !== String(admission.runId) || env.GITHUB_RUN_ATTEMPT !== String(admission.runAttempt)) bad("the admission was built by another commit, run or attempt");
  return problems;
}

/**
 * The capability to publish exactly the archives of one validated admission to `registry`. Throws (HOST_UPLOAD_REFUSED) unless
 * `admission` passes admissionProblems against the current CI environment and the approval entries in `approved`
 * (ci/approved-artifacts.json `artifacts`, as scripts/ci/upload.mjs loadApprovedArtifacts returns them).
 */
export function createPublishCapability({ admission, approved, registry }) {
  const problems = admissionProblems(admission, { registry, env: process.env, approved });
  if (problems.length) refuseHostUpload(`a publish capability needs a validated admission (${problems.join("; ")})`);
  const record = Object.freeze({ kind: "rightkit-publish-capability", registry });
  minted.set(record, { registry, units: new Map(admission.original.archives.map((archive) => [archive.name, { version: archive.version, sha256: archive.sha256 }])), admissionVerifier: admission.verifierSha });
  return record;
}

/**
 * Refuses (HOST_UPLOAD_REFUSED) unless `capability` was created by createPublishCapability for this `registry` and names the
 * unit. When `version` and `sha256` are given they must equal the admitted archive's: the publisher passes the version and the
 * sha256 of the bytes it is about to send.
 */
export function assertPublishCapability(capability, { what, registry, name, version, sha256 }) {
  const grant = typeof capability === "object" && capability !== null ? minted.get(capability) : undefined;
  if (!grant) refuseHostUpload(`${what}: no publish capability (a validated admission is required)`);
  if (grant.registry !== registry) refuseHostUpload(`${what}: the publish capability is for ${grant.registry}, not ${registry}`);
  const unit = grant.units.get(name);
  if (!unit) refuseHostUpload(`${what}: ${name} is not an archive of the admission behind the publish capability`);
  if (version !== undefined && unit.version !== version) refuseHostUpload(`${what}: ${name}@${version} is not the admitted ${unit.version}`);
  if (sha256 !== undefined && unit.sha256 !== sha256) refuseHostUpload(`${what}: the bytes of ${name} (${String(sha256).slice(0, 12)}) are not the admitted archive (${unit.sha256.slice(0, 12)})`);
}
