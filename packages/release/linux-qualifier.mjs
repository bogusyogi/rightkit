// GB-29 (GATE-CONTRACT GC-L02; P-L, A-L; E4): the private Linux qualifier, only where a manifest DECLARES a Linux target, and the pending candidate that waits for it.
//
// A private app qualifies on Mac or Dell, never on a mandatory GitHub CI path. When its manifest declares a Linux deliverable (`targets.linux`), the exact sealed Linux bytes the Linux origin
// produced are qualified in a COMPATIBLE Linux runtime (OS, architecture, libc) hosted by the Mac or the Dell: they are not rebuilt there. With no compatible runtime the candidate WAITS:
// a pending record keeps the candidate identity, the exact sealed digest and the pinned policy, nothing is discarded, and a later qualification resumes it only when it binds the same
// candidate, the same bytes, the same policy digest and every expected check passed. Nothing declared, nothing created: an app that declares no Linux target has no Linux artifact or qualification.
import { createHash } from "node:crypto";

export const PENDING_SCHEMA = "rightkit.pending-linux-candidate/1";
export const LINUX_QUALIFIER_REFUSED = "RK_LINUX_QUALIFIER_REFUSED";
const SHA256 = /^[0-9a-f]{64}$/;
const ARCHES = new Set(["x86_64", "aarch64"]);
const HOSTS = ["dell", "mac"];
const refuse = (message) => { throw Object.assign(new Error(`${LINUX_QUALIFIER_REFUSED}: ${message}`), { code: LINUX_QUALIFIER_REFUSED }); };
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/** The Linux targets a product manifest declares (`targets.linux`: one object or a list, each `{ arch, libc? }`); none when it declares none. Never inferred from another target. */
export function declaredLinuxTargets(config) {
  const declared = config?.targets?.linux;
  if (declared === undefined || declared === null) return [];
  const list = Array.isArray(declared) ? declared : [declared];
  return list.map((target, i) => {
    if (!isObject(target) || !ARCHES.has(target.arch) || (target.libc !== undefined && !["glibc", "musl"].includes(target.libc))) refuse(`targets.linux[${i}] must be { arch: x86_64|aarch64, libc?: glibc|musl }`);
    return { os: "linux", arch: target.arch, ...(target.libc ? { libc: target.libc } : {}) };
  });
}

/** Whether one hosted runtime can qualify one declared target: Linux, the same architecture, the same libc when the target names one, and available now. */
export function runtimeCompatible(runtime, target) {
  return isObject(runtime) && runtime.os === "linux" && runtime.available === true && runtime.arch === target.arch && (target.libc === undefined || runtime.libc === target.libc);
}

/**
 * The qualifier for each declared target, or the reason it waits. `hosts` is the Mac and Dell's hosted Linux runtimes `[{ host: "dell"|"mac", runtime: { kind, os, arch, libc?, available } }]`.
 * `{ state: "NOT_APPLICABLE" }` when nothing is declared; `{ state: "QUALIFIER", assignments }` when every declared target has a compatible hosted runtime (the Dell first, then the Mac, so the
 * choice is stable); `{ state: "WAITING", assignments, missing }` otherwise. Never CI, never a rebuild.
 */
export function selectLinuxQualifier({ config, hosts }) {
  const targets = declaredLinuxTargets(config);
  if (targets.length === 0) return { state: "NOT_APPLICABLE", reason: "the manifest declares no Linux target: no Linux artifact or qualification is created" };
  const ordered = [...(hosts ?? [])].filter((h) => HOSTS.includes(h?.host)).sort((a, b) => HOSTS.indexOf(a.host) - HOSTS.indexOf(b.host));
  const assignments = []; const missing = [];
  for (const target of targets) {
    const hit = ordered.find((h) => runtimeCompatible(h.runtime, target));
    if (hit) assignments.push({ target, host: hit.host, runtime: { kind: hit.runtime.kind, os: "linux", arch: hit.runtime.arch, ...(hit.runtime.libc ? { libc: hit.runtime.libc } : {}) } });
    else missing.push({ target, reason: ordered.length ? `no hosted Linux runtime of ${ordered.map((h) => `${h.host}:${h.runtime?.arch ?? "?"}${h.runtime?.available === true ? "" : " (unavailable)"}`).join(", ")} is compatible with ${target.arch}${target.libc ? `/${target.libc}` : ""}` : "no Mac or Dell hosted Linux runtime is registered" });
  }
  return missing.length ? { state: "WAITING", assignments, missing } : { state: "QUALIFIER", assignments };
}

const digest = (text) => createHash("sha256").update(text).digest("hex");

/** The pending record of a Linux candidate that has no qualifier yet: it keeps everything a later qualification must bind to, and discards nothing. */
export function pendingCandidate({ candidate, artifactSha256, target, policyDigest, missing }) {
  if (!SHA256.test(candidate ?? "") || !SHA256.test(artifactSha256 ?? "") || !SHA256.test(policyDigest ?? "")) refuse("a pending candidate names its candidate identity, sealed artifact digest and pinned policy digest");
  return { schema: PENDING_SCHEMA, state: "WAITING", candidate, artifactSha256, target, policyDigest, missing: missing ?? [] };
}

/**
 * Resume a pending candidate from a qualification run. Refuses (the pending record stays WAITING, unchanged) unless the evidence binds the same candidate and the exact sealed bytes, was
 * produced by a runtime compatible with the declared target on a Mac or Dell host, ran under the policy digest pinned before the run, rebuilt nothing, and executed every expected check with a
 * pass: a missing, skipped or failed check, an empty suite or a changed policy fails.
 */
export function resumeCandidate(pending, evidence) {
  if (!isObject(pending) || pending.schema !== PENDING_SCHEMA || pending.state !== "WAITING") refuse("only a pending (WAITING) Linux candidate can be resumed");
  if (!isObject(evidence)) refuse("the qualification evidence is not an object");
  if (evidence.candidate !== pending.candidate) refuse("the evidence is for another candidate");
  if (evidence.artifactSha256 !== pending.artifactSha256) refuse(`the evidence qualified ${String(evidence.artifactSha256).slice(0, 12)}, not the sealed ${pending.artifactSha256.slice(0, 12)}: the exact sealed bytes are qualified, never a rebuild`);
  if (evidence.rebuilt === true) refuse("the qualifier rebuilt the bytes: sealed Linux bytes are qualified unchanged");
  if (!HOSTS.includes(evidence.host)) refuse(`the qualifier host ${JSON.stringify(evidence.host)} is not the Mac or the Dell`);
  if (!runtimeCompatible({ ...evidence.runtime, available: true }, pending.target)) refuse("the qualifier's runtime is not compatible with the declared target");
  if (evidence.policyDigest !== pending.policyDigest) refuse("the qualification ran under another policy than the one pinned before the run");
  const expected = evidence.expectedChecks; const executed = evidence.checks;
  if (!Array.isArray(expected) || expected.length === 0) refuse("an empty mandatory suite is never a qualification");
  if (!Array.isArray(executed)) refuse("the evidence carries no executed checks");
  for (const id of expected) {
    const run = executed.filter((c) => c?.id === id);
    if (run.length !== 1) refuse(`expected check ${id} was ${run.length === 0 ? "not executed" : "executed more than once"}`);
    if (run[0].result !== "pass") refuse(`check ${id} is ${run[0].result}`);
  }
  if (executed.some((c) => !expected.includes(c?.id))) refuse("the evidence carries a check the pinned policy does not expect");
  return { state: "QUALIFIED", candidate: pending.candidate, artifactSha256: pending.artifactSha256, host: evidence.host, evidenceDigest: digest(JSON.stringify(evidence)) };
}
