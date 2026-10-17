// Case definitions for the ownership contract fixtures. Pure data: generate.mjs runs them
// through the CURRENT Node implementation and records the output as expected.json.
// Signed-approval cases are deliberately absent (see CONTRACT.md, "Uncovered").
import { createHash } from "node:crypto";

export const IDENT = "https://github.com/test/repo";
export const NOW = "2026-10-09T00:00:00.000Z";
export const SCANNER_POLICY_DIGEST = createHash("sha256").update("rightkit-ownership-fixture-scanner-policy-v1").digest("hex");
const H = (c) => c.repeat(64);

// ---------------------------------------------------------------- catalog
export const CATALOG = {
  schema: 1, version: "1", capabilities: [
    { package: "rightkit-http", capability: "http-client", tier: "block", fingerprints: [
      { id: "http.reqwest", type: "dependency", ecosystem: "cargo", name: "reqwest" },
      { id: "http.axios", type: "dependency", ecosystem: "npm", name: "axios" },
      { id: "http.fetch-call", type: "source", pattern: "\\bhttp_fetch\\(", extensions: [".rs"] },
      { id: "http.raw-socket", type: "source", pattern: "TcpStream::connect", codeOnly: true, extensions: [".rs"] },
    ] },
    { package: "rightkit-log", capability: "logging", tier: "advise", fingerprints: [
      { id: "log.println", type: "source", pattern: "\\bprintln!\\(", extensions: [".rs"] },
    ] },
    { package: "rightkit-sandbox", capability: "sandbox-exec", tier: "advise", fingerprints: [
      { id: "sandbox.danger-eval", type: "source", pattern: "\\bdanger_eval\\(", policy: "banned", extensions: [".rs"] },
    ] },
    { package: "rightkit-ui", capability: "ui-shell", fingerprints: [], detection: "declaration-only" },
  ],
};

// ---------------------------------------------------------------- trust
export function baseTrust(extra = {}) {
  // TEST ONLY placeholders: no key material. The committed scan re-derives enrollment from
  // trust.hosts (distinct, non-revoked keyId + publicKeyPem), so a clean pass needs both entries.
  return { schema: 1, revokedKeyIds: [], verifiedTrackingEvidence: [], minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1",
    repositories: [{ repositoryIdentity: IDENT, roots: ["."], visibility: "private" }],
    hosts: { windows: { keyId: "win-" + "1".repeat(64), publicKeyPem: "TEST-ONLY-PLACEHOLDER-WINDOWS" }, macos: { keyId: "mac-" + "2".repeat(64), publicKeyPem: "TEST-ONLY-PLACEHOLDER-MACOS" } }, ...extra };
}
const state = (trust, extra = {}) => ({ trust, enrolled: true, pending: false, protection: { ok: true, mechanism: "fixture" }, ...extra });

// ---------------------------------------------------------------- verdict-level helpers
const vTrust = baseTrust;
const vMeta = (o = {}) => ({ repositoryIdentity: IDENT, scannerVersion: "2.6.0", catalogVersion: "1", catalogDigest: H("a"), coverage: [], roots: [{ root: "." }], ...o });
export const vRep = (o = {}) => ({ ok: true, enrollmentPending: false, violations: [], problems: [], coverage: [], metadata: vMeta(), ...o });
const src = (o = {}) => ({ fingerprint: "http.fetch-call", capability: "http-client", package: "rightkit-http", tier: "block", file: "src/main.rs", line: 2, kind: "source", lineText: "    http_fetch();", ordinal: 0, text: "http_fetch();", occurrenceHash: H("b"), ...o });
const dep = (o = {}) => ({ fingerprint: "http.reqwest", capability: "http-client", package: "rightkit-http", tier: "block", file: "Cargo.toml", line: 0, kind: "dependency", ecosystem: "cargo", name: "reqwest", alias: null, section: "dependencies", requirement: "0.12", features: [], defaultFeatures: null, local: false, source: "registry", resolvedVersion: "0.12.4", text: "cargo dependency reqwest", provenance: "registry", occurrenceHash: H("c"), ...o });
const cov = (path_, reason, extra = {}) => ({ path: path_, status: "unscanned", reason, ...extra });
const without = (o, k) => { const c = structuredClone(o); delete c[k]; return c; };
const noRoots = (m) => without(m, "roots");
const ADVISE = { tier: "advise", capability: "logging", package: "rightkit-log", fingerprint: "log.println" };

export function verdictCases() {
  const v = (name, expectCode, description, report, trust = vTrust()) => [name, { kind: "verdict", description, expectCode, report, trust }];
  const list = [
    v("verdict-clean", "RK_OWNERSHIP_CLEAN", "No findings, complete coverage, registered root.", vRep()),
    v("verdict-clean-with-optional-fields", "RK_OWNERSHIP_CLEAN", "Optional findings/covered/rows/warning(s) fields are accepted.", vRep({ findings: [src()], covered: [{ ...src(), exception: { schema: 2 } }], rows: [{ capability: "http-client", owner: "rightkit-http", uses: false, status: "excepted", count: 1, root: "." }], warnings: ["w"], warning: "w" })),
    v("verdict-advisory-only", "RK_OWNERSHIP_ADVISORY", "Only advise-tier violations: admit true, code ADVISORY, problems list the advisories.", vRep({ violations: [src(ADVISE)] })),
    v("verdict-violation-block-tier", "RK_OWNERSHIP_VIOLATION", "Block-tier source violation.", vRep({ ok: false, violations: [src()] })),
    v("verdict-violation-banned-advise-tier", "RK_OWNERSHIP_VIOLATION", "policy:banned blocks even when tier is advise.", vRep({ ok: false, violations: [src({ tier: "advise", policy: "banned", fingerprint: "sandbox.danger-eval", capability: "sandbox-exec", package: "rightkit-sandbox" })] })),
    v("verdict-violation-missing-tier", "RK_OWNERSHIP_VIOLATION", "A finding with no tier is blocking (fail closed).", vRep({ ok: false, violations: [without(src(), "tier")] })),
    v("verdict-violation-dependency", "RK_OWNERSHIP_VIOLATION", "Direct dependency violation shape.", vRep({ ok: false, violations: [dep()] })),
    v("verdict-violation-transitive", "RK_OWNERSHIP_VIOLATION", "Valid transitive-dependency finding shape.", vRep({ ok: false, violations: [{ fingerprint: "http.reqwest", capability: "http-client", package: "rightkit-http", tier: "block", file: "Cargo.lock", line: 0, kind: "transitive-dependency", ecosystem: "cargo", name: "reqwest", lockfile: "Cargo.lock", dependencyPath: ["fixture-app", "wrapper", "reqwest"], source: "registry+https://github.com/rust-lang/crates.io-index", resolvedVersion: "0.12.4", text: "cargo dependency reqwest", provenance: "registry+https://github.com/rust-lang/crates.io-index", occurrenceHash: H("c") }] })),
    v("verdict-violation-mixed-advisory-and-block", "RK_OWNERSHIP_VIOLATION", "Block finding wins over advisory finding; advisory not listed in problems.", vRep({ ok: false, violations: [src(ADVISE), src()] })),
    v("verdict-pending-enrollment", "RK_OWNERSHIP_UNVERIFIED", "Pending enrollment is only a warning: report problems still refuse as unverified.", vRep({ ok: false, enrollmentPending: true, problems: ["diag one"] }), vTrust({ repositories: [] })),
    v("verdict-pending-beats-violation", "RK_OWNERSHIP_VIOLATION", "Pending enrollment never hides a violation: the finding gate still refuses.", vRep({ ok: false, enrollmentPending: true, violations: [src()] })),
    v("verdict-malformed-null-report", "RK_OWNERSHIP_MALFORMED_REPORT", "Report is null.", null),
    v("verdict-malformed-array-report", "RK_OWNERSHIP_MALFORMED_REPORT", "Report is an array.", []),
    v("verdict-malformed-string-report", "RK_OWNERSHIP_MALFORMED_REPORT", "Report is a string.", "clean"),
    v("verdict-malformed-number-report", "RK_OWNERSHIP_MALFORMED_REPORT", "Report is a number.", 1),
    v("verdict-malformed-unknown-top-field", "RK_OWNERSHIP_MALFORMED_REPORT", "Unknown top-level field is rejected.", vRep({ extra: true })),
    v("verdict-malformed-wrong-type-ok", "RK_OWNERSHIP_MALFORMED_REPORT", "ok must be boolean.", vRep({ ok: "false" })),
    v("verdict-malformed-wrong-type-enrollment", "RK_OWNERSHIP_MALFORMED_REPORT", "enrollmentPending must be boolean.", vRep({ enrollmentPending: "false" })),
    v("verdict-malformed-wrong-type-violations", "RK_OWNERSHIP_MALFORMED_REPORT", "violations must be an array.", vRep({ violations: 1 })),
    v("verdict-malformed-wrong-type-problems", "RK_OWNERSHIP_MALFORMED_REPORT", "problems must be an array of non-empty strings.", vRep({ problems: [1] })),
    v("verdict-malformed-wrong-type-coverage", "RK_OWNERSHIP_MALFORMED_REPORT", "coverage must be an array.", vRep({ coverage: 1 })),
    v("verdict-malformed-wrong-type-metadata", "RK_OWNERSHIP_MALFORMED_REPORT", "metadata must be an object.", vRep({ metadata: 1 })),
    ...["ok", "enrollmentPending", "violations", "problems", "coverage", "metadata"].map((key) => v(`verdict-malformed-missing-${key}`, "RK_OWNERSHIP_MALFORMED_REPORT", `Required field ${key} is missing.`, without(vRep(), key))),
    v("verdict-malformed-metadata-extra-field", "RK_OWNERSHIP_MALFORMED_REPORT", "metadata rejects unknown fields.", vRep({ metadata: vMeta({ extra: true }) })),
    v("verdict-malformed-bad-catalog-digest", "RK_OWNERSHIP_MALFORMED_REPORT", "catalogDigest must be 64 lowercase hex.", vRep({ metadata: vMeta({ catalogDigest: "XYZ" }) })),
    v("verdict-malformed-null-coverage-entry", "RK_OWNERSHIP_MALFORMED_REPORT", "Coverage entries must be objects.", vRep({ coverage: [null] })),
    v("verdict-malformed-coverage-entry-fields", "RK_OWNERSHIP_MALFORMED_REPORT", "Coverage entries require path,status,reason.", vRep({ coverage: [{ path: "x" }] })),
    v("verdict-malformed-null-violation", "RK_OWNERSHIP_MALFORMED_REPORT", "Violation entries must be objects.", vRep({ violations: [null] })),
    v("verdict-malformed-finding-unknown-field", "RK_OWNERSHIP_MALFORMED_REPORT", "Findings reject unknown fields.", vRep({ ok: false, violations: [src({ surprise: true })] })),
    v("verdict-malformed-finding-bad-tier", "RK_OWNERSHIP_MALFORMED_REPORT", "tier must be block|advise.", vRep({ violations: [src({ tier: "warn" })] })),
    v("verdict-malformed-transitive-path-mismatch", "RK_OWNERSHIP_MALFORMED_REPORT", "Transitive finding needs lockfile===file and dependencyPath ending in name.", vRep({ ok: false, violations: [dep({ kind: "transitive-dependency", file: "Cargo.lock", lockfile: "Other.lock", dependencyPath: ["a", "b", "reqwest"] })] })),
    v("verdict-malformed-rows-entry", "RK_OWNERSHIP_MALFORMED_REPORT", "rows entries are validated when present.", vRep({ rows: [{ capability: 1 }] })),
    v("verdict-malformed-content-digest-type", "RK_OWNERSHIP_MALFORMED_REPORT", "contentDigests[].digest must be a string.", vRep({ metadata: vMeta({ contentDigests: [{ path: "source.rs", digest: 1 }] }) })),
    v("verdict-content-digest-git-form-ok", "RK_OWNERSHIP_CLEAN", "contentDigests accept sha256 hex or git:<40-64 hex>.", vRep({ metadata: vMeta({ contentDigests: [{ path: "a.rs", digest: "git:" + "a".repeat(40) }, { path: "b.rs", digest: H("b") }] }) })),
    v("verdict-registration-unregistered-repo", "RK_OWNERSHIP_INVALID_REGISTRATION", "Repository identity not in protected registry.", vRep({ metadata: vMeta({ repositoryIdentity: "https://github.com/test/other" }) })),
    v("verdict-registration-null-identity", "RK_OWNERSHIP_INVALID_REGISTRATION", "Null repositoryIdentity cannot be registered.", vRep({ metadata: vMeta({ repositoryIdentity: null }) })),
    v("verdict-registration-null-trust", "RK_OWNERSHIP_INVALID_REGISTRATION", "Missing trust registry.", vRep(), null),
    v("verdict-registration-empty-registry", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registry with no repositories.", vRep(), vTrust({ repositories: [] })),
    v("verdict-registration-null-registry-entry", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registry entry that is null.", vRep(), vTrust({ repositories: [null] })),
    v("verdict-registration-malformed-roots", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registry roots must be exact paths.", vRep(), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["a/../b"], visibility: "private" }] })),
    v("verdict-registration-bad-visibility", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registry visibility must be public|private.", vRep(), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["."], visibility: "secret" }] })),
    v("verdict-registration-noncanonical-registry-identity", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registry identity must already be canonical.", vRep(), vTrust({ repositories: [{ repositoryIdentity: "https://GitHub.com/Test/Repo", roots: ["."], visibility: "private" }] })),
    v("verdict-registration-trust-bad-schema", "RK_OWNERSHIP_INVALID_REGISTRATION", "trust.schema must be 1.", vRep(), vTrust({ schema: 2 })),
    v("verdict-registration-engine-app-roots-mismatch", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registered roots engine+app but only '.' scanned.", vRep(), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["engine", "app"], visibility: "private" }] })),
    v("verdict-registration-path-outside-roots", "RK_OWNERSHIP_INVALID_REGISTRATION", "Violation file outside registered root.", vRep({ ok: false, violations: [src({ file: "other/main.rs" })], metadata: vMeta({ roots: [{ root: "app" }] }) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["app"], visibility: "private" }] })),
    v("verdict-registration-backslash-path", "RK_OWNERSHIP_INVALID_REGISTRATION", "Windows separators in a finding path are malformed (the verdict does not normalise).", vRep({ ok: false, violations: [src({ file: "src\\main.rs" })] })),
    v("verdict-registration-absolute-path", "RK_OWNERSHIP_INVALID_REGISTRATION", "Absolute finding path is malformed.", vRep({ ok: false, violations: [src({ file: "/etc/main.rs" })] })),
    v("verdict-registration-dotdot-path", "RK_OWNERSHIP_INVALID_REGISTRATION", "Dot-segment finding path is malformed.", vRep({ ok: false, violations: [src({ file: "src/../main.rs" })] })),
    v("verdict-registration-glob-path", "RK_OWNERSHIP_INVALID_REGISTRATION", "Glob characters in a finding path are malformed.", vRep({ ok: false, violations: [src({ file: "src/*.rs" })] })),
    v("verdict-registration-content-digest-path-outside", "RK_OWNERSHIP_INVALID_REGISTRATION", "contentDigests paths are also checked against registered roots.", vRep({ metadata: vMeta({ roots: [{ root: "app" }], contentDigests: [{ path: "other/a.rs", digest: H("b") }] }) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["app"], visibility: "private" }] })),
    v("verdict-registration-findings-path-outside", "RK_OWNERSHIP_INVALID_REGISTRATION", "report.findings (not only violations) paths are checked.", vRep({ findings: [src({ file: "other/main.rs" })], metadata: vMeta({ roots: [{ root: "app" }] }) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["app"], visibility: "private" }] })),
    v("verdict-registration-roots-mismatch", "RK_OWNERSHIP_INVALID_REGISTRATION", "Scanned roots must exactly equal the registered roots.", vRep({ metadata: vMeta({ roots: [{ root: "a" }] }) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["a", "b"], visibility: "private" }] })),
    v("verdict-registration-duplicate-scanned-root", "RK_OWNERSHIP_INVALID_REGISTRATION", "Duplicate scanned roots fail.", vRep({ metadata: vMeta({ roots: [{ root: "a" }, { root: "a" }] }) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["a"], visibility: "private" }] })),
    v("verdict-registration-multi-root-ok", "RK_OWNERSHIP_CLEAN", "Two registered roots scanned in any order.", vRep({ metadata: vMeta({ roots: [{ root: "b" }, { root: "a" }] }) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["a", "b"], visibility: "public" }] })),
    v("verdict-registration-identity-noncanonical-in-report-ok", "RK_OWNERSHIP_CLEAN", "metadata.repositoryIdentity is canonicalised before lookup (scp spelling matches).", vRep({ metadata: vMeta({ repositoryIdentity: "git@GitHub.com:Test/Repo.git" }) })),
    v("verdict-registration-legacy-pathprefix-root", "RK_OWNERSHIP_CLEAN", "Without metadata.roots the single scanned root is derived from pathPrefix (trailing slash stripped).", vRep({ metadata: noRoots(vMeta({ pathPrefix: "app/" })) }), vTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["app"], visibility: "private" }] })),
    v("verdict-registration-legacy-no-prefix-root", "RK_OWNERSHIP_CLEAN", "Without roots and pathPrefix the scanned root is '.'.", vRep({ metadata: noRoots(vMeta()) })),
    v("verdict-downgrade-scanner-floor", "RK_OWNERSHIP_POLICY_DOWNGRADE", "scannerVersion below trust.minimumScannerVersion.", vRep(), vTrust({ minimumScannerVersion: "99.0.0" })),
    v("verdict-downgrade-catalog-floor", "RK_OWNERSHIP_POLICY_DOWNGRADE", "catalogVersion below trust.minimumCatalogVersion.", vRep(), vTrust({ minimumCatalogVersion: "99" })),
    v("verdict-downgrade-catalog-digest-pin", "RK_OWNERSHIP_POLICY_DOWNGRADE", "catalogDigest differs from trust.minimumCatalogDigest.", vRep(), vTrust({ minimumCatalogDigest: H("d") })),
    v("verdict-downgrade-catalog-digest-pin-match-ok", "RK_OWNERSHIP_CLEAN", "catalogDigest equals trust.minimumCatalogDigest.", vRep(), vTrust({ minimumCatalogDigest: H("a") })),
    v("verdict-downgrade-malformed-version", "RK_OWNERSHIP_POLICY_DOWNGRADE", "A non-numeric scanner version never satisfies the floor.", vRep({ metadata: vMeta({ scannerVersion: "2.x" }) })),
    v("verdict-downgrade-multi-segment-version-ok", "RK_OWNERSHIP_CLEAN", "Version compare is numeric per dotted segment (2.10 > 2.9), missing segments are 0.", vRep({ metadata: vMeta({ scannerVersion: "2.10" }) }), vTrust({ minimumScannerVersion: "2.9.0" })),
    v("verdict-downgrade-equal-version-ok", "RK_OWNERSHIP_CLEAN", "Equal versions satisfy the floor; trailing zero segments are equal.", vRep({ metadata: vMeta({ scannerVersion: "2.0" }) }), vTrust({ minimumScannerVersion: "2.0.0" })),
    v("verdict-coverage-report-level", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "report.coverage has a non-complete entry.", vRep({ ok: false, coverage: [cov("native.bin", "opaque")] })),
    v("verdict-coverage-metadata-only", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "metadata.coverage alone is enough.", vRep({ ok: false, metadata: vMeta({ coverage: [cov("native.bin", "opaque")] }) })),
    v("verdict-coverage-incomplete-status", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Any status other than 'complete' blocks (not only 'unscanned').", vRep({ metadata: vMeta({ coverage: [{ path: "source.rs", status: "incomplete", reason: "missing" }] }) })),
    v("verdict-coverage-dedup", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Same path: reason in both lists is reported once; detail is not part of the message.", vRep({ ok: false, coverage: [cov("a", "r")], metadata: vMeta({ coverage: [cov("a", "r"), cov("b", "s", { detail: "d" })] }) })),
    v("verdict-coverage-complete-status-ok", "RK_OWNERSHIP_CLEAN", "status 'complete' entries do not block.", vRep({ coverage: [{ path: "a", status: "complete", reason: "scanned" }] })),
    v("verdict-coverage-beats-violation", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Coverage gap outranks violations.", vRep({ ok: false, coverage: [cov("a", "r")], violations: [src()] })),
    v("verdict-downgrade-beats-coverage", "RK_OWNERSHIP_POLICY_DOWNGRADE", "Floor check outranks coverage.", vRep({ ok: false, coverage: [cov("a", "r")] }), vTrust({ minimumScannerVersion: "99" })),
    v("verdict-registration-beats-downgrade", "RK_OWNERSHIP_INVALID_REGISTRATION", "Registration outranks floor check.", vRep({ metadata: vMeta({ repositoryIdentity: "https://github.com/test/other" }) }), vTrust({ minimumScannerVersion: "99" })),
    v("verdict-unverified-problems", "RK_OWNERSHIP_UNVERIFIED", "Non-empty problems with no blocking violation.", vRep({ ok: false, problems: ["approval check failed"] })),
    v("verdict-unverified-advisory-plus-problems", "RK_OWNERSHIP_UNVERIFIED", "Problems outrank advisory admission.", vRep({ ok: false, problems: ["p"], violations: [src(ADVISE)] })),
    v("verdict-violation-beats-problems", "RK_OWNERSHIP_VIOLATION", "Violation outranks problems.", vRep({ ok: false, problems: ["p"], violations: [src()] })),
    v("verdict-report-failed", "RK_OWNERSHIP_REPORT_FAILED", "ok:false with no problems, coverage or blocking findings.", vRep({ ok: false })),
    v("verdict-ok-false-with-advisory-is-report-failed", "RK_OWNERSHIP_REPORT_FAILED", "ok:false is checked before the advisory branch.", vRep({ ok: false, violations: [src(ADVISE)] })),
    v("verdict-pending-null-trust", "RK_OWNERSHIP_REPORT_FAILED", "Pending enrollment with no trust skips the protected registry checks; a report with ok:false still refuses.", vRep({ ok: false, enrollmentPending: true }), null),
    v("verdict-ordinal-negative-line", "RK_OWNERSHIP_MALFORMED_REPORT", "line must be a non-negative safe integer.", vRep({ ok: false, violations: [src({ line: -1 })] })),
    v("verdict-ordinal-fractional-line", "RK_OWNERSHIP_MALFORMED_REPORT", "line must be an integer.", vRep({ ok: false, violations: [src({ line: 1.5 })] })),
    v("verdict-ordinal-unsafe-line", "RK_OWNERSHIP_MALFORMED_REPORT", "line above Number.MAX_SAFE_INTEGER is malformed.", vRep({ ok: false, violations: [src({ line: 9007199254740992 })] })),
    v("verdict-ordinal-max-safe-line-ok", "RK_OWNERSHIP_VIOLATION", "line == Number.MAX_SAFE_INTEGER is accepted.", vRep({ ok: false, violations: [src({ line: 9007199254740991 })] })),
    v("verdict-ordinal-integral-float-note", "RK_OWNERSHIP_VIOLATION", "JSON numbers such as 2.0 or 2e0 parse to the integer 2 in JavaScript; consumers must accept integral floats (input.json is normalised, so the text form is documented in CONTRACT.md only).", vRep({ ok: false, violations: [src({ line: 2 })] })),
  ];
  return list;
}

export function cacheKeyCases() {
  const inputs = (o = {}) => ({ contentDigest: H("1"), catalogDigest: H("2"), trustDigest: H("3"), approvalsDigest: H("4"), scannerPolicyDigest: H("5"), protectedFloors: { minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1", minimumCatalogDigest: null }, ...o });
  const c = (name, expect, description, value) => [name, { kind: "cache-key", description, expectCode: expect, inputs: value }];
  const list = [
    c("cache-key-valid", "OK", "All six bindings present.", inputs()),
    c("cache-key-null-floors", "OK", "Floors may all be null.", inputs({ protectedFloors: { minimumScannerVersion: null, minimumCatalogVersion: null, minimumCatalogDigest: null } })),
    c("cache-key-field-order-independent", "OK", "Key order does not change the key (canonical JSON, sorted keys).", { scannerPolicyDigest: H("5"), protectedFloors: { minimumCatalogDigest: H("6"), minimumCatalogVersion: "3.0.0", minimumScannerVersion: "2.6.0" }, approvalsDigest: H("4"), trustDigest: H("3"), catalogDigest: H("2"), contentDigest: H("1") }),
    c("cache-key-field-order-reference", "OK", "Same values as cache-key-field-order-independent in canonical order: the two keys are equal.", inputs({ protectedFloors: { minimumScannerVersion: "2.6.0", minimumCatalogVersion: "3.0.0", minimumCatalogDigest: H("6") } })),
    c("cache-key-unknown-field", "RK_OWNERSHIP_INVALID_INPUTS", "Unknown fields are rejected (so time or host cannot be smuggled in).", inputs({ now: "2026-01-01" })),
    c("cache-key-bad-digest", "RK_OWNERSHIP_INVALID_INPUTS", "Digests are 64 lowercase hex.", inputs({ contentDigest: "A".repeat(64) })),
    c("cache-key-bad-floor-version", "RK_OWNERSHIP_INVALID_INPUTS", "Floor versions are dotted numerics.", inputs({ protectedFloors: { minimumScannerVersion: "v2", minimumCatalogVersion: null, minimumCatalogDigest: null } })),
    c("cache-key-floors-extra-field", "RK_OWNERSHIP_INVALID_INPUTS", "protectedFloors is closed.", inputs({ protectedFloors: { minimumScannerVersion: null, minimumCatalogVersion: null, minimumCatalogDigest: null, extra: 1 } })),
    c("cache-key-floors-missing-member", "RK_OWNERSHIP_INVALID_INPUTS", "protectedFloors members are all required.", inputs({ protectedFloors: { minimumScannerVersion: null, minimumCatalogVersion: null } })),
    c("cache-key-not-object", "RK_OWNERSHIP_INVALID_INPUTS", "Inputs must be a plain object.", null),
    c("cache-key-array", "RK_OWNERSHIP_INVALID_INPUTS", "Inputs must not be an array.", []),
  ];
  for (const key of ["contentDigest", "catalogDigest", "trustDigest", "approvalsDigest", "scannerPolicyDigest", "protectedFloors"]) {
    const i = inputs(); delete i[key];
    list.push(c(`cache-key-missing-${key}`, "RK_OWNERSHIP_INVALID_INPUTS", `Binding ${key} is required.`, i));
  }
  // Each single-binding change must yield a distinct key (checked by the test, recorded here).
  for (const [key, value] of [["contentDigest", H("9")], ["catalogDigest", H("9")], ["trustDigest", H("9")], ["approvalsDigest", H("9")], ["scannerPolicyDigest", H("9")]]) {
    list.push(c(`cache-key-changed-${key}`, "OK", `Changing ${key} changes the key.`, inputs({ [key]: value })));
  }
  list.push(c("cache-key-changed-floor-scanner", "OK", "Changing a protected floor changes the key.", inputs({ protectedFloors: { minimumScannerVersion: "2.1.0", minimumCatalogVersion: "1", minimumCatalogDigest: null } })));
  list.push(c("cache-key-changed-floor-catalog-digest", "OK", "Changing the catalog digest pin changes the key.", inputs({ protectedFloors: { minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1", minimumCatalogDigest: H("7") } })));
  return list;
}

// Canonical repository identity (ownership-approval.mjs:61-74). Values are recorded as the
// Node implementation behaves today, including quirks (see CONTRACT.md).
export function identityCases() {
  const values = [
    ["https-plain", "https://github.com/test/repo"], ["https-trailing-git", "https://github.com/test/repo.git"], ["https-trailing-slash", "https://github.com/test/repo/"],
    ["https-trailing-git-and-slash", "https://github.com/test/repo.git/"], ["https-trailing-multiple-slashes", "https://github.com/test/repo///"], ["https-uppercase-dot-git", "https://github.com/test/repo.GIT"],
    ["https-leading-slashes-in-path", "https://github.com//test/repo"], ["github-path-lowercased", "https://GitHub.com/Test/Repo"], ["non-github-path-case-kept", "https://git.example.com/Team/App"], ["non-github-host-lowercased", "https://GIT.Example.COM/Team/App"],
    ["scp-git-at", "git@github.com:test/repo.git"], ["scp-git-at-uppercase", "git@GitHub.com:Test/Repo.git"], ["scp-no-git-suffix", "git@github.com:test/repo"], ["scp-nongithub-case-kept", "git@git.example.com:Team/App.git"], ["scp-leading-slash-path", "git@host.example:/abs/repo.git"], ["scp-user-other", "deploy@host.example:team/app"],
    ["scp-without-user-rejected", "github.com:test/repo.git"], ["scp-empty-path", "git@github.com:"],
    ["ssh-url", "ssh://git@github.com/test/repo.git"], ["ssh-url-no-user", "ssh://github.com/test/repo"], ["ssh-url-with-port", "ssh://git.example.com:2222/Team/App"], ["ssh-url-port-22-kept", "ssh://git.example.com:22/team/app"], ["ssh-url-password-rejected", "ssh://user:pw@github.com/test/repo"],
    ["https-default-port-dropped", "https://github.com:443/test/repo/"], ["https-explicit-nondefault-port", "https://git.example.com:8443/team/app"], ["https-http-port-kept", "https://git.example.com:80/team/app"],
    ["http-scheme-rejected", "http://github.com/test/repo"], ["git-scheme-rejected", "git://github.com/test/repo.git"], ["file-scheme-rejected", "file:///tmp/repo"], ["ftp-scheme-rejected", "ftp://host/repo"],
    ["https-userinfo-user-rejected", "https://user@github.com/test/repo"], ["https-userinfo-password-rejected", "https://user:pw@github.com/test/repo"], ["https-empty-userinfo", "https://@github.com/test/repo"],
    ["query-rejected", "https://github.com/test/repo?x=1"], ["fragment-rejected", "https://github.com/test/repo#readme"], ["empty-query-marker-rejected", "https://github.com/test/repo?"],
    ["percent-encoded-slash-kept-literal", "https://git.example.com/team/a%2Fb"], ["percent-encoded-space", "https://git.example.com/team/my%20repo"], ["percent-encoded-uppercase-github-lowercases-hex", "https://github.com/Test/R%C3%A9po"], ["percent-encoded-lowercase-hex-not-normalised", "https://git.example.com/team/r%c3%a9po"], ["raw-unicode-path-encoded-by-url", "https://git.example.com/team/r\u00e9po"], ["raw-space-in-path", "https://git.example.com/team/my repo"],
    ["idn-host-https-punycoded", "https://b\u00fccher.example/team/app"], ["idn-host-ssh-opaque", "ssh://b\u00fccher.example/team/app"], ["idn-host-uppercase-https", "https://B\u00dcCHER.example/team/app"], ["idn-scp", "git@b\u00fccher.example:team/app.git"],
    ["dotdot-in-path-https-resolved", "https://github.com/test/../x"], ["dotdot-leading-resolves-out", "https://github.com/../x"], ["dot-segment-resolved", "https://github.com/test/./repo"], ["dotdot-only-rejected", "https://github.com/.."], ["dotdot-encoded", "https://git.example.com/team/%2e%2e/app"], ["dotdot-ssh", "ssh://git.example.com/team/../app"],
    ["empty-path-rejected", "https://github.com"], ["root-slash-rejected", "https://github.com/"], ["dot-git-only-rejected", "https://github.com/.git"], ["backslash-path", "https://github.com\\test\\repo"],
    ["ipv6-host", "https://[::1]/team/app"], ["ipv4-host", "https://192.0.2.10/team/app"], ["host-trailing-dot", "https://github.com./test/repo"],
    ["surrounding-whitespace-trimmed", "  https://github.com/test/repo \n"], ["empty-string-rejected", ""], ["whitespace-only-rejected", "   "], ["relative-path-rejected", "test/repo"], ["bare-host-rejected", "github.com/test/repo"], ["windows-path-rejected", "C:\\repos\\test"],
    ["very-long-path", "https://git.example.com/" + "a".repeat(600)],
  ];
  const nonStrings = [["non-string-null", null], ["non-string-number", 1], ["non-string-array", ["https://github.com/test/repo"]], ["non-string-object", {}], ["non-string-true", true]];
  return [...values, ...nonStrings].map(([name, value]) => [`identity-${name}`, { kind: "identity", description: `canonicalRepositoryIdentity(${JSON.stringify(value)?.slice(0, 80)})`, value }]);
}

// ---------------------------------------------------------------- admission (broker) scenarios
// Drives the real OwnershipAdmission (ownership-admission.mjs) with scripted worker responses.
const aInputs = (o = {}) => ({ contentDigest: H("1"), catalogDigest: H("2"), trustDigest: H("3"), approvalsDigest: H("4"), scannerPolicyDigest: H("5"), protectedFloors: { minimumScannerVersion: "2.0.0", minimumCatalogVersion: "1", minimumCatalogDigest: null }, ...o });
const observed = (o = {}) => ({ inputs: aInputs(), trust: baseTrust(), complete: true, ...o });
const scanned = (report, o = {}) => ({ report, trust: baseTrust(), inputs: aInputs(), ...o });
export function admissionCases() {
  const REPO = "/fixture/repo";
  const a = (name, description, scenario) => [name, { kind: "admission", description, scenario: { mode: "enforce", scannerRoot: "/fixture/scanner", ...scenario } }];
  const step = (o) => ({ repositoryRoot: REPO, ...o });
  const out = [];
  const codeCases = [
    ["clean", vRep()], ["advisory", vRep({ violations: [src(ADVISE)] })], ["violation", vRep({ ok: false, violations: [src()] })], ["pending-enrollment", vRep({ ok: false, enrollmentPending: true })],
    ["malformed-report", vRep({ extra: 1 })], ["incomplete-coverage", vRep({ ok: false, coverage: [cov("a", "r")] })], ["invalid-registration", vRep({ metadata: vMeta({ repositoryIdentity: "https://github.com/test/other" }) })],
    ["report-failed", vRep({ ok: false })], ["unverified", vRep({ ok: false, problems: ["p"] })],
  ];
  for (const mode of ["enforce", "report"]) for (const [name, report] of codeCases) {
    out.push(a(`admission-${mode}-${name}`, `${mode} mode, scan returns a report whose verdict is ${name}; records classification, outcome and assertAdmitted result.`, { mode, steps: [step({ observed: observed(), scanned: scanned(report) })] }));
  }
  out.push(a("admission-enforce-downgrade-scanner", "Policy downgrade classification and outcome.", { steps: [step({ observed: observed({ trust: baseTrust({ minimumScannerVersion: "99.0.0" }) }), scanned: scanned(vRep(), { trust: baseTrust({ minimumScannerVersion: "99.0.0" }) }) })] }));
  out.push(a("admission-classification-null-trust", "classifyOwnershipReport with trust absent from the inputs-run response.", { steps: [step({ observed: { inputs: aInputs(), complete: true }, scanned: scanned(vRep()) })] }));
  out.push(a("admission-classification-violation-details", "Violations projection (capability, owner=package, fingerprint, file, line) is computed from the raw report even when it is malformed; missing members become null.", { steps: [step({ observed: observed(), scanned: scanned(vRep({ ok: false, violations: [src(), without(without(src({ fingerprint: "http.axios" }), "package"), "line")] })) })] }));
  out.push(a("admission-mode-off", "Mode off returns {mode,outcome:'off'} and never probes the worker.", { mode: "off", steps: [step({ observed: observed(), scanned: scanned(vRep()) })] }));
  out.push(a("admission-scanner-unstaged", "No scanner root: failure with outcome unscanned and code RK_OWNERSHIP_UNVERIFIED.", { scannerRoot: null, steps: [step({})] }));
  out.push(a("admission-missing-root", "No repositoryRoot/workspaceRoot: INVALID_REGISTRATION failure.", { steps: [{}] }));
  out.push(a("admission-workspace-root-fallback", "workspaceRoot is used only when repositoryRoot is absent.", { steps: [{ workspaceRoot: "/fixture/ws", observed: observed(), scanned: scanned(vRep()) }] }));
  out.push(a("admission-repository-root-wins-over-workspace", "repositoryRoot wins when both are supplied.", { steps: [{ repositoryRoot: REPO, workspaceRoot: "/fixture/ws", observed: observed(), scanned: scanned(vRep()) }] }));
  out.push(a("admission-empty-root-rejected", "Empty repositoryRoot string is rejected.", { steps: [{ repositoryRoot: "" }] }));
  out.push(a("admission-cache-hit-same-inputs", "Second evaluation with identical bindings is served from cache (cached:true) without a second scan.", { steps: [step({ observed: observed(), scanned: scanned(vRep()) }), step({ observed: observed() })] }));
  out.push(a("admission-cache-hit-violation-also-cached", "Non-CLEAN deterministic verdicts are cached too.", { steps: [step({ observed: observed(), scanned: scanned(vRep({ ok: false, violations: [src()] })) }), step({ observed: observed() })] }));
  out.push(a("admission-cache-not-stored-malformed", "MALFORMED_REPORT is never cached: second evaluation rescans.", { steps: [step({ observed: observed(), scanned: scanned(vRep({ extra: 1 })) }), step({ observed: observed(), scanned: scanned(vRep()) })] }));
  out.push(a("admission-cache-not-stored-unverified", "UNVERIFIED is never cached: second evaluation rescans.", { steps: [step({ observed: observed(), scanned: scanned(vRep({ ok: false, problems: ["p"] })) }), step({ observed: observed(), scanned: scanned(vRep()) })] }));
  out.push(a("admission-cache-different-root-not-shared", "Same bindings, different scanRoot: cached verdict is not reused.", { steps: [step({ observed: observed(), scanned: scanned(vRep()) }), { repositoryRoot: "/fixture/other", observed: observed(), scanned: scanned(vRep()) }] }));
  for (const [binding, patch] of [["contentDigest", { contentDigest: H("9") }], ["catalogDigest", { catalogDigest: H("9") }], ["trustDigest", { trustDigest: H("9") }], ["approvalsDigest", { approvalsDigest: H("9") }], ["scannerPolicyDigest", { scannerPolicyDigest: H("9") }], ["protectedFloors", { protectedFloors: { minimumScannerVersion: "2.1.0", minimumCatalogVersion: "1", minimumCatalogDigest: null } }]]) {
    out.push(a(`admission-cache-invalidated-by-${binding}`, `A changed ${binding} forces a fresh scan.`, { steps: [step({ observed: observed(), scanned: scanned(vRep()) }), step({ observed: observed({ inputs: aInputs(patch) }), scanned: scanned(vRep(), { inputs: aInputs(patch) }) })] }));
  }
  out.push(a("admission-incomplete-inventory-does-not-share-in-flight-clean", "A concurrent evaluation with an incomplete inventory does not join an in-flight scan of the same key.", { concurrent: true, steps: [step({ observed: observed(), scanned: scanned(vRep()) }), step({ observed: observed({ complete: false }) })] }));
  out.push(a("admission-cache-no-expiry-by-age", "KNOWN DEFECT DEFECT-1 (see CONTRACT.md): Node has no TTL and no date in the key, so a cached verdict outlives the expiry of exceptions it relied on. Recorded as-is; the Rust crate must NOT match step 2 (it must rescan).", { knownDefect: "DEFECT-1", rustRequired: { "steps[1].result.cached": false, "calls.scan": 2 }, steps: [step({ observed: observed(), scanned: scanned(vRep()) }), step({ observed: observed(), advanceClockMs: 365 * 86400000 })] }));
  out.push(a("admission-single-flight-concurrent", "Two concurrent evaluations of the same key share one scan.", { concurrent: true, steps: [step({ observed: observed(), scanned: scanned(vRep()) }), step({ observed: observed() })] }));
  for (const binding of ["contentDigest", "catalogDigest", "trustDigest", "approvalsDigest", "scannerPolicyDigest", "protectedFloors"]) {
    const i = aInputs(); delete i[binding];
    out.push(a(`admission-missing-binding-${binding}`, `inputs-run response lacks ${binding}: INVALID_INPUTS.`, { steps: [step({ observed: observed({ inputs: i }), scanned: scanned(vRep()) })] }));
  }
  out.push(a("admission-inputs-missing-entirely", "inputs-run response has no inputs object.", { steps: [step({ observed: { complete: true, trust: baseTrust() } })] }));
  for (const [label, complete] of [["absent", undefined], ["null", null], ["string", "true"]]) {
    const o = observed(); if (complete === undefined) delete o.complete; else o.complete = complete;
    out.push(a(`admission-complete-${label}-is-invalid-inputs`, `complete=${label}: INVALID_INPUTS.`, { steps: [step({ observed: o, scanned: scanned(vRep()) })] }));
  }
  out.push(a("admission-incomplete-inventory-refuses-before-scan", "complete:false is INCOMPLETE_COVERAGE before any scan, and is not cached.", { steps: [step({ observed: observed({ complete: false }), scanned: scanned(vRep()) }), step({ observed: observed({ complete: false }), scanned: scanned(vRep()) })] }));
  out.push(a("admission-incomplete-inventory-does-not-inherit-cached-clean", "A cached CLEAN for the same key does not hide a later incomplete inventory.", { steps: [step({ observed: observed(), scanned: scanned(vRep()) }), step({ observed: observed({ complete: false }) })] }));
  out.push(a("admission-inputs-changed-between-probe-and-scan", "Scan response carries different bindings than the probe: INPUTS_CHANGED, not cached.", { steps: [step({ observed: observed(), scanned: scanned(vRep(), { inputs: aInputs({ contentDigest: H("9") }) }) })] }));
  out.push(a("admission-scan-error-retries", "Scan error is UNVERIFIED and not cached: the next evaluation scans again.", { steps: [step({ observed: observed(), scanned: { error: "scan timeout" } }), step({ observed: observed(), scanned: { error: "scan timeout" } })] }));
  out.push(a("admission-scan-error-with-code", "Worker error code RK_OWNERSHIP_* is preserved; other codes become UNVERIFIED.", { steps: [step({ observed: observed(), scanned: { error: "changed", code: "RK_OWNERSHIP_INPUTS_CHANGED" } }), step({ observed: observed(), scanned: { error: "x", code: "SOMETHING_ELSE" } })] }));
  out.push(a("admission-probe-error-with-code", "inputs-run error code is preserved.", { steps: [step({ observed: { error: "trust unverified", code: "RK_OWNERSHIP_INVALID_INPUTS" } })] }));
  out.push(a("admission-probe-error-plain", "inputs-run error with no code: the failure code defaults to UNVERIFIED.", { steps: [step({ observed: { error: "boom" } })] }));
  out.push(a("admission-probe-throws", "A throwing inputs runner is caught: UNVERIFIED with the message.", { steps: [step({ observedThrows: "spawn failed" })] }));
  out.push(a("admission-scan-null-report", "Scan returns no report: MALFORMED_REPORT (classification of undefined).", { steps: [step({ observed: observed(), scanned: { inputs: aInputs(), trust: baseTrust() } })] }));
  out.push(a("admission-assert-admitted-report-mode-never-throws", "Report mode never throws from assertAdmitted, even for violations.", { mode: "report", steps: [step({ observed: observed(), scanned: scanned(vRep({ ok: false, violations: [src()] })) })] }));
  out.push(a("admission-assert-admitted-missing-result", "assertAdmitted with no result in enforce mode throws UNVERIFIED.", { steps: [], assertOnly: [null, {}, { admit: true, code: "RK_OWNERSHIP_ADVISORY", problems: ["x"] }, { admit: true, code: "RK_OWNERSHIP_CLEAN" }, { admit: false, code: "RK_OWNERSHIP_VIOLATION", problems: ["a", "b", "c", "d", "e", "f"] }, { admit: false, error: "only an error" }] }));
  return out;
}

// ---------------------------------------------------------------- scan-level helpers
const CARGO = (extra = "") => `[package]\nname = "fixture-app"\nversion = "0.1.0"\nedition = "2021"\n${extra}`;
const MAIN_OK = "fn main() {}\n";
const MAIN_FETCH = "fn main() {\n    http_fetch();\n}\n";
const LOCK_NODEPS = '# This file is automatically @generated by Cargo.\nversion = 3\n\n[[package]]\nname = "fixture-app"\nversion = "0.1.0"\n';
const LOCK_REQWEST = '# This file is automatically @generated by Cargo.\nversion = 3\n\n[[package]]\nname = "fixture-app"\nversion = "0.1.0"\ndependencies = [\n "reqwest",\n]\n\n[[package]]\nname = "reqwest"\nversion = "0.12.4"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n';
const LOCK_TRANSITIVE = '# This file is automatically @generated by Cargo.\nversion = 3\n\n[[package]]\nname = "fixture-app"\nversion = "0.1.0"\ndependencies = [\n "wrapper",\n]\n\n[[package]]\nname = "reqwest"\nversion = "0.12.4"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n\n[[package]]\nname = "wrapper"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\ndependencies = [\n "reqwest",\n]\n';
const LOCK_OWNER = '# This file is automatically @generated by Cargo.\nversion = 3\n\n[[package]]\nname = "fixture-app"\nversion = "0.1.0"\ndependencies = [\n "rightkit-http",\n]\n\n[[package]]\nname = "rightkit-http"\nversion = "0.1.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n';
const PKG = (deps) => JSON.stringify({ name: "fixture-web", version: "1.0.0", dependencies: deps }, null, 2) + "\n";
const PKG_LOCK = (deps) => JSON.stringify({ name: "fixture-web", version: "1.0.0", lockfileVersion: 3, requires: true, packages: { "": { name: "fixture-web", version: "1.0.0", dependencies: deps }, ...Object.fromEntries(Object.entries(deps).map(([n]) => [`node_modules/${n}`, { version: "1.7.2", resolved: `https://registry.npmjs.org/${n}/-/${n}-1.7.2.tgz` }])) } }, null, 2) + "\n";

export function scanCases() {
  const s = (name, expectCode, description, o) => [name, { kind: "scan", description, expectCode, harness: { now: NOW, origin: IDENT, commit: true, ...(o.harness ?? {}) }, repo: { files: o.files ?? {}, symlinks: o.symlinks ?? {}, untracked: o.untracked ?? {} }, catalog: o.catalog ?? CATALOG, trustState: o.trustState ?? state(baseTrust()) }];
  const clean = { "Cargo.toml": CARGO(), "Cargo.lock": LOCK_NODEPS, "src/main.rs": MAIN_OK };
  const multi = (a, b) => ({ "pkg-a/Cargo.toml": CARGO(), "pkg-a/Cargo.lock": LOCK_NODEPS, "pkg-a/src/main.rs": a, "pkg-b/Cargo.toml": CARGO(), "pkg-b/Cargo.lock": LOCK_NODEPS, "pkg-b/src/main.rs": b });
  const dotTrust = state(baseTrust({ repositories: [{ repositoryIdentity: IDENT, roots: [".", "pkg-a"], visibility: "private" }] }));
  const multiTrust = state(baseTrust({ repositories: [{ repositoryIdentity: IDENT, roots: ["pkg-a", "pkg-b"], visibility: "private" }] }));
  return [
    s("scan-clean-pass", "RK_OWNERSHIP_CLEAN", "Clean tree; every catalog capability is not-detected.", { files: clean }),
    s("scan-clean-comment-and-string-ignored", "RK_OWNERSHIP_CLEAN", "Comment-only lines are skipped for line patterns; codeOnly patterns ignore literals and comments.", { files: { ...clean, "src/main.rs": '// http_fetch();\nfn main() {\n    let s = "TcpStream::connect";\n    /* TcpStream::connect */\n}\n' } }),
    s("scan-violation-source-unapproved-new-capability", "RK_OWNERSHIP_VIOLATION", "A new, unapproved use of a catalog capability in source.", { files: { ...clean, "src/main.rs": MAIN_FETCH } }),
    s("scan-violation-source-codeonly", "RK_OWNERSHIP_VIOLATION", "codeOnly pattern matches real code.", { files: { ...clean, "src/main.rs": 'fn main() {\n    let _ = std::net::TcpStream::connect("x");\n}\n' } }),
    s("scan-violation-cargo-dependency", "RK_OWNERSHIP_VIOLATION", "New direct cargo dependency on a catalog-owned crate, resolved via Cargo.lock.", { files: { "Cargo.toml": CARGO('\n[dependencies]\nreqwest = "0.12"\n'), "Cargo.lock": LOCK_REQWEST, "src/main.rs": MAIN_OK } }),
    s("scan-violation-npm-dependency", "RK_OWNERSHIP_VIOLATION", "npm dependency on a catalog-owned package, resolved via package-lock.json.", { files: { ...clean, "package.json": PKG({ axios: "^1.7.0" }), "package-lock.json": PKG_LOCK({ axios: "^1.7.0" }) } }),
    s("scan-violation-transitive-cargo", "RK_OWNERSHIP_VIOLATION", "Block-tier dependency reached only transitively through Cargo.lock.", { files: { "Cargo.toml": CARGO('\n[dependencies]\nwrapper = "1"\n'), "Cargo.lock": LOCK_TRANSITIVE, "src/main.rs": MAIN_OK } }),
    s("scan-advisory-tier", "RK_OWNERSHIP_ADVISORY", "advise-tier finding: report.ok is false but verdict admits with the advisory code.", { files: { ...clean, "src/main.rs": 'fn main() {\n    println!("hi");\n}\n' } }),
    s("scan-banned-policy-overrides-advise", "RK_OWNERSHIP_VIOLATION", "policy:banned blocks although the capability tier is advise.", { files: { ...clean, "src/main.rs": "fn main() {\n    danger_eval();\n}\n" } }),
    s("scan-declared-dependency-owner", "RK_OWNERSHIP_CLEAN", "Depending on the owning package itself is not a finding; row status becomes declared-dependency.", { files: { "Cargo.toml": CARGO('\n[dependencies]\nrightkit-http = "0.1"\n'), "Cargo.lock": LOCK_OWNER, "src/main.rs": MAIN_OK } }),
    s("scan-pending-enrollment-unenrolled-keys", "RK_OWNERSHIP_VIOLATION", "Host keys not enrolled: pending enrollment is a warning; the finding still refuses.", { files: { ...clean, "src/main.rs": MAIN_FETCH }, trustState: { trust: baseTrust({ hosts: {} }), enrolled: false, pending: true, protection: { ok: true } } }),
    s("scan-pending-trust-file-absent", "RK_OWNERSHIP_CLEAN", "Trust file absent (the one tolerated protection failure): pending enrollment is a warning and a clean scan is admitted.", { files: clean, trustState: { trust: null, enrolled: false, pending: true, protection: { ok: false, reason: "external trust file is absent" } } }),
    s("scan-downgrade-scanner-floor", "RK_OWNERSHIP_POLICY_DOWNGRADE", "Preflight verdict refuses before any evidence scan (no evidence in output).", { files: clean, trustState: state(baseTrust({ minimumScannerVersion: "99.0.0" })) }),
    s("scan-downgrade-catalog-digest-pin", "RK_OWNERSHIP_POLICY_DOWNGRADE", "trust.minimumCatalogDigest pins a different catalog.", { files: clean, trustState: state(baseTrust({ minimumCatalogDigest: H("e") })) }),
    s("scan-registration-unregistered-repo", "RK_OWNERSHIP_INVALID_REGISTRATION", "Origin is not in the registry: roots default to ['.'] and the verdict refuses.", { files: clean, harness: { origin: "https://github.com/test/unregistered" } }),
    s("scan-registration-subroots-only-refused-from-repo-root", "RK_OWNERSHIP_UNVERIFIED", "AMBIGUITY: registry roots [pkg-a,pkg-b] exclude '.', yet the worker always scans from the repository root; evidence throws 'release app root is outside protected ownership roots' (error output).", { files: multi(MAIN_OK, MAIN_OK), trustState: multiTrust }),
    s("scan-registration-dot-plus-subroot", "RK_OWNERSHIP_CLEAN", "Registry roots ['.','pkg-a']: both roots are scanned (overlapping), report.metadata.roots has two entries.", { files: multi(MAIN_OK, MAIN_OK), trustState: dotTrust }),
    s("scan-registration-dot-plus-subroot-violation-path", "RK_OWNERSHIP_VIOLATION", "With overlapping roots the same occurrence is reported once per scanned root; paths stay repository-relative.", { files: multi(MAIN_FETCH, MAIN_OK), trustState: dotTrust }),
    s("scan-registration-origin-scp-spelling", "RK_OWNERSHIP_CLEAN", "Origin configured in scp spelling is canonicalised to the registered identity.", { files: clean, harness: { origin: "git@GitHub.com:Test/Repo.git" } }),
    s("scan-coverage-symlink", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Committed symlink is unscanned (path edge case; skipped on Windows).", { files: clean, symlinks: { "src/link.rs": "main.rs" } }),
    s("scan-coverage-lockfile-missing", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Dependencies without a lockfile cannot be resolved.", { files: { "Cargo.toml": CARGO('\n[dependencies]\nserde = "1"\n'), "src/main.rs": MAIN_OK } }),
    s("scan-coverage-node-modules", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Tracked node_modules is opaque.", { files: { ...clean, "node_modules/foo/index.js": "module.exports = 1;\n" } }),
    s("scan-coverage-build-dir", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Tracked target/ is opaque installed-or-build-cache.", { files: { ...clean, "target/debug/gen.rs": "fn g() {}\n" } }),
    s("scan-coverage-invalid-cargo-toml", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Unparseable Cargo.toml.", { files: { ...clean, "Cargo.toml": "[package\nname =" } }),
    s("scan-coverage-invalid-package-json", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Unparseable package.json.", { files: { ...clean, "package.json": "{ nope" } }),
    s("scan-coverage-git-dependency", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "git dependency source is unscanned.", { files: { "Cargo.toml": CARGO('\n[dependencies]\nfoo = { git = "https://example.com/foo.git", rev = "abc" }\n'), "Cargo.lock": LOCK_NODEPS, "src/main.rs": MAIN_OK } }),
    s("scan-path-local-dependency-outside-repository", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "path dependency escaping the repository (../).", { files: { "Cargo.toml": CARGO('\n[dependencies]\nsibling = { path = "../sibling" }\n'), "Cargo.lock": LOCK_NODEPS, "src/main.rs": MAIN_OK } }),
    s("scan-path-windows-separator-dependency", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "Backslash path dependency is treated as outside-repository in committed mode (not normalised).", { files: { "Cargo.toml": CARGO('\n[dependencies]\nsibling = { path = "crates\\\\sibling" }\n'), "Cargo.lock": LOCK_NODEPS, "src/main.rs": MAIN_OK } }),
    s("scan-path-uppercase-extension-detected", "RK_OWNERSHIP_VIOLATION", "Extension matching lowercases the extension (.RS is scanned) but the fingerprint extensions list is compared as written.", { files: { ...clean, "src/MAIN.RS": MAIN_FETCH } }),
    s("scan-path-uppercase-manifest-name-ignored", "RK_OWNERSHIP_CLEAN", "AMBIGUITY: manifest basenames are case-sensitive, so sub/CARGO.TOML with a blocked dependency is silently not parsed.", { files: { ...clean, "sub/CARGO.TOML": CARGO('\n[dependencies]\nreqwest = "0.12"\n') } }),
    s("scan-path-sort-order-locale-compare", "RK_OWNERSHIP_CLEAN", "AMBIGUITY: contentDigests and the content inventory digest are sorted with String.localeCompare (ICU collation), not byte order: 'a_b.rs' < 'a-b.rs' < 'A.rs' < 'B.rs' < 'c.rs' < 'Z.rs' (byte order would put Z before a). A port must reproduce this order or the digests differ.", { files: { ...clean, "A.rs": "fn upper() {}\n", "a-b.rs": "fn hyphen() {}\n", "a_b.rs": "fn underscore() {}\n", "B.rs": "fn bupper() {}\n", "c.rs": "fn clower() {}\n", "Z.rs": "fn zupper() {}\n" } }),
    s("scan-path-space-and-unicode", "RK_OWNERSHIP_VIOLATION", "Spaces and non-ASCII path characters survive NUL-listed git paths.", { files: { ...clean, "src/na\u00efve dir/file one.rs": MAIN_FETCH } }),
    s("scan-registration-no-origin", "RK_OWNERSHIP_INVALID_REGISTRATION", "No origin remote: repositoryIdentity is null, so the preflight verdict refuses before evidence.", { files: clean, harness: { origin: null } }),
    s("scan-error-dirty-checkout", "RK_OWNERSHIP_UNVERIFIED", "Untracked file after commit: committed evidence requires a clean checkout.", { files: clean, untracked: { "src/extra.rs": MAIN_OK } }),
    s("scan-error-unborn-repository", "RK_OWNERSHIP_UNVERIFIED", "Repository with no commit.", { files: clean, harness: { commit: false } }),
    s("scan-error-trust-location-unverified", "RK_OWNERSHIP_INVALID_INPUTS", "Trust protection check failed for a reason other than absence.", { files: clean, trustState: { trust: baseTrust(), enrolled: true, pending: false, protection: { ok: false, reason: "trust path contains a symlink" } } }),
    s("scan-error-key-authority-malformed", "RK_OWNERSHIP_INVALID_INPUTS", "trust.keyAuthority with a relative checkoutPath fails resolution.", { files: clean, trustState: state(baseTrust({ keyAuthority: { repositoryIdentity: IDENT, checkoutPath: "relative/path" } })) }),
    s("scan-error-inputs-changed-during-scan", "RK_OWNERSHIP_INPUTS_CHANGED", "Working-tree inventory differs between the before and after input hashes.", { files: clean, harness: { mutateOnSecondInventory: true } }),
  ];
}

// ---------------------------------------------------------------- worker transport (runScanWorker)
const ECHO = "process.stdout.write(JSON.stringify({ argv: process.argv.slice(2) }) + '\\n');";
export function transportCases() {
  const t = (name, description, o) => [name, { kind: "transport", description, timeoutMs: 20000, inputsOnly: false, execPath: "node", ...o }];
  return [
    t("transport-args-full-scan", "Argument vector for a full scan: <worker> <scanRoot> <catalog>.", { workerSource: ECHO }),
    t("transport-args-inputs-only", "inputsOnly appends --inputs.", { workerSource: ECHO, inputsOnly: true }),
    t("transport-stdout-json-verbatim", "A zero exit with JSON stdout is returned as parsed (no schema applied here).", { workerSource: "process.stdout.write('{\"any\":[1,2,3]}\\n');" }),
    t("transport-nonzero-with-stderr", "Non-zero exit returns {error: trimmed stderr}.", { workerSource: "process.stderr.write('  boom happened \\n'); process.exit(3);" }),
    t("transport-nonzero-stderr-truncated", "stderr is truncated to its first 2000 characters.", { workerSource: "process.stderr.write('e'.repeat(2500)); process.exit(1);" }),
    t("transport-nonzero-silent", "Non-zero exit with empty stderr reports `scanner exited <code>`.", { workerSource: "process.exit(7);" }),
    t("transport-invalid-json", "Zero exit with non-JSON stdout.", { workerSource: "process.stdout.write('not json');" }),
    t("transport-empty-stdout", "Zero exit with empty stdout is invalid JSON.", { workerSource: "" }),
    t("transport-timeout", "Deadline exceeded: child killed, error `scan exceeded <timeoutMs> ms`.", { workerSource: "setTimeout(() => {}, 30000);", timeoutMs: 400 }),
    t("transport-start-failure", "Interpreter missing: error `scanner failed to start: ...` (message text is OS specific; recorded for POSIX).", { workerSource: ECHO, execPath: "/fixture/no-such-node" }),
  ];
}
