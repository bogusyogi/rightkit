// GB-29 (GATE-CONTRACT GC-L01; R-L, A-L, P-L; E3, E4, E5): direct Linux origin trust, with no broker, no new service, no custom signature scheme and no fabricated broker receipt.
//
// A Linux origin runs the gate direct. Its compile and package stages leave DIRECT-ORIGIN records: the gate's own evidence (commit, the snapshot tree at start and at exit, the
// command identity, the output digest), never presented as, and never carrying the fields of, a broker's signed receipt. The record is NOT authority: it is a claim a verifier
// re-derives. The hosted side recomputes, from the dispatched commit alone, everything it can (the real git tree, the unit's version, the unit's content digest, its own full gate) and
// admits only a record whose every bound field equals ITS value; what it cannot recompute (that a local compile ran) is recorded as the origin's claim and grants nothing. The
// candidate identity (repository, commit, tree, unit, version, registry and the exact bytes) is what dispatch and publication join or reject on, so a replay of another candidate, a
// different artifact under one identity, a broker-shaped or non-Linux record, a stale tree or a digest the hosted pack does not reproduce are each refused by name.
//
// Activation is NOT here: widening the publish workflow's `admission` input beyond `broker` is the reserved publication authorization (GC-T22), so this verifier is the model and its tests,
// and the qualification a direct Linux origin writes (unit-gate.mjs) is verified by it locally exactly as the hosted side will, before anything could be dispatched.
import { createHash } from "node:crypto";

export const DIRECT_ORIGIN_SCHEMA = "rightkit.direct-origin/1";
/** The stages a direct origin evidences (the attested stages of a broker host). */
export const DIRECT_ORIGIN_STAGES = Object.freeze(["compile", "package"]);
export const DIRECT_ORIGIN_REFUSED = "RK_DIRECT_ORIGIN_REFUSED";
/** The repository whose units a direct origin releases. */
export const RIGHTKIT_REPOSITORY = "bogusyogi/rightkit";
/** Fields only a broker's signed job receipt or a broker request carries: a direct record that has any is a fabricated broker receipt and is refused. */
export const BROKER_ONLY_FIELDS = Object.freeze(["verdict", "refreshed", "reusedFrom", "provenance", "signature", "payload", "jobId", "requestId", "brokerVersion", "keyId", "nonce"]);
const RECORD_KEYS = ["schema", "executor", "repository", "commit", "tree", "unit", "registry", "artifact", "stages", "candidate"];
const STAGE_KEYS = ["stage", "ok", "commit", "tree", "treeAtStart", "treeAtExit", "command", "outputSha256"];
const SHA256 = /^[0-9a-f]{64}$/;
const SHA1 = /^[0-9a-f]{40}$/;

const sha256 = (text) => createHash("sha256").update(text).digest("hex");
const canonical = (value) => (Array.isArray(value) ? `[${value.map(canonical).join(",")}]` : value !== null && typeof value === "object" ? `{${Object.keys(value).sort().map((k) => `${JSON.stringify(k)}:${canonical(value[k])}`).join(",")}}` : JSON.stringify(value));
const refuse = (message) => { throw Object.assign(new Error(`${DIRECT_ORIGIN_REFUSED}: ${message}`), { code: DIRECT_ORIGIN_REFUSED }); };
const same = (a, b) => canonical(a) === canonical(b);
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/** The identity dispatch and publication join or reject on: what a candidate IS, independent of who built it or when. */
export function candidateIdentity({ repository, commit, tree, unit, registry, artifact }) {
  return sha256(canonical({ repository, commit, tree, unit: { kind: unit.kind, name: unit.name, version: unit.version }, registry, artifactSha256: artifact.artifactSha256, contentSha256: artifact.contentSha256 ?? null }));
}

/** One stage's evidence on a direct origin: the gate's own facts, taken after the stage guard confirmed the snapshot was the commit's bytes at start and at exit. */
export function directStageEvidence({ stage, commit, tree, treeAtStart = tree, treeAtExit = tree, command, outputSha256 = null }) {
  return { stage, ok: true, commit, tree, treeAtStart, treeAtExit, command, outputSha256 };
}

/** The record a direct Linux origin writes into its qualification (`directOrigin`). `stages` is `{ compile, package }` of directStageEvidence. */
export function buildDirectOrigin({ repository, commit, tree, unit, registry, platform, artifact, stages }) {
  const record = { schema: DIRECT_ORIGIN_SCHEMA, executor: { kind: "direct", platform }, repository, commit, tree, unit: { kind: unit.kind, name: unit.name, version: unit.version }, registry, artifact: { artifactSha256: artifact.artifactSha256, contentSha256: artifact.contentSha256 ?? null }, stages: DIRECT_ORIGIN_STAGES.map((stage) => stages[stage]) };
  return { ...record, candidate: candidateIdentity({ repository, commit, tree, unit, registry, artifact: record.artifact }) };
}

function noBrokerFields(value, where) {
  if (Array.isArray(value)) { value.forEach((item, i) => noBrokerFields(item, `${where}[${i}]`)); return; }
  if (!isObject(value)) return;
  for (const [key, inner] of Object.entries(value)) {
    if (BROKER_ONLY_FIELDS.includes(key)) refuse(`${where}.${key} is a broker receipt field: a direct origin has no broker and a direct record never carries or imitates one`);
    noBrokerFields(inner, `${where}.${key}`);
  }
}

/**
 * The verifier (hosted side, and the origin's own self-check). `expected` is what the verifier derives independently:
 * `{ repository, commit, tree (the real git tree of the commit), unit: { kind, name, version }, registry, contentSha256 (the unit's content digest recomputed from the commit; required for an
 * npm unit), artifactSha256 (a crate's archive digest, when recomputed) }`. Throws RK_DIRECT_ORIGIN_REFUSED naming the first thing that is not so; returns `{ candidate }`.
 */
export function verifyDirectOrigin(record, expected) {
  if (!isObject(record)) refuse("the direct-origin record is not an object");
  noBrokerFields(record, "directOrigin");
  if (!same(Object.keys(record).sort(), [...RECORD_KEYS].sort())) refuse(`the record has unexpected or missing keys (${Object.keys(record).sort().join(", ")})`);
  if (record.schema !== DIRECT_ORIGIN_SCHEMA) refuse(`schema ${JSON.stringify(record.schema)} is not ${DIRECT_ORIGIN_SCHEMA}`);
  // Mac and the Dell always run through their broker; a direct record is a Linux origin's and nothing else's.
  if (!isObject(record.executor) || !same(Object.keys(record.executor).sort(), ["kind", "platform"]) || record.executor.kind !== "direct" || record.executor.platform !== "linux") refuse(`the executor ${JSON.stringify(record.executor)} is not a direct Linux origin (Mac and Dell builds are broker-attested, never direct)`);
  if (record.repository !== expected.repository) refuse(`the record is for repository ${record.repository}, not ${expected.repository}`);
  if (!SHA1.test(record.commit ?? "") || record.commit !== expected.commit) refuse(`the record is for commit ${String(record.commit).slice(0, 12)}, not ${String(expected.commit).slice(0, 12)}`);
  if (!SHA1.test(record.tree ?? "") || record.tree !== expected.tree) refuse(`the record's tree ${String(record.tree).slice(0, 12)} is not the commit's real tree ${String(expected.tree).slice(0, 12)}`);
  if (!isObject(record.unit) || record.unit.kind !== expected.unit.kind || record.unit.name !== expected.unit.name || record.unit.version !== expected.unit.version) refuse(`the record covers ${JSON.stringify(record.unit)}, not ${JSON.stringify(expected.unit)}`);
  if (record.registry !== expected.registry) refuse(`the record is for registry ${record.registry}, not ${expected.registry}`);
  const artifact = record.artifact;
  if (!isObject(artifact) || !SHA256.test(artifact.artifactSha256 ?? "")) refuse("the record names no artifact sha256");
  if (expected.unit.kind === "npm") {
    if (!SHA256.test(expected.contentSha256 ?? "")) refuse("the verifier has no recomputed content digest for this npm unit");
    if (artifact.contentSha256 !== expected.contentSha256) refuse(`the record's content sha256 ${artifact.contentSha256} is not the ${expected.contentSha256} the verifier recomputed from the commit`);
  } else if (expected.artifactSha256 !== undefined && artifact.artifactSha256 !== expected.artifactSha256) refuse(`the record's artifact sha256 ${artifact.artifactSha256} is not the ${expected.artifactSha256} the verifier recomputed`);
  if (!Array.isArray(record.stages) || record.stages.length !== DIRECT_ORIGIN_STAGES.length) refuse(`the record carries ${Array.isArray(record.stages) ? record.stages.length : "no"} stage records, not ${DIRECT_ORIGIN_STAGES.join(" and ")}`);
  record.stages.forEach((stage, i) => {
    const name = DIRECT_ORIGIN_STAGES[i];
    if (!isObject(stage) || !same(Object.keys(stage).sort(), [...STAGE_KEYS].sort())) refuse(`the ${name} stage record has unexpected or missing keys`);
    if (stage.stage !== name || stage.ok !== true) refuse(`stage ${i + 1} is ${JSON.stringify(stage.stage)} ok=${stage.ok}, not a passed ${name}`);
    if (stage.commit !== expected.commit || stage.tree !== expected.tree) refuse(`the ${name} stage is for another commit or tree`);
    // The snapshot was the commit's bytes when the stage started and when it ended: a stale or mutated tree is no evidence.
    if (stage.treeAtStart !== expected.tree || stage.treeAtExit !== expected.tree) refuse(`the ${name} stage's snapshot tree changed (start ${String(stage.treeAtStart).slice(0, 12)}, exit ${String(stage.treeAtExit).slice(0, 12)}, commit ${expected.tree.slice(0, 12)})`);
    if (typeof stage.command !== "string" || !stage.command) refuse(`the ${name} stage names no command`);
    if (name === "package" && stage.outputSha256 !== artifact.artifactSha256) refuse("the package stage's output is not the record's artifact");
  });
  const candidate = candidateIdentity({ repository: expected.repository, commit: expected.commit, tree: expected.tree, unit: expected.unit, registry: expected.registry, artifact });
  if (record.candidate !== candidate) refuse("the record's candidate identity is not the one its own fields give (a replayed or edited record)");
  return { candidate };
}

/**
 * Dispatch and publication join or reject on the candidate identity (GC-T21): the same identity and bytes JOIN the existing claim; a different candidate or different bytes for the same
 * dispatch key (repository, commit, unit, version, registry) REJECT; no claim is created here. `existing` is the claim already held for the key, or null.
 */
export function reconcileClaim(existing, record) {
  const key = sha256(canonical({ repository: record.repository, commit: record.commit, unit: record.unit, registry: record.registry }));
  if (existing === null || existing === undefined) return { action: "claim", key, candidate: record.candidate };
  if (existing.key !== key) refuse("the existing claim is for another dispatch key");
  if (existing.candidate === record.candidate && existing.artifactSha256 === record.artifact.artifactSha256) return { action: "join", key, candidate: record.candidate };
  refuse(`a claim for this dispatch key already binds candidate ${String(existing.candidate).slice(0, 12)} (artifact ${String(existing.artifactSha256).slice(0, 12)}); conflicting bytes are rejected, never replaced`);
}
