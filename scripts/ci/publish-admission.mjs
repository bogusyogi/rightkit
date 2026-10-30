#!/usr/bin/env node
// Publication admission, established inside the qualification run itself (publish.yml job `scans`, after the class A scans, so the
// pinned class A tools are on PATH), before plan, pack or any build lane.
//
// The same trusted admission runs for every dispatch, whoever asked for it. The `admission` input only says who asked:
//   broker  a RustRight broker's rightkit-dispatch adapter on a managed host; request_id is its job id, recorded as correlation only.
//           This run cannot verify the broker's signature, so the id grants nothing: a raw dispatch with an invented id gets exactly
//           the checks below and nothing more.
//   hosted  no broker exists (a cloud session); request_id must be empty, and the scope is one @rightkit/* npm package.
// The checks, on the dispatched commit (EXPECTED_SHA == GITHUB_SHA, main of this repository):
//   1. the full gate: `rightkit-gate check --commit <sha>` from this checkout (app rules, class A over the whole commit, and
//      RightKit's own rules), exit 0 with its pass lines; a partial view (exit 3) or a red check admits nothing;
//   2. every unit is exactly one workspace package (npm) or crate (crates/<unit>) at that commit, with a publishable version;
//   3. that version is not on the registry yet (a version is published once; an unreadable registry is no proof of absence);
//   4. every allowset entry is granted by the committed exception policy (rk-preflight.mjs exceptionAllowsets, the broker's own).
//   5. the qualification is AUTHENTICATED, not asserted: its compile and package stages carry the local broker's signed job receipts
//      (rustright.provenance.v1), verified here against the attestation pins committed in ci/rk-pipeline.json AT THE DISPATCHED COMMIT,
//      each bound by its nonce to this commit, its real git tree, the unit and the stage (verifyQualificationReceipts).
// The receipt (publish-admission-<attempt>) is an artifact of this run and attempt. publish-direct's uploader reads it from the run's
// own artifact listing and refuses the upload unless it binds this repository, commit, run, attempt, registry, units, versions,
// allowsets and a passed gate to the sealed manifest and the verify receipts (verifyRunAdmission). Nothing here is signed by a
// caller: the runner that wrote it is the run the uploader already trusts for every other receipt.
//
//   publish-admission.mjs admit --out <dir>               (publish.yml scans job, inside Actions)
//   publish-admission.mjs dispatch --unit <@rightkit/x>   (cloud session: rightkit-gate check, then dispatch admission=hosted)
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT, exclusionsFor, npmContentDigest } from "../../packages/release/pack-digest.mjs";
import { ATTESTED_STAGES, enrolledPins, verifyQualificationReceipts } from "../../packages/release/qualification-verify.mjs";
import { SWAP_HELPER_PACKAGE } from "../../packages/release/swap-helper-bundle.mjs";

export const ADMISSION_SCHEMA = "rk-publish-admission.v2";
export const ADMISSION_FILE = "admission.json";
export const REPOSITORY = "bogusyogi/rightkit";
export const HOSTED_UNIT = /^@rightkit\/[a-z][a-z0-9-]{0,63}$/;
const REQUEST_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const ALLOW_INPUTS = { hostExclusion: "ALLOW_HOST_EXCLUSION", noTest: "ALLOW_NO_TEST", ignored: "ALLOW_IGNORED", missingAssets: "ALLOW_MISSING_ASSETS" };
const POLICY_KEYS = { hostExclusion: "allow_host_exclusion", noTest: "allow_no_test", ignored: "allow_ignored", missingAssets: "allow_missing_assets" };
const RECEIPT_KEYS = ["schema", "mode", "requestId", "repository", "sourceSha", "runId", "runAttempt", "registry", "units", "versions", "allow", "gate", "qualification"];
const CRATE_NAME = /^[a-z][a-z0-9_-]{0,63}$/;
const HERE = path.dirname(fileURLToPath(import.meta.url));

const refuse = (message) => { throw new Error(`publication admission refused: ${message}`); };
const requireThat = (condition, message) => { if (!condition) refuse(message); };
const words = (text) => String(text ?? "").split(/\s+/).filter(Boolean);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const sorted = (list) => [...list].sort();
const sameSet = (a, b) => a.length === b.length && same(sorted(a), sorted(b));

export function admissionArtifactName(runAttempt) {
  requireThat(/^[1-9][0-9]*$/.test(String(runAttempt)), "invalid run attempt");
  return `publish-admission-${runAttempt}`;
}

/** Whether the registry already holds `unit@version`. Only a 404 counts as absent; anything else unreadable refuses. */
async function published(registry, unit, version, fetchImpl) {
  const url = registry === "npm" ? `https://registry.npmjs.org/${unit.replace("/", "%2f")}` : `https://crates.io/api/v1/crates/${unit}/${version}`;
  const response = await fetchImpl(url, { headers: { accept: "application/json", "user-agent": "rightkit-publish-admission (github.com/bogusyogi/rightkit)" }, redirect: "error", signal: AbortSignal.timeout(60_000) });
  if (response.status === 404) return false;
  requireThat(response.ok, `${registry} registry returned ${response.status} for ${unit}`);
  // A 200 is evidence only when it is this package's own record: malformed or foreign metadata is unreadable, never "absent".
  let doc;
  try { doc = await response.json(); } catch { refuse(`${registry} registry answered 200 for ${unit} with no JSON record`); }
  const record = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
  if (registry === "crates") {
    requireThat(record(doc) && record(doc.version) && doc.version.crate === unit && doc.version.num === version, `crates.io answered 200 for ${unit}@${version} with a record of another or no crate version`);
    return true;
  }
  requireThat(record(doc) && doc.name === unit && record(doc.versions), `npm answered 200 for ${unit} with a malformed or foreign packument`);
  return Object.hasOwn(doc.versions, version);
}

/** The committed version of one unit at `sha`: the one workspace package named `unit` (npm) or crates/<unit>/Cargo.toml (crates). */
function committedVersion(registry, unit, sha, git) {
  if (registry === "npm") {
    const dirs = git("ls-tree", "--name-only", `${sha}:packages`).split("\n").filter(Boolean);
    const owners = dirs.map((dir) => {
      let text;
      try { text = git("show", `${sha}:packages/${dir}/package.json`); } catch { return null; }
      const manifest = JSON.parse(text);
      return manifest.name === unit ? manifest : null;
    }).filter(Boolean);
    requireThat(owners.length === 1, `${unit} is not exactly one workspace package at ${sha.slice(0, 12)}`);
    requireThat(owners[0].private !== true, `${unit} is private`);
    requireThat(typeof owners[0].version === "string" && /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(owners[0].version), `${unit} has no publishable version`);
    return owners[0].version;
  }
  requireThat(CRATE_NAME.test(unit), `${unit} is not a crate name`);
  let toml;
  try { toml = git("show", `${sha}:crates/${unit}/Cargo.toml`); } catch { refuse(`${unit} is not a crate of this workspace at ${sha.slice(0, 12)}`); }
  const version = /^\[package\][^[]*?^version\s*=\s*"([^"]+)"/ms.exec(toml)?.[1];
  requireThat(version && /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version), `${unit} has no publishable version`);
  return version;
}

/** The gate's own verdict on the commit: exit 0, the app-check pass line for this commit and the class A pass line for this full commit. */
export function gateVerdict(result, sha) {
  const out = String(result?.stdout ?? "");
  const rules = new RegExp(`^check: ${sha.slice(0, 12)} passed (\\d+) rules \\(release tier\\) in \\d+(?:\\.\\d+)?s(?: \\(.*\\))?$`, "m").exec(out);
  const classA = new RegExp(`^rightkit-gate: class A content pass: (\\d+) deterministic scan\\(s\\) over (\\d+) file\\(s\\) of ${sha}$`, "m").exec(out);
  requireThat(result?.status === 0 && rules && classA && Number(rules[1]) > 0 && Number(classA[1]) > 0, `rightkit-gate check did not pass at ${sha.slice(0, 12)} (exit ${result?.status}); nothing is admitted`);
  return { command: `rightkit-gate check --commit ${sha} --rule-tier release`, exit: 0, rules: Number(rules[1]), classAScans: Number(classA[1]), classAFiles: Number(classA[2]) };
}

/**
 * Admits the dispatched request inside publish.yml and returns the receipt. `env` is the Actions environment plus the dispatch
 * inputs (ADMISSION, REQUEST_ID, EXPECTED_SHA, REGISTRY, UNITS, ALLOW_*); `git(...args)` reads the dispatched commit; `runGate(sha)`
 * runs `rightkit-gate check --commit <sha> --rule-tier release` on this checkout and returns { status, stdout }.
 */
export async function admit({ env, git, runGate, fetchImpl = fetch, policy }) {
  requireThat(env.GITHUB_ACTIONS === "true" && env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_REF === "refs/heads/main", "admission runs only in Actions on main of the repository");
  requireThat(/^[0-9a-f]{40}$/.test(env.GITHUB_SHA ?? "") && env.EXPECTED_SHA === env.GITHUB_SHA, "the dispatched commit is not the run's commit");
  requireThat(/^[1-9][0-9]*$/.test(env.GITHUB_RUN_ID ?? "") && /^[1-9][0-9]*$/.test(env.GITHUB_RUN_ATTEMPT ?? ""), "no run id or attempt");
  const sha = env.GITHUB_SHA, registry = env.REGISTRY, units = words(env.UNITS);
  requireThat(["npm", "crates"].includes(registry) && units.length > 0 && new Set(units).size === units.length, "admission needs a registry and distinct units");
  // One door (Adrian, 2026-10-27): only the broker-admitted dispatch of rightkit-gate --to release is admitted, and only with the
  // gate's qualification receipt for this commit and unit. Hosted self-admission (class A here, no local compile or qualify) is retired.
  requireThat(env.ADMISSION === "broker", "admission must be broker: hosted self-admission is retired; publish through rightkit-gate --to release");
  requireThat(REQUEST_ID.test(env.REQUEST_ID ?? ""), "broker admission needs the broker's request id (correlation only; it grants nothing)");
  const qualification = readQualificationInput(env.QUALIFICATION, { sha, registry, units });
  if (registry === "npm") requireThat(units.every((unit) => HOSTED_UNIT.test(unit)), "npm units must be @rightkit/* packages");
  // The full gate first: a red or partial check admits nothing, whatever the registry says.
  const gate = gateVerdict(runGate(sha), sha);
  const versions = {};
  const granted = { hostExclusion: new Set(), noTest: new Set(), ignored: new Set(), missingAssets: new Set() };
  for (const unit of units) {
    const version = committedVersion(registry, unit, sha, git);
    requireThat(!(await published(registry, unit, version, fetchImpl)), `${unit}@${version} is already on ${registry}; a version is published once`);
    versions[unit] = version;
    const policyFor = policy(unit, version);
    for (const [key, name] of Object.entries(POLICY_KEYS)) for (const item of policyFor[name] ?? []) granted[key].add(item);
  }
  const allow = Object.fromEntries(Object.entries(ALLOW_INPUTS).map(([key, name]) => [key, sorted(words(env[name]))]));
  for (const [key, list] of Object.entries(allow)) for (const item of list) requireThat(granted[key].has(item), `${POLICY_KEYS[key]} names ${item}, which the committed exception policy does not grant`);
  // Authenticate it (after the cheap scope and version checks above): unsigned booleans admit nothing.
  qualification.provenance = verifyQualificationReceipts(env.QUALIFICATION, { sha, git });
  // The check and qualify stages run in the gate's own process with no broker job, so no signed receipt can exist for them: their `ok`
  // is the caller's word and is recorded as such. The evidence that remains is this run's own gate (below), its own qualification and
  // the sealed archive's digest being bound to the qualification's artifactSha256 (bindPackedToQualification).
  qualification.unattested = ["check", "qualify"];
  requireThat(qualification.unit.version === versions[qualification.unit.name], `the qualification receipt qualified ${qualification.unit.name}@${qualification.unit.version}, the committed version is ${versions[qualification.unit.name]}`);
  return { schema: ADMISSION_SCHEMA, mode: env.ADMISSION, requestId: env.ADMISSION === "broker" ? env.REQUEST_ID : "", repository: REPOSITORY, sourceSha: sha, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, qualification, registry, units, versions, allow, gate };
}

export const QUALIFICATION_SCHEMA = "rightkit.unit-qualification/1";
/** The dispatcher's `qualification` input: the gate's local receipt, as rightkit-dispatch forwarded it. Every field is checked
 * against what this run knows (commit, registry, units); the stages must all be ok; one artifact sha256 binds the bytes. */
export function readQualificationInput(text, { sha, registry, units }) {
  requireThat(typeof text === "string" && text.trim(), "broker admission needs the gate's qualification receipt (input `qualification`); a dispatch without one is not gated");
  let q;
  try { q = JSON.parse(text); } catch { refuse("the qualification input is not JSON"); }
  requireThat(q?.schema === QUALIFICATION_SCHEMA, `qualification schema ${JSON.stringify(q?.schema)} is not ${QUALIFICATION_SCHEMA}`);
  requireThat(q.commit === sha, `the qualification receipt is for ${String(q.commit).slice(0, 12)}, not this run's ${sha.slice(0, 12)}`);
  const kind = registry === "npm" ? "npm" : "cargo";
  requireThat(q.unit?.kind === kind && units.length === 1 && q.unit?.name === units[0], `the qualification receipt covers ${q.unit?.kind}:${q.unit?.name}; this run publishes ${registry} ${units.join(" ")}`);
  requireThat(/^[0-9a-f]{40}$/.test(q.tree ?? "") && /^\d+\.\d+\.\d+/.test(q.unit?.version ?? ""), "the qualification receipt lacks a tree or a version");
  for (const stage of ["check", "compile", "package", "qualify"]) requireThat(q.stages?.[stage]?.ok === true, `the qualification receipt's ${stage} stage is not ok`);
  requireThat(/^[0-9a-f]{64}$/.test(q.artifactSha256 ?? ""), "the qualification receipt names no artifact sha256");
  // An npm unit is bound by its content digest (pack-digest.mjs), not by the archive's bytes: the qualification must carry it. For the
  // swap-helper package it must also record that the local archive held both excluded files (the hosted side checks the helper separately).
  const content = {};
  if (kind === "npm") {
    requireThat(/^[0-9a-f]{64}$/.test(q.contentSha256 ?? ""), "the qualification receipt names no content sha256 for its npm unit");
    content.contentSha256 = q.contentSha256;
    // Exclusions are this code's, for one package: a qualification that claims excluded entries for any other unit is refused outright.
    requireThat(exclusionsFor(q.unit.name).length > 0 || q.contentExcluded === undefined, `the qualification receipt claims excluded archive entries for ${q.unit.name}, which has none`);
    if (exclusionsFor(q.unit.name).length) {
      requireThat(Array.isArray(q.contentExcluded) && sameSet(q.contentExcluded, exclusionsFor(q.unit.name)), `the qualification receipt does not record that its ${q.unit.name} archive held ${exclusionsFor(q.unit.name).join(" and ")}`);
      content.contentExcluded = sorted(q.contentExcluded);
    }
  }
  return { schema: q.schema, commit: q.commit, tree: q.tree, runtime: q.runtime ?? null, executor: q.executor ?? null, unit: { kind: q.unit.kind, name: q.unit.name, version: q.unit.version }, artifactSha256: q.artifactSha256, ...content };
}

export { enrolledPins, verifyQualificationReceipts };

/**
 * The hosted pack must reproduce the locally qualified content: the sealed manifest's entry for the unit carries a content digest
 * (pack-digest.mjs: path, type, exec bit and sha256 of every entry) equal to the qualification's, or the run stops here with both digests
 * and the hosted archive's listing; nothing is repacked, nothing proceeds. The whole archive's sha256 is recorded for information, never equated:
 * gzip and entry metadata are not part of what a package ships.
 *
 * The one binary that cannot match is @rightkit/release's Developer-ID-signed swap helper (re-signed, timestamped, on every signing), so it is
 * left out of that digest by a constant of this code, for that package alone, and bound on its own: the sealed helper's sha256 is the
 * sign-swap job's (`swapSha256`, from its rightkit-swap.sha256), the sealed archive holds no `.sign.json` (that receipt is the local signing's),
 * and the local qualification records that its archive held both. `packed(entry)` reads the sealed archive (default: `<dir>/<entry.file>`).
 */
export function bindPackedToQualification({ manifest, qualification, listing = () => "", packed, swapSha256 }) {
  requireThat(manifest?.commit === qualification.commit, `the sealed manifest is for ${String(manifest?.commit).slice(0, 12)}, the qualification for ${qualification.commit.slice(0, 12)}`);
  const entry = (manifest.units ?? []).find((u) => u.name === qualification.unit.name);
  requireThat(entry, `the sealed manifest has no entry for ${qualification.unit.name}`);
  requireThat(entry.version === qualification.unit.version, `the sealed ${qualification.unit.name} is ${entry.version}, the qualification is ${qualification.unit.version}`);
  const name = qualification.unit.name;
  if (qualification.unit.kind !== "npm") {
    if (entry.sha256 !== qualification.artifactSha256) {
      refuse(`the hosted pack of ${name}@${entry.version} is not the locally qualified archive: hosted sha256 ${entry.sha256}, qualified ${qualification.artifactSha256}; stopped before verify and upload (no repack). Hosted archive listing:\n${listing(entry)}`);
    }
    return { unit: name, version: entry.version, sha256: entry.sha256, file: entry.file };
  }
  // The exclusions come from this code, keyed to the package; the qualification and the manifest never name one.
  const exclude = exclusionsFor(name);
  requireThat(typeof packed === "function", "no way to read the sealed archive");
  let hosted;
  try { hosted = npmContentDigest(packed(entry), { exclude }); } catch (error) { refuse(`the sealed ${name}@${entry.version} cannot be read (${error.message})`); }
  if (hosted.contentSha256 !== qualification.contentSha256) {
    refuse(`the hosted pack of ${name}@${entry.version} does not hold the locally qualified content: hosted content sha256 ${hosted.contentSha256}, qualified ${qualification.contentSha256} (archive sha256 hosted ${entry.sha256}, qualified ${qualification.artifactSha256}${exclude.length ? `; ${exclude.join(" and ")} are not compared here` : ""}); stopped before verify and upload (no repack). Hosted archive listing:\n${listing(entry)}`);
  }
  const bound = { unit: name, version: entry.version, sha256: entry.sha256, file: entry.file, contentSha256: hosted.contentSha256, qualifiedSha256: qualification.artifactSha256 };
  if (name !== SWAP_HELPER_PACKAGE) return bound;
  // The swap helper: the sealed one is exactly the one the sign-swap job signed, and the hosted archive carries no signing receipt of the local route.
  requireThat(sameSet(qualification.contentExcluded ?? [], exclude), `the qualification does not record that its ${name} archive held ${exclude.join(" and ")}`);
  const helper = hosted.excluded[SWAP_HELPER_ARCHIVE_BIN];
  requireThat(helper && helper.type === "file" && helper.exec === true, `the sealed ${name}@${entry.version} holds no executable ${SWAP_HELPER_ARCHIVE_BIN}`);
  requireThat(Object.hasOwn(hosted.excluded, SWAP_HELPER_ARCHIVE_RECEIPT) === false, `the sealed ${name}@${entry.version} holds ${SWAP_HELPER_ARCHIVE_RECEIPT}, the local signing's receipt: the hosted helper is signed by the sign-swap job and carries none`);
  const signed = /^([0-9a-f]{64})\s+\*?rightkit-swap\s*$/.exec(String(swapSha256 ?? ""))?.[1];
  requireThat(signed, `${name} needs the sign-swap job's rightkit-swap sha256 (its rightkit-swap.sha256) to bind the sealed helper`);
  requireThat(helper.sha256 === signed, `the sealed ${SWAP_HELPER_ARCHIVE_BIN} has sha256 ${helper.sha256}, not the sign-swap job's ${signed}`);
  return { ...bound, swapHelperSha256: helper.sha256 };
}

/**
 * Checks a receipt against what the uploader independently knows: the run (id, attempt, commit), the sealed manifest (registry,
 * units with versions) and the allowsets the verify receipts ran with. Returns the receipt.
 */
export function verifyAdmissionReceipt(receipt, { repository, sourceSha, runId, runAttempt, registry, units, allow }) {
  requireThat(receipt && typeof receipt === "object" && !Array.isArray(receipt) && same(Object.keys(receipt).sort(), [...RECEIPT_KEYS].sort()), "receipt has unexpected or missing keys");
  requireThat(receipt.schema === ADMISSION_SCHEMA, "unknown receipt schema");
  requireThat(receipt.repository === repository && receipt.sourceSha === sourceSha, "receipt is for another repository or commit");
  requireThat(String(receipt.runId) === String(runId) && String(receipt.runAttempt) === String(runAttempt), "receipt is for another run or attempt");
  requireThat(receipt.registry === registry, "receipt registry differs from the sealed manifest");
  const names = units.map((unit) => unit.name);
  requireThat(Array.isArray(receipt.units) && same(sorted(receipt.units), sorted(names)), "receipt units differ from the sealed manifest");
  requireThat(receipt.allow && same(Object.keys(receipt.allow).sort(), Object.keys(ALLOW_INPUTS).sort()), "receipt allowsets are incomplete");
  for (const key of Object.keys(ALLOW_INPUTS)) requireThat(Array.isArray(allow[key]) && same(receipt.allow[key], sorted(allow[key])), `receipt ${key} differs from the verify receipts`);
  requireThat(receipt.versions && same(sorted(Object.keys(receipt.versions)), sorted(names)) && units.every((unit) => receipt.versions[unit.name] === unit.version), "receipt versions differ from the sealed versions");
  const gate = receipt.gate;
  requireThat(gate && same(Object.keys(gate).sort(), ["classAFiles", "classAScans", "command", "exit", "rules"]) && gate.exit === 0 && gate.command === `rightkit-gate check --commit ${sourceSha} --rule-tier release` && Number.isInteger(gate.rules) && gate.rules > 0 && Number.isInteger(gate.classAScans) && gate.classAScans > 0, "receipt carries no passed gate for this commit");
  requireThat(receipt.mode === "broker", "unknown admission mode: only broker admission exists");
  requireThat(REQUEST_ID.test(receipt.requestId), "broker receipt carries no valid request id");
  // The qualification the admission checked must name this commit, one of the sealed units and its sealed version.
  const q = receipt.qualification;
  requireThat(q && q.schema === QUALIFICATION_SCHEMA && q.commit === sourceSha && /^[0-9a-f]{64}$/.test(q.artifactSha256 ?? ""), "receipt carries no qualification for this commit");
  requireThat(units.some((unit) => unit.name === q.unit?.name && unit.version === q.unit?.version), "receipt qualification names no sealed unit at its sealed version");
  requireThat(ATTESTED_STAGES.every((stage) => Array.isArray(q.provenance?.[stage]) && q.provenance[stage].length > 0), "receipt qualification carries no verified broker provenance for its compile and package stages");
  return receipt;
}

/** The uploader's side: the run's `scans` job (gate, then admission) succeeded in this attempt and its one receipt artifact verifies. */
export async function verifyRunAdmission({ repository, sourceSha, runId, runAttempt, registry, units, allow, jobs, listing, env, fetchImpl, api, artifactMatches, unpack, workDir }) {
  const job = jobs.filter((row) => row.name === "scans");
  requireThat(job.length === 1 && job[0].status === "completed" && job[0].conclusion === "success" && String(job[0].run_attempt) === String(runAttempt) && job[0].head_sha === sourceSha, `scans (gate and admission) did not succeed in attempt ${runAttempt}`);
  const name = admissionArtifactName(runAttempt);
  const found = listing.filter((artifact) => artifact?.name === name);
  requireThat(found.length === 1 && artifactMatches(found[0], { name, runId, sourceSha, repository }), `${name} must exist exactly once in run ${runId} at ${sourceSha}`);
  const bytes = await api(`actions/artifacts/${found[0].id}/zip`, env, fetchImpl, true);
  requireThat(`sha256:${crypto.createHash("sha256").update(bytes).digest("hex")}` === found[0].digest, "admission ZIP digest mismatch");
  const destination = path.join(workDir, "publish-admission");
  const names = unpack({ bytes, zipFile: path.join(workDir, "publish-admission.zip"), destination });
  requireThat(names.length === 1 && names[0] === ADMISSION_FILE, `admission artifact is not exactly one ${ADMISSION_FILE}`);
  let receipt;
  try { receipt = JSON.parse(fs.readFileSync(path.join(destination, ADMISSION_FILE), "utf8")); } catch { refuse("admission receipt is not valid JSON"); }
  return verifyAdmissionReceipt(receipt, { repository: repository.full_name, sourceSha, runId, runAttempt, registry, units, allow });
}

/**
 * The cloud entry: rightkit-gate check passes on this exact commit, the commit is origin's main, then publish.yml is dispatched
 * with admission=hosted for one package and the committed policy's allowsets. It never chooses a request id. It is a convenience,
 * not the boundary: a raw dispatch skips it, and the run's own admission (scans job) re-runs the gate and every check regardless.
 */
export function dispatchHosted({ unit, root, run = spawnSync, out = (line) => process.stdout.write(`${line}\n`), policy }) {
  // Retired (Adrian, 2026-10-27: one door): a hosted self-admission dispatched from a shell skips the local compile and qualify
  // stages. The only publication entry is rightkit-gate --to release on a broker-managed host.
  void unit; void root; void run; void out; void policy;
  refuse("hosted dispatch is retired: publish through rightkit-gate --to release --unit <kind:name> on a broker-managed host");
  requireThat(HOSTED_UNIT.test(unit ?? ""), "--unit names exactly one @rightkit/* package");
  const sh = (cmd, args, options = {}) => run(cmd, args, { cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
  const text = (cmd, args) => { const r = sh(cmd, args); requireThat(r.status === 0, `${cmd} ${args.join(" ")} failed`); return r.stdout.trim(); };
  requireThat(text("git", ["status", "--porcelain", "--untracked-files=no"]) === "", "the checkout has uncommitted tracked changes");
  const head = text("git", ["rev-parse", "HEAD"]);
  const main = text("gh", ["api", `repos/${REPOSITORY}/commits/main`, "--jq", ".sha"]);
  requireThat(head === main, `HEAD ${head.slice(0, 12)} is not origin main ${main.slice(0, 12)}`);
  const manifest = JSON.parse(text("git", ["show", `${head}:${packageDir(root, head, unit, text)}/package.json`]));
  const gate = sh(process.execPath, [path.join(root, "packages", "release", "bin", "rightkit-gate.mjs"), "check", "--repo", root, "--commit", head, "--rule-tier", "release"], { stdio: ["ignore", "pipe", "inherit"] });
  out(String(gate.stdout ?? "").trimEnd());
  gateVerdict(gate, head); // the same verdict the run's admission requires; this one only saves a run that would be refused

  const granted = policy(unit, manifest.version);
  const fields = ["-f", "ref=main", "-f", "inputs[admission]=hosted", "-f", "inputs[request_id]=", "-f", `inputs[expected_sha]=${head}`, "-f", "inputs[registry]=npm", "-f", `inputs[units]=${unit}`];
  for (const key of Object.values(POLICY_KEYS)) fields.push("-f", `inputs[${key}]=${(granted[key] ?? []).join(" ")}`);
  text("gh", ["api", "-X", "POST", `repos/${REPOSITORY}/actions/workflows/publish.yml/dispatches`, ...fields]);
  out(`dispatched publish.yml admission=hosted for ${unit}@${manifest.version} at ${head}; the run's admission job re-admits it before any build`);
  return { sha: head, unit, version: manifest.version };
}

function packageDir(root, sha, unit, text) {
  const dirs = text("git", ["ls-tree", "--name-only", `${sha}:packages`]).split("\n").filter(Boolean);
  const hits = dirs.filter((dir) => { try { return JSON.parse(text("git", ["show", `${sha}:packages/${dir}/package.json`])).name === unit; } catch { return false; } });
  requireThat(hits.length === 1, `${unit} is not exactly one workspace package`);
  return `packages/${hits[0]}`;
}

/** The committed exception policy, the same one rightkit-dispatch's admission applies (rk-preflight.mjs exceptionAllowsets). */
async function committedPolicy(root, read) {
  const { exceptionAllowsets } = await import("./rk-preflight.mjs");
  const config = JSON.parse(read("ci/gate.config.json"));
  const pipeline = JSON.parse(read("ci/rk-pipeline.json"));
  return (unit, version) => exceptionAllowsets(unit, version, config, pipeline);
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...rest] = process.argv.slice(2);
  const flag = (name) => { const i = rest.indexOf(name); return i < 0 ? undefined : rest[i + 1]; };
  const root = path.resolve(HERE, "..", "..");
  try {
    if (command === "admit") {
      const outDir = flag("--out");
      requireThat(outDir && rest.length === 2, "usage: publish-admission.mjs admit --out <dir>");
      const git = (...args) => { const r = spawnSync("git", args, { cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }); if (r.status !== 0) throw new Error(`git ${args[0]} failed`); return r.stdout; };
      const policy = await committedPolicy(root, (file) => git("show", `${process.env.GITHUB_SHA}:${file}`));
      const runGate = (sha) => spawnSync(process.execPath, [path.join(root, "packages", "release", "bin", "rightkit-gate.mjs"), "check", "--repo", root, "--commit", sha, "--rule-tier", "release"], { cwd: root, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] });
      const receipt = await admit({ env: process.env, git, runGate: (sha) => { const r = runGate(sha); process.stdout.write(r.stdout ?? ""); return r; }, policy });
      fs.mkdirSync(outDir, { recursive: true });
      fs.writeFileSync(path.join(outDir, ADMISSION_FILE), `${JSON.stringify(receipt, null, 2)}\n`, { flag: "wx" });
      process.stdout.write(`admission ${receipt.mode}: ${receipt.registry} ${receipt.units.join(" ")} at ${receipt.sourceSha} (run ${receipt.runId} attempt ${receipt.runAttempt})\n`);
    } else if (command === "bind") {
      const packed = flag("--packed");
      const swapFile = flag("--swap-sha256-file");
      requireThat(packed && (rest.length === 2 || (swapFile && rest.length === 4)), "usage: publish-admission.mjs bind --packed <dir> [--swap-sha256-file <rightkit-swap.sha256>] (QUALIFICATION, EXPECTED_SHA, REGISTRY, UNITS in the environment)");
      const manifest = JSON.parse(fs.readFileSync(path.join(packed, "manifest.json"), "utf8"));
      const qualification = readQualificationInput(process.env.QUALIFICATION, { sha: process.env.EXPECTED_SHA, registry: process.env.REGISTRY, units: words(process.env.UNITS) });
      const bound = bindPackedToQualification({ manifest, qualification, packed: (entry) => fs.readFileSync(path.join(packed, entry.file)), swapSha256: swapFile ? fs.readFileSync(swapFile, "utf8") : undefined,
        listing: (entry) => { const r = spawnSync("tar", ["-tzvf", path.join(packed, entry.file)], { encoding: "utf8" }); return r.stdout ?? ""; } });
      fs.writeFileSync(path.join(packed, "qualification-binding.json"), `${JSON.stringify({ schema: "rk-qualification-binding.v1", ...bound, qualification }, null, 2)}\n`, { flag: "wx" });
      process.stdout.write(`bound ${bound.unit}@${bound.version} ${bound.contentSha256 ? `content sha256 ${bound.contentSha256} (archive ${bound.sha256})` : `sha256 ${bound.sha256}`} to the local qualification at ${qualification.commit}\n`);
    } else if (command === "dispatch") {
      requireThat(flag("--unit") && rest.length === 2, "usage: publish-admission.mjs dispatch --unit <@rightkit/name>");
      const policy = await committedPolicy(root, (file) => fs.readFileSync(path.join(root, file), "utf8"));
      dispatchHosted({ unit: flag("--unit"), root, policy });
    } else refuse("usage: publish-admission.mjs admit --out <dir> | bind --packed <dir> | dispatch --unit <@rightkit/name>");
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
