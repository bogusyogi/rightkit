// GB-29: direct Linux origin trust (GATE-CONTRACT GC-L01). The verifier re-derives what it can from the commit and grants nothing for what it cannot; every adversarial record is refused by name.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { BROKER_ONLY_FIELDS, DIRECT_ORIGIN_REFUSED, DIRECT_ORIGIN_SCHEMA, buildDirectOrigin, candidateIdentity, directStageEvidence, reconcileClaim, verifyDirectOrigin } from "./direct-origin.mjs";
import { declaredLinuxTargets, pendingCandidate, resumeCandidate, runtimeCompatible, selectLinuxQualifier } from "./linux-qualifier.mjs";

const commit = "a".repeat(40), tree = "b".repeat(40), artifactSha256 = "c".repeat(64), contentSha256 = "d".repeat(64), policyDigest = "e".repeat(64);
const unit = { kind: "npm", name: "@rightkit/demo", version: "1.2.3" };
const repository = "bogusyogi/rightkit";
const expected = { repository, commit, tree, unit, registry: "npm", contentSha256 };
const stages = () => ({ compile: directStageEvidence({ stage: "compile", commit, tree, command: "cargo build --locked" }), package: directStageEvidence({ stage: "package", commit, tree, command: "pnpm pack", outputSha256: artifactSha256 }) });
const record = (over = {}) => ({ ...buildDirectOrigin({ repository, commit, tree, unit, registry: "npm", platform: "linux", artifact: { artifactSha256, contentSha256 }, stages: stages() }), ...over });
const refused = (r, pattern, over = expected) => assert.throws(() => verifyDirectOrigin(r, over), (e) => e.code === DIRECT_ORIGIN_REFUSED && pattern.test(e.message), String(pattern));

test("[GB-29] a direct Linux origin record whose every bound field equals the verifier's own value is admitted, and carries no broker field", () => {
  const r = record();
  assert.equal(r.schema, DIRECT_ORIGIN_SCHEMA);
  assert.deepEqual(r.executor, { kind: "direct", platform: "linux" });
  assert.equal(verifyDirectOrigin(r, expected).candidate, r.candidate);
  assert.equal(r.candidate, candidateIdentity({ repository, commit, tree, unit, registry: "npm", artifact: { artifactSha256, contentSha256 } }));
  assert.equal(JSON.stringify(r).includes("provenance"), false);
  // A crate is bound by its archive digest, which the verifier recomputes too.
  const crate = { kind: "cargo", name: "rightkit-demo", version: "0.1.0" };
  const c = buildDirectOrigin({ repository, commit, tree, unit: crate, registry: "crates", platform: "linux", artifact: { artifactSha256 }, stages: stages() });
  assert.ok(verifyDirectOrigin(c, { repository, commit, tree, unit: crate, registry: "crates", artifactSha256 }));
  refused(c, /not the .* the verifier recomputed/, { repository, commit, tree, unit: crate, registry: "crates", artifactSha256: "9".repeat(64) });
});

test("[GB-29] negative: a fabricated broker receipt, a broker executor, a Mac or Windows direct record and an unknown shape are refused", () => {
  for (const field of BROKER_ONLY_FIELDS) refused(record({ [field]: "x" }), new RegExp(`directOrigin\\.${field} is a broker receipt field`), expected);
  const nested = record(); nested.stages = nested.stages.map((s, i) => (i === 0 ? { ...s, provenance: { payload: {} } } : s));
  refused(nested, /stages\[0\]\.provenance is a broker receipt field/);
  refused(record({ executor: { kind: "broker", platform: "linux" } }), /not a direct Linux origin/);
  for (const platform of ["darwin", "win32"]) refused(record({ executor: { kind: "direct", platform } }), /Mac and Dell builds are broker-attested, never direct/);
  refused(record({ extra: 1 }), /unexpected or missing keys/);
  refused(record({ schema: "rightkit.direct-origin/2" }), /schema/);
  refused("not a record", /not an object/);
});

test("[GB-29] negative: a record for another repository, commit, tree, unit, version, registry or content is refused; so is a stale tree or a replayed candidate", () => {
  refused(record(), /repository/, { ...expected, repository: "someone/else" });
  refused(record(), /commit/, { ...expected, commit: "f".repeat(40) });
  refused(record(), /real tree/, { ...expected, tree: "f".repeat(40) });
  refused(record(), /covers/, { ...expected, unit: { ...unit, version: "9.9.9" } });
  refused(record(), /covers/, { ...expected, unit: { ...unit, name: "@rightkit/other" } });
  refused(record(), /registry/, { ...expected, registry: "crates" });
  refused(record(), /content sha256 .* recomputed/, { ...expected, contentSha256: "0".repeat(64) });
  refused(record(), /no recomputed content digest/, { ...expected, contentSha256: undefined });
  // The snapshot changed during a stage: the record is no evidence.
  const stale = record(); stale.stages[0] = { ...stale.stages[0], treeAtExit: "9".repeat(40) };
  refused(stale, /snapshot tree changed/);
  const otherTree = record(); otherTree.stages[1] = { ...otherTree.stages[1], tree: "9".repeat(40) };
  refused(otherTree, /another commit or tree/);
  const failed = record(); failed.stages[1] = { ...failed.stages[1], ok: false };
  refused(failed, /not a passed package/);
  const wrongOutput = record(); wrongOutput.stages[1] = { ...wrongOutput.stages[1], outputSha256: "1".repeat(64) };
  refused(wrongOutput, /not the record's artifact/);
  refused(record({ stages: [record().stages[0]] }), /stage records/);
  refused(record({ stages: [record().stages[1], record().stages[0]] }), /not a passed compile/);
  // An edited record no longer matches the identity its own fields give.
  refused(record({ artifact: { artifactSha256: "2".repeat(64), contentSha256 } }), /output is not the record's artifact|candidate identity/);
  refused(record({ candidate: "3".repeat(64) }), /candidate identity/);
});

test("[GB-29] the same candidate JOINS an existing claim; a different candidate or different bytes for one dispatch key is REJECTED, never replaced", () => {
  const r = record();
  const first = reconcileClaim(null, r);
  assert.equal(first.action, "claim");
  const existing = { key: first.key, candidate: r.candidate, artifactSha256 };
  assert.equal(reconcileClaim(existing, r).action, "join");
  const other = buildDirectOrigin({ repository, commit, tree, unit, registry: "npm", platform: "linux", artifact: { artifactSha256: "4".repeat(64), contentSha256 }, stages: { ...stages(), package: directStageEvidence({ stage: "package", commit, tree, command: "pnpm pack", outputSha256: "4".repeat(64) }) } });
  assert.throws(() => reconcileClaim(existing, other), (e) => e.code === DIRECT_ORIGIN_REFUSED && /conflicting bytes are rejected/.test(e.message));
  assert.throws(() => reconcileClaim({ ...existing, key: "0".repeat(64) }, r), /another dispatch key/);
});

const dell = { host: "dell", runtime: { kind: "wsl2", os: "linux", arch: "x86_64", libc: "glibc", available: true } };
const mac = { host: "mac", runtime: { kind: "vm", os: "linux", arch: "aarch64", libc: "glibc", available: true } };
const declared = (...targets) => ({ targets: { linux: targets } });

test("[GB-29] the private Linux qualifier exists only where a manifest declares a Linux target, is hosted on the Mac or Dell and never on CI", () => {
  assert.deepEqual(declaredLinuxTargets({}), []);
  assert.deepEqual(declaredLinuxTargets({ targets: { android: {} } }), [], "another target never implies Linux");
  assert.equal(selectLinuxQualifier({ config: {}, hosts: [dell, mac] }).state, "NOT_APPLICABLE");
  const one = selectLinuxQualifier({ config: declared({ arch: "x86_64", libc: "glibc" }), hosts: [mac, dell] });
  assert.equal(one.state, "QUALIFIER"); assert.deepEqual(one.assignments.map((a) => [a.host, a.runtime.kind]), [["dell", "wsl2"]]);
  const both = selectLinuxQualifier({ config: declared({ arch: "x86_64" }, { arch: "aarch64" }), hosts: [mac, dell] });
  assert.deepEqual(both.assignments.map((a) => a.host), ["dell", "mac"]);
  // Negative: an incompatible architecture or libc, an unavailable runtime, a CI host or no host at all leaves the candidate WAITING.
  for (const [config, hosts, pattern] of [
    [declared({ arch: "aarch64" }), [dell], /no hosted Linux runtime of dell:x86_64 is compatible with aarch64/],
    [declared({ arch: "x86_64", libc: "musl" }), [dell], /compatible with x86_64\/musl/],
    [declared({ arch: "x86_64" }), [{ host: "dell", runtime: { ...dell.runtime, available: false } }], /dell:x86_64 \(unavailable\)/],
    [declared({ arch: "x86_64" }), [{ host: "ci", runtime: dell.runtime }], /no Mac or Dell hosted Linux runtime is registered/],
    [declared({ arch: "x86_64" }), [], /no Mac or Dell hosted Linux runtime is registered/],
  ]) { const r = selectLinuxQualifier({ config, hosts }); assert.equal(r.state, "WAITING"); assert.match(r.missing[0].reason, pattern); }
  assert.throws(() => declaredLinuxTargets(declared({ arch: "riscv" })), /targets\.linux\[0\]/);
  assert.equal(runtimeCompatible({ ...dell.runtime, os: "darwin" }, { arch: "x86_64" }), false);
});

test("[GB-29] a pending Linux candidate resumes only on evidence that binds the same candidate, the exact sealed bytes, the pinned policy and every expected check; a refusal leaves it WAITING", () => {
  const target = { os: "linux", arch: "x86_64", libc: "glibc" };
  const pending = pendingCandidate({ candidate: "5".repeat(64), artifactSha256, target, policyDigest });
  const evidence = (over = {}) => ({ candidate: pending.candidate, artifactSha256, host: "dell", runtime: { kind: "wsl2", os: "linux", arch: "x86_64", libc: "glibc" }, policyDigest, expectedChecks: ["boot", "headless"], checks: [{ id: "boot", result: "pass" }, { id: "headless", result: "pass" }], ...over });
  const ok = resumeCandidate(pending, evidence());
  assert.equal(ok.state, "QUALIFIED"); assert.equal(ok.host, "dell");
  assert.equal(pending.state, "WAITING", "the pending record is never mutated");
  const bad = (over, pattern) => assert.throws(() => resumeCandidate(pending, evidence(over)), (e) => /RK_LINUX_QUALIFIER_REFUSED/.test(e.message) && pattern.test(e.message), JSON.stringify(Object.keys(over)));
  bad({ candidate: "6".repeat(64) }, /another candidate/);
  bad({ artifactSha256: "7".repeat(64) }, /exact sealed bytes are qualified, never a rebuild/);
  bad({ rebuilt: true }, /rebuilt the bytes/);
  bad({ host: "ci" }, /not the Mac or the Dell/);
  bad({ runtime: { kind: "vm", os: "linux", arch: "aarch64" } }, /not compatible with the declared target/);
  bad({ policyDigest: "8".repeat(64) }, /another policy/);
  bad({ expectedChecks: [] }, /empty mandatory suite/);
  bad({ checks: [{ id: "boot", result: "pass" }] }, /headless was not executed/);
  bad({ checks: [{ id: "boot", result: "pass" }, { id: "headless", result: "skipped" }] }, /headless is skipped/);
  bad({ checks: [{ id: "boot", result: "pass" }, { id: "headless", result: "fail" }] }, /headless is fail/);
  bad({ checks: [{ id: "boot", result: "pass" }, { id: "headless", result: "pass" }, { id: "extra", result: "pass" }] }, /does not expect/);
  assert.throws(() => resumeCandidate({ ...pending, state: "QUALIFIED" }, evidence()), /only a pending/);
  assert.throws(() => pendingCandidate({ candidate: "x", artifactSha256, policyDigest, target }), /names its candidate identity/);
});
