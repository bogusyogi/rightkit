import { sha256, validateOwnershipFloor, verifyRepositoryRegistration } from "./ownership-approval.mjs";

const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype;
const text = (value) => typeof value === "string" && value.length > 0;
const digest = (value) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const fields = (value, required, optional = {}) => object(value) && Object.keys(value).every((key) => Object.hasOwn(required, key) || Object.hasOwn(optional, key))
  && Object.entries(required).every(([key, check]) => Object.hasOwn(value, key) && check(value[key]))
  && Object.entries(optional).every(([key, check]) => !Object.hasOwn(value, key) || check(value[key]));
const strings = (value) => Array.isArray(value) && value.every(text);
const nullableText = (value) => value === null || text(value);
const string = (value) => typeof value === "string";
const boolean = (value) => typeof value === "boolean";
const ordinal = (value) => Number.isSafeInteger(value) && value >= 0;
const version = (value) => text(value) && /^\d+(?:\.\d+)*$/.test(value);
const coverage = (value) => Array.isArray(value) && value.every((entry) => fields(entry, { path: text, status: text, reason: text }));
const finding = (value, extra = {}) => fields(value, { fingerprint: text, capability: text, package: text, file: text, line: ordinal }, {
  kind: (v) => ["source", "dependency"].includes(v), lineText: string, ordinal, text: string, occurrenceHash: digest,
  ecosystem: text, name: text, alias: nullableText, section: text, requirement: nullableText, features: strings,
  defaultFeatures: (v) => v === null || boolean(v), source: nullableText, resolvedVersion: nullableText, local: boolean, provenance: nullableText, qaOnly: boolean, ...extra,
});
const findings = (value) => Array.isArray(value) && value.every((entry) => finding(entry));
const provenance = (value, extra = {}) => fields(value, { kind: (v) => ["third-party", "generated"].includes(v), requestId: text, contentDigest: digest }, extra);
const contentDigests = (value) => Array.isArray(value) && value.every((entry) => fields(entry, { path: text, digest: (v) => digest(v) || typeof v === "string" && /^git:[a-f0-9]{40,64}$/.test(v) }, { provenance }));
const rows = (value) => Array.isArray(value) && value.every((entry) => fields(entry, { capability: text, owner: text, uses: boolean, status: text, count: ordinal }, { root: text }));
const metadata = (value) => fields(value, { repositoryIdentity: nullableText, scannerVersion: text, catalogVersion: text, catalogDigest: digest, coverage }, {
  commit: nullableText, tree: text, host: text, scannedAt: text, contentDigests, pathBase: text, pathPrefix: string,
  provenance: (v) => Array.isArray(v) && v.every((entry) => provenance(entry, { paths: strings })),
  exceptionDigest: digest, source: text, keyLogDigest: digest, keyCheckpoint: (v) => fields(v, { sequence: ordinal, head: digest }), rootFingerprint: digest,
  keyAuthority: (v) => fields(v, { repositoryIdentity: text, commit: text, tree: text }),
  roots: (v) => Array.isArray(v) && v.every((entry) => fields(entry, { root: text }, { metadata })),
});
const validReport = (value) => fields(value, { ok: (v) => typeof v === "boolean", enrollmentPending: (v) => typeof v === "boolean", violations: findings, problems: strings, coverage, metadata }, {
  findings, covered: (v) => Array.isArray(v) && v.every((entry) => finding(entry, { exception: object })), qaOnly: findings, rows, warnings: strings, warning: text,
});
const refuse = (code, problems) => ({ admit: false, code, problems });

/** One fail-closed contract for protected scan workers & broker admission. */
export function ownershipVerdict(report, trust) {
  if (!validReport(report)) return refuse("RK_OWNERSHIP_MALFORMED_REPORT", ["ownership report has missing, unknown, or wrong-typed fields"]);
  if (report.enrollmentPending) return refuse("RK_OWNERSHIP_PENDING_ENROLLMENT", ["protected ownership approval enrollment is pending", ...report.problems]);
  let registration;
  try {
    registration = verifyRepositoryRegistration(trust, report.metadata.repositoryIdentity, { files: [
      ...report.violations.map((entry) => entry.file), ...(report.findings ?? []).map((entry) => entry.file),
      ...(report.metadata.contentDigests ?? []).map((entry) => entry.path),
    ] });
  } catch { return refuse("RK_OWNERSHIP_INVALID_REGISTRATION", ["protected repository registry is missing or malformed"]); }
  if (!registration.ok) return refuse("RK_OWNERSHIP_INVALID_REGISTRATION", registration.problems);
  const scannedRoots = report.metadata.roots?.map((entry) => entry.root) ?? [report.metadata.pathPrefix?.replace(/\/$/, "") || "."];
  if (new Set(scannedRoots).size !== scannedRoots.length || sha256([...scannedRoots].sort()) !== sha256([...registration.registration.roots].sort()))
    return refuse("RK_OWNERSHIP_INVALID_REGISTRATION", ["scan roots do not exactly cover protected repository registration"]);
  const floor = validateOwnershipFloor(trust, report.metadata);
  if (!floor.ok) return refuse("RK_OWNERSHIP_POLICY_DOWNGRADE", floor.problems);
  const incomplete = [...report.coverage, ...report.metadata.coverage].filter((entry) => entry.status !== "complete");
  if (incomplete.length) return refuse("RK_OWNERSHIP_INCOMPLETE_COVERAGE", [...new Set(incomplete.map((entry) => entry.path + ": " + entry.reason))]);
  if (report.violations.length) return refuse("RK_OWNERSHIP_VIOLATION", report.violations.map((entry) => entry.capability + " (" + entry.fingerprint + " in " + entry.file + ") is owned by " + entry.package));
  if (report.problems.length) return refuse("RK_OWNERSHIP_UNVERIFIED", report.problems);
  if (!report.ok) return refuse("RK_OWNERSHIP_REPORT_FAILED", ["protected ownership scanner reported ok:false"]);
  return { admit: true, code: "RK_OWNERSHIP_CLEAN", problems: [] };
}

/** SHA-256 over every required authenticated cache binding; never directory or age. */
export function ownershipCacheKey(inputs) {
  if (!fields(inputs, { contentDigest: digest, catalogDigest: digest, trustDigest: digest, approvalsDigest: digest, scannerPolicyDigest: digest,
    protectedFloors: (value) => fields(value, { minimumScannerVersion: (v) => v === null || version(v), minimumCatalogVersion: (v) => v === null || version(v), minimumCatalogDigest: (v) => v === null || digest(v) }) }))
    throw Object.assign(new TypeError("ownership cache inputs have missing, unknown, or malformed digest/floor fields"), { code: "RK_OWNERSHIP_INVALID_INPUTS" });
  return sha256(inputs);
}
