import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { catalogDigest as scanCatalogDigest, checkApp, checkAppForTest, checkCommittedApp, checkCommittedAppForTest, MANIFEST_FILE, SCANNER_VERSION } from "./ownership.mjs";
import { acquireCommittedSource, assertCommittedCheckoutClean, committedSourceDataForInternalUse } from "./committed-source.mjs";
import { readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { runningPolicyBundle } from "./ownership-policy-bundle.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, resolveKeyLogTrust, sha256, validateOwnershipFloor, verifyRepositoryRegistration } from "./ownership-approval.mjs";
import { isReleaseSource } from "./release-source-brand.mjs";

const digestFile = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

function git(root, args, runner = spawnSync) {
  const result = runner("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  if (result?.status !== 0) throw new Error(`ownership evidence: git ${args.join(" ")} failed`);
  return String(result.stdout).trim();
}

function rootContains(root, candidate) {
  return root === "." || candidate === root || candidate.startsWith(`${root}/`);
}

function createEvidence(repoRoot, {
  scanRoot = repoRoot,
  scope: requestedScope = null,
  manifestPath = path.join(path.dirname(fileURLToPath(import.meta.url)), MANIFEST_FILE),
  scan = checkApp,
  trustLoader = loadTrustedExternal,
  gitRunner = spawnSync,
  committed = false,
  requireCleanCheckout = false,
  allowPendingAdvisory = false,
  policyBundleProvider = runningPolicyBundle,
  // A door source split (release-source-identity.mjs `resolveReleaseSource`, item 7 row 7.04): the evidence binds the SELECTED commit's
  // objects, read from the checkout's object database, and never the checkout's HEAD or status. null (and a checkout-mode source) is today's behaviour.
  releaseSource = null,
} = {}, readAuthority = readCommittedKeyAuthority, scanCommitted = checkCommittedApp) {
  if (releaseSource !== null && !isReleaseSource(releaseSource)) throw new Error("ownership evidence: the release source was not resolved by resolveReleaseSource");
  const snapshotSource = releaseSource?.mode === "snapshot" ? releaseSource : null;
  const root = path.resolve(repoRoot);
  // Real paths on both sides: Git's root is resolved (macOS /private/var), callers may pass /var or /tmp.
  const realPath = (p) => { try { return realpathSync.native(p); } catch { return path.resolve(p); } };
  if (snapshotSource && realPath(root) !== realPath(snapshotSource.checkout)) throw new Error(`ownership evidence: in a door source split the git root is the run's checkout ${snapshotSource.checkout}, not ${root}`);
  // The caller names its app root in the snapshot (its cwd); the committed scan places it by its path inside the repository, which is the
  // same relative path in the checkout. Nothing is read from either: the scan reads committed objects.
  if (snapshotSource) {
    const relative = path.relative(realPath(snapshotSource.snapshot), realPath(scanRoot));
    if (relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative))) scanRoot = path.join(snapshotSource.checkout, relative);
  }
  const scope = requestedScope ? [...new Set(requestedScope)].sort() : null;
  const manifestBytes = readFileSync(manifestPath);
  const manifest = JSON.parse(manifestBytes.toString("utf8"));
  let trustState = trustLoader();
  const pendingWithoutTrust = trustState.pending === true && trustState.enrolled === false && trustState.trust === null
    && trustState.protection?.ok === false && trustState.protection?.reason === "external trust file is absent";
  if (trustState.protection?.ok !== true && !pendingWithoutTrust) throw new Error("protected ownership trust location was not verified");
  if (!trustState.trust && !pendingWithoutTrust) throw new Error("protected ownership trust registry is missing");
  let enrollmentPending = trustState.enrolled !== true;
  // Pending approval enrollment is a reported warning, never a failure: enrollment existed only for retired signed approvals (#262).
  // A valid protected registry with incomplete key enrollment can still drive
  // a useful local backlog scan. It cannot assert a protected source binding or verify
  // approval signatures, so this result is deliberately advisory.
  const useCommittedSource = committed && (!enrollmentPending || Boolean(trustState.trust?.rootFingerprint));
  // In a split the evidence is the selected commit's committed source or nothing: the advisory fallback would scan a mutable tree and
  // read HEAD, which are exactly the reads a split replaces. Fail closed.
  if (snapshotSource && !useCommittedSource) throw new Error("ownership evidence in a door source split needs the committed-source scan; this host's trust enrollment cannot provide it (RK_SOURCE_SPLIT)");
  // The clean-checkout condition guards "bytes scanned (committed blobs) = bytes built (working tree)". A split builds a verified snapshot
  // of the commit instead, so the checkout's cleanliness is irrelevant there (GC:24, GC:156 item 1).
  if (useCommittedSource && requireCleanCheckout && !snapshotSource) assertCommittedCheckoutClean(root);
  const selected = snapshotSource ? { commit: snapshotSource.commit, expectedTree: snapshotSource.tree } : {};
  const source = useCommittedSource ? acquireCommittedSource(root, { ...selected, requiredPaths: [...(trustState.trust?.provenance ?? []).flatMap((record) => record.paths ?? []), ...(trustState.trust?.rootFingerprint && !trustState.trust?.keyAuthority ? [".rightkit/ownership-keys.json"] : [])] }) : null;
  let keyLogEvidence = null;
  if (trustState.trust?.keyAuthority) {
    const resolved = readAuthority(trustState.trust);
    if (!resolved.ok) throw new Error(`protected ownership key authority failed: ${resolved.problems.join("; ")}`);
    trustState = { ...trustState, trust: resolved.resolvedTrust, enrolled: resolved.resolvedTrust.enrollmentComplete, pending: !resolved.resolvedTrust.enrollmentComplete };
    enrollmentPending = trustState.pending;
    keyLogEvidence = { keyLogDigest: resolved.digest, keyCheckpoint: resolved.keyCheckpoint, rootFingerprint: resolved.rootFingerprint, keyAuthority: resolved.authority };
    if (!resolved.resolvedTrust.ownershipKeys.length) throw new Error("protected ownership key authority has no active approval keys");
  } else if (trustState.trust?.rootFingerprint) {
    const keySource = source ?? acquireCommittedSource(root, { ...selected, requiredPaths: [".rightkit/ownership-keys.json"] });
    const sourceData = committedSourceDataForInternalUse(keySource);
    if (!sourceData) throw new Error("protected ownership key log requires authenticated committed source");
    const entry = sourceData.entries.find((item) => item.rel === ".rightkit/ownership-keys.json");
    const resolved = resolveKeyLogTrust(trustState.trust, entry?.bytes);
    if (!resolved.ok) throw new Error(`protected ownership key log failed: ${resolved.problems.join("; ")}`);
    trustState = { ...trustState, trust: resolved.trust, enrolled: resolved.trust.enrollmentComplete, pending: !resolved.trust.enrollmentComplete };
    enrollmentPending = trustState.pending;
    keyLogEvidence = { keyLogDigest: resolved.keyLogDigest, keyCheckpoint: resolved.checkpoint, rootFingerprint: resolved.trust.rootFingerprint };
    if (!resolved.trust.ownershipKeys.length) throw new Error("protected ownership key authority has no active approval keys");
  }
  if (useCommittedSource && requireCleanCheckout && !snapshotSource) assertCommittedCheckoutClean(root, { expectedCommit: source.commit, expectedTree: source.tree });
  if (snapshotSource && (source.commit !== snapshotSource.commit || source.tree !== snapshotSource.tree)) throw new Error("ownership evidence: the acquired committed source is not the selected commit's");
  const commit = useCommittedSource ? source.commit : git(root, ["rev-parse", "HEAD"], gitRunner);
  if (!commit) throw new Error("ownership evidence: scan source did not produce a HEAD commit");
  const tree = useCommittedSource ? source.tree : git(root, ["rev-parse", `${commit}^{tree}`], gitRunner);
  const scannerVersion = SCANNER_VERSION;
  const catalogVersion = manifest.version ?? String(manifest.schema ?? "1");
  const catalogDigest = scanCatalogDigest(manifest);
  // When a protected registry exists, key enrollment controls signing
  // authority; scanner and catalog floors still apply during pending enrollment.
  // Step 2: the protected trust pins the policy bundle (catalog + registry + dependency allowlist + scanner version +
  // scanner source digests). It is fingerprinted and compared for drift only: a pin that differs, or a bundle that cannot be
  // computed, is a warning, never a block (trust-sync is retired).
  let policyBundleDigest;
  let bundleProblem = null;
  if (trustState.trust?.policyBundle?.digest) {
    try { policyBundleDigest = policyBundleProvider({ scannerVersion, manifest }).digest; }
    catch (error) { policyBundleDigest = null; bundleProblem = `ownership policy bundle could not be computed: ${error.message}`; }
  }
  const floor = trustState.trust
    ? validateOwnershipFloor(trustState.trust, { scannerVersion, catalogVersion, catalogDigest, ...(policyBundleDigest !== undefined ? { policyBundleDigest } : {}) })
    : { ok: true, problems: [], warnings: ["protected ownership floor is unavailable: no protected trust registry is enrolled on this host (warning only)"] };
  floor.warnings = [...(floor.warnings ?? []), ...(bundleProblem ? [bundleProblem] : [])];
  const real = realPath;
  const requestedRoot = path.relative(real(root), real(scanRoot)).replaceAll("\\", "/") || ".";
  if (requestedRoot.startsWith("../") || requestedRoot === ".." || path.isAbsolute(requestedRoot)) throw new Error("ownership evidence scan root must be inside repository");
  const evidenceDiagnostics = [];
  let repositoryIdentity = null;
  let registrationCheck = null;
  if (trustState.trust) {
    try {
      const origin = useCommittedSource ? source.origin : git(root, ["remote", "get-url", "origin"], gitRunner);
      repositoryIdentity = canonicalRepositoryIdentity(origin);
      registrationCheck = verifyRepositoryRegistration(trustState.trust, repositoryIdentity);
      if (!registrationCheck.ok) {
        if (!enrollmentPending) throw new Error(registrationCheck.problems.join("; "));
        evidenceDiagnostics.push(...registrationCheck.problems);
      }
    } catch (error) {
      if (!enrollmentPending) throw error;
      evidenceDiagnostics.push(`protected registration unavailable during pending enrollment: ${error.message}`);
      repositoryIdentity = null;
      registrationCheck = null;
    }
  }
  const registration = registrationCheck?.registration ?? null;
  if (scope && !useCommittedSource) throw new Error("a scoped ownership scan requires the committed-source scan; this host's trust enrollment cannot provide it");
  // A scoped scan covers its scope roots; the unscoped scan covers the requested root.
  const coverageTargets = scope ?? [requestedRoot];
  const unregisteredTargets = registration ? coverageTargets.filter((target) => !registration.roots.some((registered) => rootContains(registered, target))) : [];
  const requestedRootRegistered = unregisteredTargets.length === 0;
  for (const target of unregisteredTargets) {
    if (!enrollmentPending) throw new Error(`release app root is outside protected ownership roots: ${target}`);
    evidenceDiagnostics.push(`requested scan root is outside protected ownership roots during pending enrollment: ${target}`);
  }

  // The release app's manifest/config can be nested, but the external registry
  // defines repository coverage. Scan every protected root so a nested release
  // cannot omit sibling packages or engine source by narrowing scanRoot.
  const overlapsScope = (registered) => !scope || scope.some((target) => rootContains(registered, target) || rootContains(target, registered));
  const scanRoots = registration?.roots ? registration.roots.filter(overlapsScope) : [];
  if (scope) { for (const target of unregisteredTargets) if (!scanRoots.includes(target)) scanRoots.push(target); }
  else if (!requestedRootRegistered && !scanRoots.includes(requestedRoot)) scanRoots.push(requestedRoot);
  const scans = (scanRoots.length ? scanRoots : [scope ? "." : requestedRoot]).map((registered) => {
    const absolute = registered === "." ? root : path.resolve(root, ...registered.split("/"));
    const scanned = useCommittedSource
      ? scanCommitted(absolute, manifest, { trust: trustState.trust, expectedCommit: commit, expectedTree: tree, source, ...(scope ? { scope } : {}) })
      : scan(absolute, manifest, { trust: { trust: trustState.trust, enrolled: trustState.enrolled, pending: enrollmentPending }, scannerVersion, catalogVersion, ...(enrollmentPending ? { enrollmentPendingAdvisory: true } : {}) });
    if (repositoryIdentity && canonicalRepositoryIdentity(scanned.metadata?.repositoryIdentity) !== repositoryIdentity) throw new Error(`ownership evidence: scanner observed a different repository identity under ${registered}`);
    if (useCommittedSource && scanned.metadata?.commit !== commit) throw new Error(`ownership evidence: scanner observed a different source commit under ${registered}`);
    if (scanned.metadata?.catalogDigest !== catalogDigest) throw new Error(`ownership evidence: scanner observed a different catalog under ${registered}`);
    if (keyLogEvidence?.keyAuthority && JSON.stringify(scanned.metadata?.keyAuthority) !== JSON.stringify(keyLogEvidence.keyAuthority)) throw new Error(`ownership evidence: scanner observed a different shared key authority under ${registered}`);
    const repositoryPaths = scanned.metadata?.pathBase === "repository";
    const prefix = registered === "." || repositoryPaths ? "" : `${registered}/`;
    return { root: registered, scanned, prefix };
  });
  const rebase = (entry, prefix) => ({ ...entry, ...(entry.path ? { path: `${prefix}${entry.path}` } : {}), ...(entry.file ? { file: `${prefix}${entry.file}` } : {}) });
  const contentCoverage = scans.flatMap(({ scanned, prefix }) => (scanned.metadata?.coverage ?? []).map((entry) => rebase(entry, prefix)));
  const contentDigests = scans.flatMap(({ scanned, prefix }) => (scanned.metadata?.contentDigests ?? []).map((entry) => ({ ...entry, path: `${prefix}${entry.path}` }))).sort((a, b) => a.path.localeCompare(b.path));
  const contentDigest = sha256(contentDigests);
  const approvals = scans.map(({ root: registered, scanned }) => {
    if (useCommittedSource) return { root: registered, digest: scanned.metadata?.exceptionDigest ?? sha256([]) };
    const exceptionsPath = path.join(root, ...(registered === "." ? [] : registered.split("/")), ".rightkit", "ownership-exceptions.json");
    try { return { root: registered, digest: digestFile(exceptionsPath) }; }
    catch (error) { if (error?.code !== "ENOENT") throw error; return { root: registered, digest: sha256([]) }; }
  });
  const approvalsDigest = enrollmentPending ? null : sha256(approvals);
  const resultWarnings = [...scans.flatMap(({ scanned, root: registered }) => (scanned.warnings ?? []).filter((w) => w.startsWith("exception #")).map((w) => (registered === "." ? w : `${registered}: ${w}`))), ...evidenceDiagnostics];
  const result = {
    ok: scans.every(({ scanned }) => scanned.ok),
    findings: scans.flatMap(({ scanned, prefix }) => (scanned.findings ?? []).map((entry) => rebase(entry, prefix))),
    violations: scans.flatMap(({ scanned, prefix }) => (scanned.violations ?? []).map((entry) => rebase(entry, prefix))),
    problems: scans.flatMap(({ scanned }) => scanned.problems ?? []),
    rows: scans.flatMap(({ scanned, root: registered }) => (scanned.rows ?? []).map((row) => ({ ...row, root: registered }))),
    covered: scans.flatMap(({ scanned, prefix }) => (scanned.covered ?? []).map((entry) => rebase(entry, prefix))),
    // Stale plain exception entries (match no finding) are warnings, never blockers (Orthic-Labs/rightkit#262).
    ...(resultWarnings.length ? { warnings: resultWarnings } : {}),
    coverage: contentCoverage,
    metadata: {
      repositoryIdentity, commit, tree, catalogDigest, contentDigests, coverage: contentCoverage,
      pathBase: "repository", pathPrefix: "",
      roots: scans.map(({ root: registered, scanned }) => ({ root: registered, metadata: scanned.metadata })),
    },
  };
  const evidence = {
    schema: 1,
    repositoryIdentity: repositoryIdentity ? canonicalRepositoryIdentity(result.metadata?.repositoryIdentity) : null,
    source: enrollmentPending ? "enrollment-pending-advisory" : useCommittedSource ? "committed-git-tree" : "mutable-checkout-test-seam",
    commit,
    ...(enrollmentPending ? {} : { tree }),
    catalogDigest,
    catalogVersion,
    scannerVersion,
    approvalsDigest,
    trustEnrolled: !enrollmentPending,
    ...(keyLogEvidence ?? {}),
    floorOk: floor.ok,
    visibility: registration?.visibility ?? null,
    registrationDigest: registration ? sha256(registration) : null,
    ...(scope ? { ownershipScope: scope, ownershipScopeClosed: [...new Set(scans.flatMap(({ scanned }) => scanned.metadata?.ownershipScopeClosed ?? scope))].sort() } : {}),
    registeredRoots: registration ? [...registration.roots] : [],
    contentDigest: enrollmentPending ? null : contentDigest,
    checkedAt: new Date().toISOString(),
  };
  const warning = enrollmentPending ? "WARNING: ownership approval enrollment is pending (warning only: exceptions are plain git-reviewed entries); the advisory evidence asserts no protected source binding" : undefined;
  return { ok: result.ok && floor.ok && contentCoverage.length === 0, ...(enrollmentPending ? { enrollmentPending: true, warning } : {}), ...(floor.warnings.length ? { warnings: floor.warnings } : {}), evidence, result, floor, contentCoverage };
}

/** Create scan evidence bound to committed source, catalog, and signed exceptions. */
export function createOwnershipEvidence(repoRoot, scanRoot = repoRoot, scope = null, releaseSource = null) {
  return createEvidence(repoRoot, { scanRoot, scope, committed: true, requireCleanCheckout: true, allowPendingAdvisory: true, releaseSource });
}

/** Alias spelling makes the immutable-source contract explicit at adapter boundaries. */
export function createCommittedOwnershipEvidence(repoRoot, scanRoot = repoRoot, scope = null, releaseSource = null) {
  return createEvidence(repoRoot, { scanRoot, scope, committed: true, requireCleanCheckout: true, releaseSource });
}

// Injection seam is deliberately named for tests and is not used by release commands.
export function createOwnershipEvidenceForTest(repoRoot, options) {
  const reader = options?.readAuthorityForTest;
  return createEvidence(repoRoot, { ...options, scan: options?.scan ?? ((root, manifest, context) => checkAppForTest(root, manifest, { ...context, readAuthorityForTest: reader })) }, reader,
    (root, manifest, context) => checkCommittedAppForTest(root, manifest, { ...context, readAuthorityForTest: reader }));
}

/** Test-only access to the production committed-tree evidence path. */
export function createCommittedOwnershipEvidenceForTest(repoRoot, options = {}) {
  return createOwnershipEvidenceForTest(repoRoot, { ...options, committed: true });
}

/** Entry point for an OS-protected local hook/adapter on private repositories. */
export function runProtectedLocalOwnershipCheck(repoRoot, scanRoot = repoRoot) {
  const checked = createCommittedOwnershipEvidence(repoRoot, scanRoot);
  if (checked.evidence.visibility !== "private") throw new Error("protected local ownership adapter only accepts registered private repositories");
  return checked;
}

/** Re-scan exact source and compare every sealed binding before release upload. */
export function verifyOwnershipEvidence(expected, repoRoot, scanRoot = repoRoot, scope = null, releaseSource = null) {
  return verifyEvidence(expected, repoRoot, scanRoot, () => createCommittedOwnershipEvidence(repoRoot, scanRoot, scope, releaseSource), scope);
}

export function verifyOwnershipEvidenceForTest(expected, repoRoot, options) {
  // Re-scan the way the evidence was made: committed-git-tree evidence (always the case in a door source split) through the
  // committed-source scan, as verifyOwnershipEvidence does in production.
  const rescan = expected?.source === "committed-git-tree" ? createCommittedOwnershipEvidenceForTest : createOwnershipEvidenceForTest;
  return verifyEvidence(expected, repoRoot, options.scanRoot ?? repoRoot, () => rescan(repoRoot, options), options.scope ?? null);
}

function verifyEvidence(expected, repoRoot, _scanRoot, createCurrent, scope = null) {
  if (!expected || expected.schema !== 1) return { ok: false, reason: "sealed ownership evidence is missing or malformed" };
  // The sealed scope must equal what the current config yields; the re-scan below uses that same scope.
  const wanted = scope ? [...new Set(scope)].sort() : null;
  if (JSON.stringify(expected.ownershipScope ?? null) !== JSON.stringify(wanted)) return { ok: false, reason: "ownership evidence ownershipScope changed after build" };
  let current;
  try { current = createCurrent(); }
  catch (error) { return { ok: false, reason: error.message }; }
  for (const key of ["repositoryIdentity", "source", "commit", "tree", "catalogDigest", "catalogVersion", "scannerVersion", "approvalsDigest", "keyLogDigest", "keyCheckpoint", "rootFingerprint", "keyAuthority", "visibility", "registrationDigest", "registeredRoots", "contentDigest", "ownershipScopeClosed"]) {
    const unchanged = key === "registeredRoots" || key === "ownershipScopeClosed" || key === "keyCheckpoint" || key === "keyAuthority"
      ? JSON.stringify(expected[key]) === JSON.stringify(current.evidence[key])
      : expected[key] === current.evidence[key];
    if (!unchanged) return { ok: false, reason: `ownership evidence ${key} changed after build` };
  }
  if (!current.ok) return { ok: false, reason: "current ownership scan fails or has incomplete coverage" };
  return { ok: true, evidence: current.evidence };
}
