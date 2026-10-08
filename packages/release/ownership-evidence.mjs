import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { catalogDigest as scanCatalogDigest, checkApp, checkCommittedApp, MANIFEST_FILE, SCANNER_VERSION } from "./ownership.mjs";
import { acquireCommittedSource, assertCommittedCheckoutClean, committedSourceDataForInternalUse } from "./committed-source.mjs";
import { readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, resolveKeyLogTrust, sha256, validateOwnershipFloor, verifyRepositoryRegistration } from "./ownership-approval.mjs";

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
  manifestPath = path.join(path.dirname(fileURLToPath(import.meta.url)), MANIFEST_FILE),
  scan = checkApp,
  trustLoader = loadTrustedExternal,
  gitRunner = spawnSync,
  committed = false,
  requireCleanCheckout = false,
  allowPendingAdvisory = false,
} = {}) {
  const root = path.resolve(repoRoot);
  const manifestBytes = readFileSync(manifestPath);
  const manifest = JSON.parse(manifestBytes.toString("utf8"));
  let trustState = trustLoader();
  const pendingWithoutTrust = trustState.pending === true && trustState.enrolled === false && trustState.trust === null
    && trustState.protection?.ok === false && trustState.protection?.reason === "external trust file is absent";
  if (trustState.protection?.ok !== true && !pendingWithoutTrust) throw new Error("protected ownership trust location was not verified");
  if (!trustState.trust && !pendingWithoutTrust) throw new Error("protected ownership trust registry is missing");
  let enrollmentPending = trustState.enrolled !== true;
  if (committed && enrollmentPending && !allowPendingAdvisory && !trustState.trust?.rootFingerprint) throw new Error("protected ownership evidence requires both distinct, unrevoked approval keys to be enrolled");
  // A valid protected registry with incomplete key enrollment can still drive
  // a useful local backlog scan. It cannot assert a protected source binding or verify
  // approval signatures, so this result is deliberately advisory.
  const useCommittedSource = committed && (!enrollmentPending || Boolean(trustState.trust?.rootFingerprint));
  if (useCommittedSource && requireCleanCheckout) assertCommittedCheckoutClean(root);
  const source = useCommittedSource ? acquireCommittedSource(root, { requiredPaths: [...(trustState.trust?.provenance ?? []).flatMap((record) => record.paths ?? []), ...(trustState.trust?.rootFingerprint && !trustState.trust?.keyAuthority ? [".rightkit/ownership-keys.json"] : [])] }) : null;
  let keyLogEvidence = null;
  if (trustState.trust?.keyAuthority) {
    const resolved = readCommittedKeyAuthority(trustState.trust);
    if (!resolved.ok) throw new Error(`protected ownership key authority failed: ${resolved.problems.join("; ")}`);
    trustState = { ...trustState, trust: resolved.resolvedTrust, enrolled: resolved.resolvedTrust.enrollmentComplete, pending: !resolved.resolvedTrust.enrollmentComplete };
    enrollmentPending = trustState.pending;
    keyLogEvidence = { keyLogDigest: resolved.digest, keyCheckpoint: resolved.keyCheckpoint, rootFingerprint: resolved.rootFingerprint, keyAuthority: resolved.authority };
    if (!resolved.resolvedTrust.ownershipKeys.length) throw new Error("protected ownership key authority has no active approval keys");
  } else if (trustState.trust?.rootFingerprint) {
    const keySource = source ?? acquireCommittedSource(root, { requiredPaths: [".rightkit/ownership-keys.json"] });
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
  if (useCommittedSource && requireCleanCheckout) assertCommittedCheckoutClean(root, { expectedCommit: source.commit, expectedTree: source.tree });
  const commit = useCommittedSource ? source.commit : git(root, ["rev-parse", "HEAD"], gitRunner);
  if (!commit) throw new Error("ownership evidence: scan source did not produce a HEAD commit");
  const tree = useCommittedSource ? source.tree : git(root, ["rev-parse", `${commit}^{tree}`], gitRunner);
  const scannerVersion = SCANNER_VERSION;
  const catalogVersion = manifest.version ?? String(manifest.schema ?? "1");
  const catalogDigest = scanCatalogDigest(manifest);
  // When a protected registry exists, key enrollment controls signing
  // authority; scanner and catalog floors still apply during pending enrollment.
  const floor = trustState.trust
    ? validateOwnershipFloor(trustState.trust, { scannerVersion, catalogVersion, catalogDigest })
    : { ok: false, problems: ["protected ownership floor is unavailable until trust registry enrollment"] };
  // Real paths on both sides: Git's root is resolved (macOS /private/var), callers may pass /var or /tmp.
  const real = (p) => { try { return realpathSync(p); } catch { return path.resolve(p); } };
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
  const requestedRootRegistered = registration?.roots.some((registered) => rootContains(registered, requestedRoot)) ?? false;
  if (registration && !requestedRootRegistered) {
    if (!enrollmentPending) throw new Error(`release app root is outside protected ownership roots: ${requestedRoot}`);
    evidenceDiagnostics.push(`requested scan root is outside protected ownership roots during pending enrollment: ${requestedRoot}`);
  }

  // The release app's manifest/config can be nested, but the external registry
  // defines repository coverage. Scan every protected root so a nested release
  // cannot omit sibling packages or engine source by narrowing scanRoot.
  const scanRoots = registration?.roots ? [...registration.roots] : [];
  if (!requestedRootRegistered && !scanRoots.includes(requestedRoot)) scanRoots.push(requestedRoot);
  const scans = (scanRoots.length ? scanRoots : [requestedRoot]).map((registered) => {
    const absolute = registered === "." ? root : path.resolve(root, ...registered.split("/"));
    const scanned = useCommittedSource
      ? checkCommittedApp(absolute, manifest, { trust: trustState.trust, expectedCommit: commit, expectedTree: tree, source })
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
  const result = {
    ok: scans.every(({ scanned }) => scanned.ok),
    findings: scans.flatMap(({ scanned, prefix }) => (scanned.findings ?? []).map((entry) => rebase(entry, prefix))),
    violations: scans.flatMap(({ scanned, prefix }) => (scanned.violations ?? []).map((entry) => rebase(entry, prefix))),
    problems: [...scans.flatMap(({ scanned }) => scanned.problems ?? []), ...evidenceDiagnostics, ...(enrollmentPending ? floor.problems : [])],
    rows: scans.flatMap(({ scanned, root: registered }) => (scanned.rows ?? []).map((row) => ({ ...row, root: registered }))),
    covered: scans.flatMap(({ scanned, prefix }) => (scanned.covered ?? []).map((entry) => rebase(entry, prefix))),
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
    registeredRoots: registration ? [...registration.roots] : [],
    contentDigest: enrollmentPending ? null : contentDigest,
    checkedAt: new Date().toISOString(),
  };
  const warning = enrollmentPending ? "WARNING: ownership approval enrollment is pending; findings remain backlog diagnostics and no protected evidence or approvals are asserted" : undefined;
  return { ok: !enrollmentPending && result.ok && floor.ok && contentCoverage.length === 0, ...(enrollmentPending ? { enrollmentPending: true, warning } : {}), evidence, result, floor, contentCoverage };
}

/** Create scan evidence bound to committed source, catalog, and signed exceptions. */
export function createOwnershipEvidence(repoRoot, scanRoot = repoRoot) {
  return createEvidence(repoRoot, { scanRoot, committed: true, requireCleanCheckout: true, allowPendingAdvisory: true });
}

/** Alias spelling makes the immutable-source contract explicit at adapter boundaries. */
export function createCommittedOwnershipEvidence(repoRoot, scanRoot = repoRoot) {
  return createEvidence(repoRoot, { scanRoot, committed: true, requireCleanCheckout: true });
}

// Injection seam is deliberately named for tests and is not used by release commands.
export function createOwnershipEvidenceForTest(repoRoot, options) {
  return createEvidence(repoRoot, options);
}

/** Test-only access to the production committed-tree evidence path. */
export function createCommittedOwnershipEvidenceForTest(repoRoot, options = {}) {
  return createEvidence(repoRoot, { ...options, committed: true });
}

/** Entry point for an OS-protected local hook/adapter on private repositories. */
export function runProtectedLocalOwnershipCheck(repoRoot, scanRoot = repoRoot) {
  const checked = createCommittedOwnershipEvidence(repoRoot, scanRoot);
  if (checked.evidence.visibility !== "private") throw new Error("protected local ownership adapter only accepts registered private repositories");
  return checked;
}

/** Re-scan exact source and compare every sealed binding before release upload. */
export function verifyOwnershipEvidence(expected, repoRoot, scanRoot = repoRoot) {
  return verifyEvidence(expected, repoRoot, scanRoot, () => createCommittedOwnershipEvidence(repoRoot, scanRoot));
}

export function verifyOwnershipEvidenceForTest(expected, repoRoot, options) {
  return verifyEvidence(expected, repoRoot, options.scanRoot ?? repoRoot, () => createEvidence(repoRoot, options));
}

function verifyEvidence(expected, repoRoot, _scanRoot, createCurrent) {
  if (!expected || expected.schema !== 1) return { ok: false, reason: "sealed ownership evidence is missing or malformed" };
  let current;
  try { current = createCurrent(); }
  catch (error) { return { ok: false, reason: error.message }; }
  for (const key of ["repositoryIdentity", "source", "commit", "tree", "catalogDigest", "catalogVersion", "scannerVersion", "approvalsDigest", "keyLogDigest", "keyCheckpoint", "rootFingerprint", "keyAuthority", "visibility", "registrationDigest", "registeredRoots", "contentDigest"]) {
    const unchanged = key === "registeredRoots" || key === "keyCheckpoint" || key === "keyAuthority"
      ? JSON.stringify(expected[key]) === JSON.stringify(current.evidence[key])
      : expected[key] === current.evidence[key];
    if (!unchanged) return { ok: false, reason: `ownership evidence ${key} changed after build` };
  }
  if (!current.ok) return { ok: false, reason: "current ownership scan fails or has incomplete coverage" };
  return { ok: true, evidence: current.evidence };
}
