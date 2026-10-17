# RightKit ownership scanner and verdict: frozen JSON contract

Frozen from the Node implementation in `packages/release` (scanner version `2.6.0`, `ownership.mjs:22`).
Shared fixtures for the Rust port (`rightkit-ownership`) and the RustRight broker live in `cases/`.
Everything below was read from code; where behaviour looks accidental it is recorded as-is and marked **AMBIGUITY** or **DEFECT**.
Line numbers refer to `packages/release/*.mjs` unless a path is given.

Files:

| File | Purpose |
|---|---|
| `report.schema.json` | Report, evidence, scan-worker stdout (`$defs.workerOutput`, `$defs.inputsOutput`) |
| `verdict.schema.json` | Verdict, cache-key inputs/result, admission result |
| `generate.mjs` | Writes `cases/` from the current Node code; `--check` fails on drift (exit 1) |
| `cases.mjs` | Case definitions (data only) |
| `schema-check.mjs` | Dependency-free JSON Schema subset validator used by the test |
| `../ownership-fixtures.test.mjs` | Runs `generate.mjs --check`, validates schemas, asserts code coverage |

Regenerate with `node fixtures/ownership/generate.mjs` after an intended behaviour change, from `packages/release`.
A scanner version bump changes every scan case by design (it is part of the report and of `evidence`).

## 1. Fixture layout

`cases/<name>/input.json` has `kind` and `description`. `expected.json` is whatever the current Node code produced.

| kind | input | expected | Node function exercised |
|---|---|---|---|
| `verdict` | `report`, `trust` | `{verdict}` | `ownershipVerdict` |
| `cache-key` | `inputs` | `{key}` or `{error:{code,message}}` | `ownershipCacheKey` |
| `identity` | `value` | `{canonical}` or `{error}` | `canonicalRepositoryIdentity` |
| `admission` | `scenario` (scripted worker replies) | per-step `result`, `assertAdmitted`, `calls` | `OwnershipAdmission` (broker) |
| `transport` | `workerSource`, `timeoutMs`, `inputsOnly`, `execPath` | `{result}` | `runScanWorker` |
| `scan` | `repo`, `catalog`, `trustState`, `harness` | `{scan, inputsMode, cacheKey}` | `scanOwnershipForTest`, `ownershipScanInputsForTest` |

`expectCode` in an input is the author's intent; `generate.mjs` refuses to write if the implementation disagrees.

### Scan-case materialisation (the repo is data, not a checked-in tree)

`repo.files` (path to text) are written to a fresh temp directory, then: `git init -b main`, `git remote add origin <harness.origin>` (skipped when null), symlinks from `repo.symlinks` (path to target), `git add -A`, `git commit` (skipped when `harness.commit` is false), then `repo.untracked` files are written after the commit.
Git runs with `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, author and committer `Fixture <fixture@example.invalid>`, both dates `2026-01-01T00:00:00Z`, message `fixture`, `core.autocrlf=false`. Commit and tree ids in the expected files are therefore reproducible with git sha1 objects.
Trees are inline in `input.json` rather than checked in as directories because the cases include symlinks, a case-colliding-name hazard (see 8) and `target/`/`node_modules/` paths that tooling ignores.
`scan-coverage-symlink` needs symlink support (skip on Windows hosts without it).

### Pinned volatile values

| Value | Pinned how |
|---|---|
| Clock (`new Date()`, `Date.now()`) | `harness.now`, default `2026-10-09T00:00:00.000Z`; the generator replaces `Date` for the call. It feeds `scannedAt`, `evidence.checkedAt` and exception-expiry checks (`ownership.mjs:622`). |
| `metadata.*.host` | rewritten to `fixture-host/fixture-os` (`ownership.mjs:600`). |
| temp paths | rewritten to `<ROOT>`, `<TMP>`, `<SCANNER>`. |
| `scannerPolicyDigest` | injected constant `sha256("rightkit-ownership-fixture-scanner-policy-v1")`. In production it hashes the scanner's own module closure (`ownership-scan-worker.mjs:16-28`) and cannot be shared. |
| Trust location and ACL audit | `trustState` is injected through the `trustLoader` seam; the fixed OS path (`ownership-approval.mjs:10-13,328-357`) is never read. |
| `admission` clock and paths | `now()` returns 0 (`scanMs` is 0); roots are the literal strings `/fixture/repo`, `/fixture/scanner` (POSIX spelling). |
| JSON parser error text | `transport-invalid-json`, `transport-empty-stdout` keep only the prefix `scanner returned invalid JSON: <parser message>`. |

`trustState.trust.hosts` use `TEST-ONLY-PLACEHOLDER-*` strings. No key material, signature or private key exists anywhere in the fixtures.

## 2. Scan worker

### 2.1 Invocation (the "input message")

`node ownership-scan-worker.mjs <appRoot> <installedCatalogPath> [--inputs]` (`ownership-scan-worker.mjs:92-100`; spawned by `ownership-admission.mjs:29-52`).

- `argv` rules (`:95`): `appRoot` and `catalogPath` required; `path.resolve(catalogPath)` must equal the sibling `rightkit-ownership.json` of the worker; the third argument may only be `--inputs`. Violations throw `TypeError` and are printed as an error line.
- stdin is ignored. Exactly one JSON line is written to stdout; the process exits `0` even for error lines. A non-zero exit happens only on a crash outside the `try`.
- Implicit inputs: the protected trust file (`loadTrustedExternal`, `ownership-approval.mjs:328-357`), the catalog JSON at `catalogPath`, the committed git tree of `appRoot` (HEAD, not the working tree, for the evidence scan: `ownership-evidence.mjs:42-48`), the working tree for the content inventory (`ownership.mjs:435-440`), the origin remote, and the clock.
- The logical trust input is `trustState = {trust, enrolled, pending, protection:{ok, reason?, mechanism?}, path?}` (`ownership-approval.mjs:331,356`). Accepted when `protection.ok === true`, or in the single tolerated absence form `{pending:true, enrolled:false, trust:null, protection:{ok:false, reason:"external trust file is absent"}}` (`ownership-scan-worker.mjs:33`).
- `--inputs` mode returns only the cache bindings (section 4) and does no evidence scan.

### 2.2 Output shapes (`report.schema.json#/$defs/workerOutput`, `inputsOutput`)

Full mode prints one of:

| Shape | When | Fields |
|---|---|---|
| scan | preflight verdict admits, or is `PENDING_ENROLLMENT`, and evidence succeeds (`:85`) | `inputs, trust, report, verdict, evidence` |
| preflight refusal | preflight verdict refuses for any other code (`:71`) | `inputs, trust, complete, enrollmentPending, metadata, report, verdict`; `report.ok` forced false, `report.violations` and `problems` empty |
| pending with evidence error | pending enrollment and `createEvidence` throws (`:74-77`) | scan shape without `evidence`: not produced by the fixtures; the throw is converted to `report.problems:[message]`, `ok:false` |
| error | any throw (`:98`), or inputs changed (`:84`) | `{error: string, code: string}` |

`--inputs` mode prints `{inputs, trust, complete, enrollmentPending, metadata}` (`:60-61`) or an error line.

`trust` is the resolved trust: when `trust.keyAuthority` is set it is the key-log-resolved trust (`:36-40`), otherwise the loaded registry; it may be `null` (pending, trust absent).
`complete` is `true` iff `metadata.coverage` is empty (`:45`).
`enrollmentPending = trust?.enrollmentComplete === false || state.enrolled !== true || state.pending === true` (`:60`).

Preflight verdict (`:68-71`) is computed from `{ok:true, enrollmentPending, violations:[], problems:[], coverage, metadata}` where `coverage` and `metadata` come from the working-tree inventory (`:61`). Hence INCOMPLETE_COVERAGE, INVALID_REGISTRATION and POLICY_DOWNGRADE are decided before any evidence scan and the evidence-level coverage reasons (lockfile, cargo parse) only appear when inventory coverage is empty.

### 2.3 Report (`report.schema.json#/$defs/report`)

Closed object; unknown keys fail (`ownership-verdict.mjs:6-8,33-35`). Worker report = `{...evidence.result, ok, enrollmentPending, metadata:{...result.metadata, scannerVersion, catalogVersion}}` round-tripped through JSON (`ownership-scan-worker.mjs:79-80`).

| Field | Type | Req | Source |
|---|---|---|---|
| `ok` | boolean | yes | `ownership-evidence.mjs:174`: `!enrollmentPending && result.ok && floor.ok && contentCoverage.length===0` |
| `enrollmentPending` | boolean | yes | `ownership-scan-worker.mjs:79` (`checked.enrollmentPending === true`) |
| `violations` | finding[] | yes | evidence `:143` |
| `problems` | non-empty string[] | yes | evidence `:144`; includes floor problems and registration diagnostics while enrollment is pending |
| `coverage` | coverage[] | yes | evidence `:147` |
| `metadata` | metadata | yes | evidence `:148-152` |
| `findings` | finding[] | no | all findings incl. covered (`:142`) |
| `covered` | finding+`exception`[] | no | findings covered by a signed approval (`:146`) |
| `rows` | row[] | no | `capabilityTable` (`ownership.mjs:650-663`), each row gains `root` (`:145`) |
| `warnings` | string[] | no | verdict accepts it; produced by `evaluate` but not forwarded by the worker |
| `warning` | non-empty string | no | accepted by the verdict; not produced by the worker report |

Finding (`ownership-verdict.mjs:16-21`): required `fingerprint, capability, package, file` (non-empty strings) and `line` (safe integer >= 0). Optional: `kind` (`source|dependency|transitive-dependency`), `lineText`, `ordinal`, `text` (strings, may be empty), `occurrenceHash` (64 lowercase hex), `lockfile`, `dependencyPath` (>= 3 non-empty strings), `ecosystem, name, section` (non-empty), `alias, requirement, source, resolvedVersion, provenance` (null or non-empty), `features` (non-empty strings), `defaultFeatures` (boolean or null), `local` (boolean), `tier` (`block|advise`), `policy` (non-empty).
Transitive findings additionally require `name, resolvedVersion, source, dependencyPath, lockfile`, with `dependencyPath.at(-1) === name` and `lockfile === file` (`:21`).
Producers: source findings `ownership.mjs:56`; dependency and transitive findings `:521-523`. `tier` defaults to the fingerprint tier, then the capability tier, then `block` (`:618-620`). `policy:"banned"` is copied only when present.
`occurrenceHash` rules: `ownership-approval.mjs:91-111`.

Metadata (`ownership-verdict.mjs:26-32`): required `repositoryIdentity` (null or non-empty), `scannerVersion`, `catalogVersion` (non-empty), `catalogDigest` (64 hex), `coverage`. Optional: `commit` (null or text), `tree, host, scannedAt, pathBase, source` (non-empty), `pathPrefix` (string, may be empty), `contentDigests` (`{path, digest: 64hex | git:<40..64 hex>, provenance?}`), `provenance` (`{kind, requestId, contentDigest, paths}`), `exceptionDigest, keyLogDigest, rootFingerprint` (64 hex), `keyCheckpoint {sequence, head}`, `keyAuthority {repositoryIdentity, commit, tree}`, `roots` (`{root, metadata?}`; `metadata` is recursive).
Worker metadata always has `pathBase:"repository"`, `pathPrefix:""` and one `roots[]` entry per scanned registered root, each carrying that root's own scanner metadata (`ownership-evidence.mjs:148-152`).

Coverage entry: `{path, status, reason, detail?}`; any `status` other than the literal `"complete"` blocks (`ownership-verdict.mjs:57`). Producers always emit `status:"unscanned"`.
Reasons observed or emitted: `symlink`, `committed-symlink`, `gitlink`, `committed-gitlink`, `deleted`, `not-regular-file`, `git-index-conflict`, `file-over-size-limit`, `source-file-over-2MiB`, `manifest-file-over-64MiB`, `protected-provenance-file-over-2MiB`, `committed-exceptions-over-2MiB`, `invalid-committed-exceptions-json`, `node-modules-provenance-not-verified`, `installed-or-build-cache`, `invalid-cargo-toml`, `invalid-json`, `invalid-npm-dependency:<name>`, `git-dependency-source:<name>`, `git-npm-dependency-source:<name>`, `local-dependency-outside-repository`, `local-dependency-path-unresolved`, `local-npm-dependency-*`, `local-swift-dependency-*`, `dependency-lock-resolution:<message>`, `lockfile-missing` (with `detail` ecosystem), `lockfile-root-unresolved` (only for a manifest the lock workspace owns: Cargo `[workspace]` members, default-members, exclude and in-directory path dependencies; pnpm-workspace.yaml `packages` globs; a manifest outside the workspace is the non-blocking info entry `manifest-outside-workspace`, never a coverage gap), `lockfile-parse-failed`, `lockfile-unsupported-format`, `registered-root-missing-from-committed-tree`, `protected-provenance-stale-or-unreadable`, `git-inventory-failed:*`, `git-listing-invalid:*`, `git-blob-read-failed:*`, `repository-root-unavailable:*`, `git-blob-cache-miss`, `file-read-failed:*`, `stat-failed:*` (`ownership.mjs`, `ownership-git-inventory.mjs`, `ownership-lockfiles.mjs`). The reason strings are free text for the verdict; only the path/reason pair is used (de-duplicated) in the message.

Row (`ownership-verdict.mjs:25`): `capability, owner, status` non-empty, `uses` boolean, `count` safe integer >= 0, optional `root`, `owners`. `status` values: `blocked | excepted | declared-dependency | unscanned | not-detected` (`ownership.mjs:660`), priority in that order.

Evidence (`report.schema.json#/$defs/evidence`, `ownership-evidence.mjs:154-172`): `schema:1, repositoryIdentity, source` (`enrollment-pending-advisory | committed-git-tree | mutable-checkout-test-seam`), `commit, tree` (absent while pending), `catalogDigest, catalogVersion, scannerVersion, approvalsDigest` (null while pending), `trustEnrolled, keyLogDigest?, keyCheckpoint?, rootFingerprint?, keyAuthority?, floorOk, visibility, registrationDigest, registeredRoots, contentDigest` (null while pending), `checkedAt`.
`verifyOwnershipEvidence` (`:215-228`) re-scans and compares: `repositoryIdentity, source, commit, tree, catalogDigest, catalogVersion, scannerVersion, approvalsDigest, keyLogDigest, keyCheckpoint, rootFingerprint, keyAuthority, visibility, registrationDigest, registeredRoots, contentDigest`; `checkedAt` is excluded.

## 3. Verdict

`ownershipVerdict(report, trust) -> {admit, code, problems}` (`ownership-verdict.mjs:41-66`). Pure function of its two arguments; no clock, no filesystem.

Evaluation order (first match wins):

| # | Condition | `admit` | `code` | `problems` | Line |
|---|---|---|---|---|---|
| 1 | report fails `validReport` | false | `RK_OWNERSHIP_MALFORMED_REPORT` | `["ownership report has missing, unknown, or wrong-typed fields"]` | 42 |
| 2 | `report.enrollmentPending` | false | `RK_OWNERSHIP_PENDING_ENROLLMENT` | `["protected ownership approval enrollment is pending", ...report.problems]` | 43 |
| 3 | registration lookup throws, or fails, or scanned roots do not exactly equal registered roots | false | `RK_OWNERSHIP_INVALID_REGISTRATION` | thrown: `["protected repository registry is missing or malformed"]`; else registration problems; else `["scan roots do not exactly cover protected repository registration"]` | 44-54 |
| 4 | `validateOwnershipFloor` fails | false | `RK_OWNERSHIP_POLICY_DOWNGRADE` | floor problems | 55-56 |
| 5 | any `report.coverage` or `report.metadata.coverage` entry with `status !== "complete"` | false | `RK_OWNERSHIP_INCOMPLETE_COVERAGE` | de-duplicated `"<path>: <reason>"` | 57-58 |
| 6 | any blocking violation | false | `RK_OWNERSHIP_VIOLATION` | `"<capability> (<fingerprint> in <file>) is owned by <package>"` per blocking finding | 59-60 |
| 7 | `report.problems` non-empty | false | `RK_OWNERSHIP_UNVERIFIED` | `report.problems` verbatim | 61 |
| 8 | `report.ok === false` | false | `RK_OWNERSHIP_REPORT_FAILED` | `["protected ownership scanner reported ok:false"]` | 62 |
| 9 | advise-tier violations remain | **true** | `RK_OWNERSHIP_ADVISORY` | `"<capability> (<fingerprint> in <file>) is owned by <package> [advise]"` | 63-64 |
| 10 | otherwise | **true** | `RK_OWNERSHIP_CLEAN` | `[]` | 65 |

Blocking finding (`:37`): `policy === "banned" || tier !== "advise"`. A missing `tier` is blocking. `tier:"warn"` is malformed (step 1), not advisory.
Steps 8 and 9: `ok:false` is tested before the advisory branch, so a hand-built `ok:false` report holding only advisory findings is `REPORT_FAILED` (`verdict-ok-false-with-advisory-is-report-failed`). A real advisory-only scan reports `ok:true` and is admitted as `ADVISORY` (see A8).

Registration check (`ownership-approval.mjs:222-231`): `validateTrustRegistry` (`:168-200`) must pass; identity is canonicalised (section 7); the registry entry is looked up by canonical identity; every path in `violations[].file`, `findings[].file`, `metadata.contentDigests[].path` must satisfy `exactRepositoryPath` (`:164-166`: `"."` or a relative path with no leading `/`, no `\`, no empty/`.`/`..` segments, no `* ? [ ] { }`) and lie within a registered root (`:167`). Scanned roots are `metadata.roots[].root`, else the single root derived from `metadata.pathPrefix` with the trailing `/` removed, else `"."` (`ownership-verdict.mjs:52`). They must have no duplicates and equal the registered set after sorting (`:53`).
`validateTrustRegistry` returns early with only `["protected repository registry missing"]` when `repositories` is not an array, discarding earlier problems (`:184`).

Floor (`ownership-approval.mjs:267-284`): `versionAtLeast` splits on `.`, each segment must match `^\d+$` (else NaN, so false), compares numerically with missing segments as 0. Checks `scannerVersion >= trust.minimumScannerVersion`, `catalogVersion >= trust.minimumCatalogVersion`, and `trust.minimumCatalogDigest` (when truthy) equal to `metadata.catalogDigest`.

Trust registry subset the verdict reads (`verdict.schema.json#/$defs/trustRegistry`): `schema:1`, `revokedKeyIds[]`, `verifiedTrackingEvidence[]` (64 hex), `minimumScannerVersion`, `minimumCatalogVersion` (dotted numeric), optional `minimumCatalogDigest`, `repositories[] {repositoryIdentity (canonical), roots[] (exact paths, non-empty), visibility public|private}` (no duplicates), optional `provenance[]`, `rootFingerprint`, `keyCheckpoint`, `keyAuthority {repositoryIdentity, checkoutPath}`, `hosts`.

## 4. Cache revalidation inputs

`ownershipCacheKey(inputs) = sha256(canonicalJson(inputs))` (`ownership-verdict.mjs:69-74`, `canonicalJson`/`sha256` `ownership-approval.mjs:76-84`). `canonicalJson` sorts object keys at every level and uses `JSON.stringify` for scalars with no whitespace; `sha256` of a string hashes its UTF-8 bytes.
The inputs object is closed and every member is required:

| Binding | Type | Computed at |
|---|---|---|
| `contentDigest` | 64 hex | `sha256([{path,size,digest}...])` over every git-listed file (tracked and untracked, ignores excluded), sorted by `localeCompare` on `path` (`ownership.mjs:435-440`). `digest` is `git:<oid>` for clean files or `sha256(bytes)` for dirty ones (`ownership-git-inventory.mjs:187-189`). File mode is not included. |
| `catalogDigest` | 64 hex | `sha256(catalogJson)` (`ownership.mjs:23`) |
| `trustDigest` | 64 hex | `sha256({trust, repositoryIdentity, enrolled, pending, keyLogDigest})` where `trust` is the resolved trust (`ownership-scan-worker.mjs:55`) |
| `approvalsDigest` | 64 hex | `sha256([{root, digest}])`; per registered root, `sha256(bytes)` of its approvals file in the **working tree**, or `sha256({absent:true})` (`:48-52,56`) |
| `scannerPolicyDigest` | 64 hex | `sha256({version, files:[{path,digest}]})` over the scanner's static import closure (`:16-28`) |
| `protectedFloors` | closed object | `{minimumScannerVersion, minimumCatalogVersion, minimumCatalogDigest}`, each `null` or (dotted numeric / 64 hex) (`:57`) |

Anything else is rejected with `RK_OWNERSHIP_INVALID_INPUTS` (a `TypeError`), so no time, host or path can be added to the key.
Not part of the key: the wall clock, the scan root path (the broker keeps `scanRoot` equality separately, `ownership-admission.mjs:94`), git commit/tree ids, file modes, the OS user. `repositoryIdentity` is covered through `trustDigest`.

Broker revalidation (`ownership-admission.mjs:82-104,113-124`) on every evaluation: run the worker with `--inputs`; `complete` must be a boolean; compute the key; `complete:false` refuses with `INCOMPLETE_COVERAGE` before any lookup (so an incomplete inventory never inherits a cached or in-flight CLEAN); hit requires equal key and equal `scanRoot`; concurrent requests for the same `scanRoot:key` share one scan; after a scan the returned `inputs` must hash to the same key or the result is `INPUTS_CHANGED` (not cached); everything except `MALFORMED_REPORT` and `UNVERIFIED` is cached (128 entries, FIFO eviction); there is no TTL.

### DEFECT-1: a cached verdict can outlive the exceptions it relied on

Approvals expire by calendar date, evaluated with the live clock: `today = new Date().toISOString().slice(0,10)` (`ownership.mjs:622`), `p.expires < today` (`:633`) and `r.expires < today` (`ownership-approval.mjs:404`). The cache key has no date input (`ownership-verdict.mjs:70`, `ownership-scan-worker.mjs:53-58`) and the broker cache has no TTL (`ownership-admission.mjs:93-94,119-122`). A CLEAN (or ADVISORY) verdict produced because an approval covered a violation is therefore served after that approval expires, until another binding changes or the broker restarts.
Recorded as-is in `cases/admission-cache-no-expiry-by-age` (`scenario.knownDefect = "DEFECT-1"`, `scenario.rustRequired`).

**Required behaviour for the Rust crate (not the Node behaviour):** a cached verdict must not outlive the earliest `expires` of any approval it relied on. Enforce it with either (a) the expiry (or at minimum the UTC date) as part of the cache key, or (b) a `validUntil` carried on the cached verdict and checked on every hit. `ownershipCacheKey` itself stays a pure function of the six bindings so `cache-key-*` fixtures remain shared. `admission-cache-no-expiry-by-age` is the one fixture the Rust crate must deliberately fail to match: step 2 must rescan (`cached:false`, `calls.scan == 2`).
RustRight's `OwnershipAdmission` already appends the UTC date to its cache key (`rustright-ownership/src/admission.rs:357`, `utc_date` at `:525`). That meets (a) conservatively; it does not need the exact earliest expiry. Node must still be fixed separately; this task changes no Node code.

## 5. Error and verdict codes

Verdict codes (all produced by `ownershipVerdict`): `CLEAN`, `ADVISORY` (admit true); `MALFORMED_REPORT`, `PENDING_ENROLLMENT`, `INVALID_REGISTRATION`, `POLICY_DOWNGRADE`, `INCOMPLETE_COVERAGE`, `VIOLATION`, `UNVERIFIED`, `REPORT_FAILED` (admit false). All prefixed `RK_OWNERSHIP_`.

Non-verdict codes:

| Code | Emitted by | When |
|---|---|---|
| `RK_OWNERSHIP_INVALID_INPUTS` | `ownership-scan-worker.mjs:34,38,46`; `ownership-verdict.mjs:72` | trust location not verified; key authority unresolved; inventory completeness missing; cache inputs malformed. Broker: `ownership-admission.mjs:90` |
| `RK_OWNERSHIP_INPUTS_CHANGED` | `ownership-scan-worker.mjs:84`; broker `:117` | inputs differ between the before/after hash, or between probe and scan |
| `RK_OWNERSHIP_UNVERIFIED` | worker CLI catch-all (`:98`); broker default (`:77,103`) | any other throw, scanner start/exit/JSON failure, unstaged scanner (`:86`) |
| `RK_OWNERSHIP_INCOMPLETE_COVERAGE` | broker `:92` | `complete:false` from the inputs probe |
| `RK_OWNERSHIP_INVALID_REGISTRATION` | broker `:85` | no `repositoryRoot`/`workspaceRoot`, or an empty string |
| `RK_OWNERSHIP_VIOLATION`, `RK_OWNERSHIP_UNVERIFIED` | `thin-job-service.mjs:727` | consumed, not produced |
| `RK_OWNERSHIP_ELEVATION_UNPROTECTED`, `RK_OWNERSHIP_REPOSITORY_DRIFT`, `RK_OWNERSHIP_REPOSITORY_REGISTRY_MALFORMED` | `ownership-key-command.mjs`, `ownership-key-trust.mjs` | key ceremony only; out of scope |

Worker error strings that are stable enough to compare (`code` is the contract; text is informative): `protected ownership trust location was not verified`; `ownership inputs changed during protected evidence scan`; `app root & installed protected catalog path are required`; plus thrown messages from git/evidence (see `scan-error-*`).
Broker failure shape (`ownership-admission.mjs:77-79`): `{mode, admit:false, code, outcome, scanRoot, error, problems:[error], violations:[], scanMs:0, cached:false}`; `outcome = "unscanned"` when `code === UNVERIFIED`, else `outcomes[code] ?? "unscanned"`.
Thrown by `assertAdmitted` (`:107-111`): `Error("RightKit ownership refuses this build: " + first five problems joined by "; " or error or "ownership verdict is missing or malformed")` with `.code = result.code ?? "RK_OWNERSHIP_UNVERIFIED"`.

### Intended behaviours (recorded as intended, not quirks)

1. **Enforce mode refuses `RK_OWNERSHIP_ADVISORY` (fail closed).** `assertAdmitted` admits only `result.admit === true && result.code === "RK_OWNERSHIP_CLEAN"` (`ownership-admission.mjs:109`). In `enforce`, an ADVISORY result throws with code `RK_OWNERSHIP_ADVISORY`. In `report` mode nothing throws and the verdict is recorded. Fixtures: `admission-enforce-advisory`, `admission-report-advisory`, `admission-assert-admitted-missing-result`.
2. **An advisory report has outcome `unscanned`.** `outcomes` (`:54-59`) deliberately has no entry for `RK_OWNERSHIP_ADVISORY`, so `classifyOwnershipReport` yields `outcome:"unscanned"` (`:64`) while `admit` stays `true`. Fixture: `admission-enforce-advisory` / `admission-report-advisory`.

## 6. Admission and classification (broker layer)

`classifyOwnershipReport(report, trust)` (`ownership-admission.mjs:61-65`) = `{...ownershipVerdict(report, trust), outcome, violations}` where `violations` is projected from the **raw** `report.violations` (object entries only) to `{capability, owner (=package), fingerprint, file, line}`, each missing member `null`, even when the report is malformed (`admission-classification-violation-details`).
Outcome map: CLEAN `clean`, VIOLATION `violations`, PENDING_ENROLLMENT `pending-enrollment`, MALFORMED_REPORT `malformed-report`, INCOMPLETE_COVERAGE `incomplete-coverage`, INVALID_REGISTRATION `invalid-registration`, POLICY_DOWNGRADE `policy-downgrade`, REPORT_FAILED `report-failed`, UNVERIFIED `problems`; every other code (ADVISORY, INVALID_INPUTS, INPUTS_CHANGED, unknown) `unscanned`. Mode `off` returns only `{mode:"off", outcome:"off"}` without probing.
Scan root: `repositoryRoot` if a non-empty string, else `workspaceRoot`; resolved with `path.resolve` (`:24-27`).
Trust for classification comes from the **inputs probe** response (`observed.trust`), not from the scan response (`:118`).

## 7. Canonical repository identity (`ownership-approval.mjs:61-74`)

Algorithm: reject non-strings and blank strings (`repository identity is required`); trim; if it matches `^([^@/:]+)@([^:]+):(.+)$` rewrite to `ssh://<host>/<path>`; parse with the WHATWG `URL`; require protocol `https:` or `ssh:`, non-empty hostname, no `search`, no `hash`, no `password`, and for `https:` no `username` (`unsupported repository identity URL`); lowercase the hostname; take `pathname`, strip leading `/`+ and trailing `/`+, strip a trailing `.git` case-insensitively; if the host is `github.com` lowercase the path; reject empty path or any `''`, `.`, `..` segment (`invalid repository identity path`); return `https://<host>[:<port>]/<path>`.
`url.port` is the URL parser's port, so `https` drops `:443` and keeps every other port, `ssh` keeps `:22`.
Observed behaviours worth porting exactly (all in `cases/identity-*`):

- IDNA: `https://bücher.example/...` becomes punycode `xn--bcher-kva.example`, but `ssh://bücher.example/...` and the scp form keep an opaque host, percent-encoded and lowercased: `b%c3%bccher.example`. (Non-special scheme host parsing.)
- Percent-encoding is preserved as written, not decoded: `a%2Fb` stays; `r%c3%a9po` keeps lowercase hex; raw non-ASCII or a space in the path is percent-encoded uppercase (`%C3%A9`, `%20`) by the URL parser; for `github.com` the lowercasing also lowercases existing hex (`R%C3%A9po` becomes `r%c3%a9po`).
- Dot segments (`.`, `..`, `%2e%2e`) are resolved by the URL parser before the checks, so `https://github.com/test/../x` is `https://github.com/x` and `https://github.com/../x` is `https://github.com/x`; `https://github.com/..` fails with `invalid repository identity path`.
- `https://@github.com/test/repo` (empty userinfo) and a trailing `?` with an empty query are accepted; `?x=1` and `#frag` are rejected.
- Backslashes in an `https` URL are converted to `/` by the URL parser; `https://github.com\test\repo` is accepted.
- `https://github.com./test/repo` keeps the trailing dot in the host.
- `github.com:test/repo.git` (scp without user) is parsed as scheme `github.com` and fails as `unsupported repository identity URL`; `git@github.com:` fails as `repository identity must be an absolute HTTPS/SSH URL or SSH scp remote`.
- scp paths keep a leading slash after the colon: `git@host.example:/abs/repo.git` becomes `https://host.example/abs/repo`.
- Expected Rust divergence: the RustRight port documents that IDNA, IPv6 normalisation and percent handling are not reproduced (`approval.rs:12-16`). The `identity-idn-*`, `identity-percent-*`, `identity-raw-*`, `identity-backslash-path`, `identity-host-trailing-dot` and `identity-https-empty-userinfo` fixtures will expose it; either implement WHATWG behaviour or record each divergence as an explicit, reviewed exception.

## 8. Path edge cases

- **Symlinks:** a tracked or untracked symlink is `unscanned` (`symlink` in the working-tree inventory, `ownership-git-inventory.mjs:118-120,152`; `committed-symlink` in the committed inventory, `ownership.mjs:212`). Gitlinks: `gitlink` / `committed-gitlink`. Fixture `scan-coverage-symlink` is decided at preflight (`symlink`).
- **Case:** extension matching lowercases the extension (`ownership.mjs:195,215,536`), so `src/MAIN.RS` is scanned (`scan-path-uppercase-extension-detected`). Manifest and lockfile names are matched exactly (`["Cargo.toml","package.json","Package.swift", ...]`, `:195,215,348`), so `sub/CARGO.TOML` is silently ignored (**AMBIGUITY A4**, `scan-path-uppercase-manifest-name-ignored`).
  Case-colliding paths (`A.rs`/`a.rs`) cannot be materialised on a case-insensitive filesystem; no such fixture exists (the generator was found to lose one of the pair on macOS).
- **Windows separators:** the inventory uses git's `/` paths and `rel.split("/")`. In the committed inventory a dependency `path` containing `\` or an absolute path is reported as `local-dependency-outside-repository` without resolving (`ownership.mjs:135`; `scan-path-windows-separator-dependency`). Working-tree mode (test seam only) resolves with `path.resolve` and so is OS-dependent. The verdict never normalises: any `\` in a finding or content path is `INVALID_REGISTRATION` (`ownership-approval.mjs:165`, `verdict-registration-backslash-path`). Registry roots with `\` are malformed (`:191`).
- **Sub-roots and prefix:** findings in a registered root keep repository-relative paths (`ownership-evidence.mjs:125-129`).
- **Ordering:** content digests are sorted with `String.prototype.localeCompare` (**AMBIGUITY A7**, `scan-path-sort-order-locale-compare`).
- **Non-ASCII and spaces:** git lists paths NUL-separated; decoded as strict UTF-8 (`ownership-git-inventory.mjs:42-44`). `scan-path-space-and-unicode`.
- **Size limits:** code files and protected-provenance files over 2 MiB, manifests over 64 MiB become coverage (`ownership.mjs:196,219`). No fixture materialises a 2 MiB file.

## 9. Ambiguities and defects (Node behaviour kept as-is)

| Id | Finding | Where |
|---|---|---|
| DEFECT-1 | Cached verdict outlives approval expiry (section 4). | `ownership-verdict.mjs:69-74`; `ownership.mjs:622,633`; `ownership-admission.mjs:93-94,119-122` |
| A1 | The committed scan ignores `trustState.enrolled`/`pending`: enrollment is re-derived from the trust object (`hosts` keys or `rootFingerprint`) because `ownership-evidence.mjs:119` passes only `{trust}`. With `enrolled:true` but no `hosts`, `ok` is false and the verdict is `REPORT_FAILED` (observed while building fixtures). | `ownership.mjs:609-615,684-686,750-752`; `ownership-evidence.mjs:119` |
| A2 | Registered roots that exclude `.` (for example `["pkg-a","pkg-b"]`) can never be scanned: the worker scans from the repository root and evidence throws `release app root is outside protected ownership roots: .`. Overlapping registrations (`[".","pkg-a"]`) scan twice and duplicate findings. | `ownership-evidence.mjs:105-116`; `scan-registration-subroots-only-refused-from-repo-root`, `scan-registration-dot-plus-subroot*` |
| A3 | Broker returns the worker's `code` unsanitised on probe/scan error lines, so a non-`RK_OWNERSHIP_` code (`SOMETHING_ELSE`) is surfaced; thrown errors are sanitised (`:103`) and the CLI sanitises (`ownership-scan-worker.mjs:98`). | `ownership-admission.mjs:89,116`; `admission-scan-error-with-code` |
| A4 | Manifest basenames are case-sensitive, extensions are not (section 8). | `ownership.mjs:195,215,348` |
| A5 | Absent-approvals digest differs: worker `sha256({absent:true})` for the cache key (`ownership-scan-worker.mjs:51`) versus `sha256([])` in evidence (`ownership-evidence.mjs:134,137`, `ownership.mjs:742`). Both are internally stable. The worker hashes the working-tree approvals file; the committed scan reads the HEAD blob (`ownership.mjs:735`); the clean-checkout requirement (`ownership-evidence.mjs:47`) hides the difference. | see lines |
| A6 | `validateTrustRegistry` drops earlier problems when `repositories` is not an array (`ownership-approval.mjs:184`). | |
| A7 | `contentInventory`, `contentDigests` (`ownership.mjs:438,553`; `ownership-evidence.mjs:131`) and the content digest depend on ICU `localeCompare`. A port that sorts by bytes or code point produces different digests for mixed-case or punctuated names. | |
| A8 | `ok` and the advisory branch interact: the scanner computes `ok` from `!result.violations.some(isBlockingFinding)` (`ownership.mjs:690,755`), so an advise-only scan has `ok:true` (`scan-advisory-tier`) and verdict step 9 yields `ADVISORY`; the same findings with `ok:false` yield `REPORT_FAILED` because step 8 runs first. Both orders are fixtured; a port must keep step 8 before step 9. | `ownership-verdict.mjs:62-64` |
| A9 | `report.warnings` and `report.warning` are accepted by the verdict but never produced by the worker report; the evidence-level `warning` is returned only inside `checked` and dropped by the worker. | `ownership-verdict.mjs:34`; `ownership-scan-worker.mjs:79-85` |
| A10 | `PENDING_ENROLLMENT` is decided before registration, so a pending report with `trust:null` is not `INVALID_REGISTRATION` (`verdict-pending-null-trust`). | `ownership-verdict.mjs:43` |
| A11 | `finding.line`, `ordinal`, `count`, `sequence` use "safe integer >= 0". JSON `2.0` and `2e0` parse to the integer 2 in JavaScript and must be accepted; `input.json` is normalised so this is only visible in text. | `ownership-verdict.mjs:13` |

## 10. Coverage matrix (case counts)

Totals: 293 cases = verdict 88, scan 38 (real git repos through the real worker code), admission 62 scenarios, cache-key 24, identity 71, transport 10.
Counts below are cases (verdict, scan, cache-key) or evaluation steps (admission) whose primary outcome is that code.

| Code | verdict | scan | admission steps | cache-key |
|---|---|---|---|---|
| CLEAN | 11 | 7 | 28 | 11 (`OK` keys) |
| ADVISORY | 1 | 1 | 2 | |
| MALFORMED_REPORT | 30 | | 5 | |
| PENDING_ENROLLMENT | 3 | 2 | 2 | |
| INVALID_REGISTRATION | 20 | 2 | 5 | |
| POLICY_DOWNGRADE | 5 | 2 | 1 | |
| INCOMPLETE_COVERAGE | 5 | 9 | 6 | |
| VIOLATION | 9 | 9 | 5 | |
| UNVERIFIED | 2 | 3 (error lines) | 8 | |
| REPORT_FAILED | 2 | | 2 | |
| INVALID_INPUTS | | 2 | 11 | 13 |
| INPUTS_CHANGED | | 1 | 2 | |

The test asserts every verdict code is present among verdict cases, and the reachable ones among real-scan cases. Exact counts are derivable from `cases/*/expected.json`.

Mapping of the user-facing scenario list to cases: clean pass `scan-clean-pass`; capability absent `scan-clean-pass` (rows `not-detected`) and declared via owner `scan-declared-dependency-owner`; unapproved new capability `scan-violation-source-unapproved-new-capability`, `scan-violation-cargo-dependency`, `scan-violation-npm-dependency`, `scan-violation-transitive-cargo`; key-authority failure `scan-error-key-authority-malformed`; trust failure `scan-error-trust-location-unverified`; evidence mismatch `scan-error-inputs-changed-during-scan`, `admission-inputs-changed-between-probe-and-scan`, `scan-registration-unregistered-repo`; malformed input `verdict-malformed-*`, `cache-key-*`; path edge cases section 8.

### RustRight port crosswalk (`rustright-ownership/src/tests.rs`, `approval.rs`, `verdict.rs`)

| Rust test | Fixtures |
|---|---|
| `enforce_refuses_*` / report records (8 codes) | `admission-enforce-*`, `admission-report-*` |
| `missing_extra_wrong_typed_and_non_object_reports_fail_closed` | `verdict-malformed-*` (null, array, string, number, extra, missing and wrong type per key, metadata extra, `[null]` coverage/violations, `[1]` problems, finding extra, rows, content digest), `admission-enforce-malformed-report` |
| `shared_classification_*` | `admission-classification-*`, `admission-enforce-violation`, `verdict-pending-null-trust` |
| `catalog_paths_never_confer_owner_exemption_and_missing_roots_refuse` | `admission-repository-root-wins-over-workspace`, `admission-workspace-root-fallback`, `admission-missing-root`, `admission-empty-root-rejected` |
| `cache_invalidates_changed_*` (6 bindings) | `admission-cache-invalidated-by-*`, `cache-key-changed-*`, `cache-key-missing-*` |
| `identical_digests_share_cache_and_in_flight_scan` | `admission-cache-hit-same-inputs`, `admission-single-flight-concurrent`, `admission-cache-no-expiry-by-age` (DEFECT-1: Rust must differ) |
| `incomplete_inventory_never_inherits_cached_or_in_flight_clean_verdict` | `admission-incomplete-inventory-does-not-inherit-cached-clean`, `...-share-in-flight-clean` |
| `protected_pinned_catalog_root_coverage_and_malformed_registry_fail_closed` | `verdict-downgrade-catalog-digest-pin`, `verdict-registration-engine-app-roots-mismatch`, `verdict-registration-null-registry-entry`, `verdict-coverage-incomplete-status` |
| `missing_digests_changed_scan_bindings_and_unavailable_scanners_refuse_failures_retry` | `admission-missing-binding-*`, `admission-inputs-changed-between-probe-and-scan`, `admission-complete-*`, `admission-scan-error-retries`, `admission-scanner-unstaged`, `admission-mode-off` |
| `only_block_tier_or_banned_violations_refuse_advise...` | `verdict-advisory-only`, `verdict-violation-*`, `verdict-malformed-finding-bad-tier` |
| approval.rs identity, path, version tests | `identity-*`, `verdict-registration-*path*`, `verdict-downgrade-*version*` |
| runner.rs worker transport tests | `transport-*` (output-over-cap, 32 MiB, is not fixtured: `ownership-admission.mjs:8,39`) |
| limits.rs, `default_scanner_root_requires_worker_and_catalog`, `cache_day_is_the_utc_date` | Rust-only (bounded workers, scanner discovery, UTC cache day): no Node counterpart |

## 11. Not covered: exception-record cases

By instruction no signed-approval (exception record) fixtures exist, and the fixtures contain no signing keys or signatures. Still uncovered:

1. Valid signed, in-date, tracked approval covering one occurrence: the finding moves to `covered[]`, row `excepted`, verdict `CLEAN`, `approvalsDigest` over a real file.
2. Approval with a payload edited after signing (`invalid signature`), verdict stays `VIOLATION` with `problems`.
3. Expired approval (`invalid or expired expires date` / `approval expired`), clock-dependent, and the DEFECT-1 interaction.
4. Revoked signing key; unknown or not-enrolled host key; wrong host for key id; RSA PSS/PKCS1 and key-log (`rootFingerprint`/`ownershipKeys`) verification.
5. Tracking reference without protected verification evidence.
6. Stale approval (matches no current occurrence) yielding `UNVERIFIED`.
7. Malformed committed approvals file (`invalid-committed-exceptions-json` coverage) and `committed-exceptions-over-2MiB`.
8. Approval schema validation (`validateRecord`, `ownership-approval.mjs:129-162`), duration over 90 days, issued in the future, `minimumScannerVersion`/`minimumCatalogVersion` in the approval, policy digest mismatch, repository identity change, occurrence kind/hash mismatch.
9. Key authority success path (`readCommittedKeyAuthority`, checkpoints): only the malformed-authority failure is fixtured. Key-log verification lives in `ownership-key-log.mjs`, out of this scope.
10. `approvalsDigest` change when an approvals file is present or modified (covered only by `ownership-scan-worker.test.mjs:69`).

Existing unit tests (`ownership-approval.test.mjs`, `ownership-key-authority.test.mjs`, `ownership-key-log.test.mjs`) remain the specification for these until separately approved fixtures exist.

## 12. Rules for consumers

- Compare parsed JSON structurally; object key order in the Node output is not contractual (only `canonicalJson` for hashing).
- `expected.json` for `scan` cases is volatile-pinned, not volatile-free: a consumer that produces its own scan must use the same fixed clock, host string, scanner policy digest and git environment, or compare only the fields the contract defines (`verdict`, `report.violations`, `report.coverage`, `inputs`).
- `scan.verdict` must equal `ownershipVerdict(scan.report, scan.trust)`; the test in `ownership-scan-worker.test.mjs:28` holds the same invariant on the Node side.
