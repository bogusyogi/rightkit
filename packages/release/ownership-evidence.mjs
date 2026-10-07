import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { catalogDigest as scanCatalogDigest, checkApp, MANIFEST_FILE, SCANNER_VERSION } from "./ownership.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, sha256, validateOwnershipFloor, verifyRepositoryRegistration } from "./ownership-approval.mjs";

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
} = {}) {
  const root = path.resolve(repoRoot);
  const manifestBytes = readFileSync(manifestPath);
  const manifest = JSON.parse(manifestBytes.toString("utf8"));
  const trustState = trustLoader();
  if (trustState.protection?.ok !== true) throw new Error("protected ownership trust location was not verified");
  if (!trustState.trust) throw new Error("protected ownership trust registry is missing");
  const commit = git(root, ["rev-parse", "HEAD"], gitRunner);
  const tree = git(root, ["rev-parse", `${commit}^{tree}`], gitRunner);
  const scannerVersion = SCANNER_VERSION;
  const catalogVersion = manifest.version ?? String(manifest.schema ?? "1");
  const catalogDigest = scanCatalogDigest(manifest);
  // Enrollment controls exception-signing authority, not the minimum scanner
  // or catalog accepted by the protected registry. Pending key enrollment
  // remains a typed state while the protected floors still apply.
  const floor = validateOwnershipFloor(trustState.trust, { scannerVersion, catalogVersion, catalogDigest });
  const origin = git(root, ["remote", "get-url", "origin"], gitRunner);
  const repositoryIdentity = canonicalRepositoryIdentity(origin);
  const registrationCheck = verifyRepositoryRegistration(trustState.trust, repositoryIdentity);
  if (!registrationCheck.ok) throw new Error(registrationCheck.problems.join("; "));
  const registration = registrationCheck.registration;
  const requestedRoot = path.relative(root, scanRoot).replaceAll("\\", "/") || ".";
  if (requestedRoot.startsWith("../") || requestedRoot === ".." || path.isAbsolute(requestedRoot)) throw new Error("ownership evidence scan root must be inside repository");
  if (!registration.roots.some((registered) => rootContains(registered, requestedRoot))) throw new Error(`release app root is outside protected ownership roots: ${requestedRoot}`);

  // The release app's manifest/config can be nested, but the external registry
  // defines repository coverage. Scan every protected root so a nested release
  // cannot omit sibling packages or engine source by narrowing scanRoot.
  const scans = registration.roots.map((registered) => {
    const absolute = registered === "." ? root : path.resolve(root, ...registered.split("/"));
    const scanned = scan(absolute, manifest, { trust: trustState.trust, scannerVersion, catalogVersion });
    if (canonicalRepositoryIdentity(scanned.metadata?.repositoryIdentity) !== repositoryIdentity) throw new Error(`ownership evidence: scanner observed a different repository identity under ${registered}`);
    if (scanned.metadata?.commit !== commit) throw new Error(`ownership evidence: scanner observed a different source commit under ${registered}`);
    if (scanned.metadata?.catalogDigest !== catalogDigest) throw new Error(`ownership evidence: scanner observed a different catalog under ${registered}`);
    const repositoryPaths = scanned.metadata?.pathBase === "repository";
    const prefix = registered === "." || repositoryPaths ? "" : `${registered}/`;
    return { root: registered, scanned, prefix };
  });
  const rebase = (entry, prefix) => ({ ...entry, ...(entry.path ? { path: `${prefix}${entry.path}` } : {}), ...(entry.file ? { file: `${prefix}${entry.file}` } : {}) });
  const contentCoverage = scans.flatMap(({ scanned, prefix }) => (scanned.metadata?.coverage ?? []).map((entry) => rebase(entry, prefix)));
  const contentDigests = scans.flatMap(({ scanned, prefix }) => (scanned.metadata?.contentDigests ?? []).map((entry) => ({ ...entry, path: `${prefix}${entry.path}` }))).sort((a, b) => a.path.localeCompare(b.path));
  const contentDigest = sha256(contentDigests);
  const approvals = scans.map(({ root: registered }) => {
    const exceptionsPath = path.join(root, ...(registered === "." ? [] : registered.split("/")), ".rightkit", "ownership-exceptions.json");
    try { return { root: registered, digest: digestFile(exceptionsPath) }; }
    catch (error) { if (error?.code !== "ENOENT") throw error; return { root: registered, digest: sha256([]) }; }
  });
  const approvalsDigest = sha256(approvals);
  const result = {
    ok: scans.every(({ scanned }) => scanned.ok),
    findings: scans.flatMap(({ scanned, prefix }) => (scanned.findings ?? []).map((entry) => rebase(entry, prefix))),
    violations: scans.flatMap(({ scanned, prefix }) => (scanned.violations ?? []).map((entry) => rebase(entry, prefix))),
    problems: scans.flatMap(({ scanned }) => scanned.problems ?? []),
    rows: scans.flatMap(({ scanned, root: registered }) => (scanned.rows ?? []).map((row) => ({ ...row, root: registered }))),
    covered: scans.flatMap(({ scanned, prefix }) => (scanned.covered ?? []).map((entry) => rebase(entry, prefix))),
    coverage: contentCoverage,
    metadata: {
      repositoryIdentity, commit, catalogDigest, contentDigests, coverage: contentCoverage,
      pathBase: "repository", pathPrefix: "",
      roots: scans.map(({ root: registered, scanned }) => ({ root: registered, metadata: scanned.metadata })),
    },
  };
  const evidence = {
    schema: 1,
    repositoryIdentity: canonicalRepositoryIdentity(result.metadata?.repositoryIdentity),
    commit,
    tree,
    catalogDigest,
    catalogVersion,
    scannerVersion,
    approvalsDigest,
    trustEnrolled: trustState.enrolled === true,
    floorOk: floor.ok,
    visibility: registration.visibility,
    registrationDigest: sha256(registration),
    registeredRoots: [...registration.roots],
    contentDigest,
    checkedAt: new Date().toISOString(),
  };
  return { ok: result.ok && floor.ok && contentCoverage.length === 0, evidence, result, floor, contentCoverage };
}

/** Create scan evidence bound to committed source, catalog, and signed exceptions. */
export function createOwnershipEvidence(repoRoot, scanRoot = repoRoot) {
  return createEvidence(repoRoot, { scanRoot });
}

// Injection seam is deliberately named for tests and is not used by release commands.
export function createOwnershipEvidenceForTest(repoRoot, options) {
  return createEvidence(repoRoot, options);
}

/** Entry point for an OS-protected local hook/adapter on private repositories. */
export function runProtectedLocalOwnershipCheck(repoRoot, scanRoot = repoRoot) {
  const checked = createOwnershipEvidence(repoRoot, scanRoot);
  if (checked.evidence.visibility !== "private") throw new Error("protected local ownership adapter only accepts registered private repositories");
  return checked;
}

/** Re-scan exact source and compare every sealed binding before release upload. */
export function verifyOwnershipEvidence(expected, repoRoot, scanRoot = repoRoot) {
  return verifyEvidence(expected, repoRoot, scanRoot, () => createOwnershipEvidence(repoRoot, scanRoot));
}

export function verifyOwnershipEvidenceForTest(expected, repoRoot, options) {
  return verifyEvidence(expected, repoRoot, options.scanRoot ?? repoRoot, () => createEvidence(repoRoot, options));
}

function verifyEvidence(expected, repoRoot, _scanRoot, createCurrent) {
  if (!expected || expected.schema !== 1) return { ok: false, reason: "sealed ownership evidence is missing or malformed" };
  let current;
  try { current = createCurrent(); }
  catch (error) { return { ok: false, reason: error.message }; }
  for (const key of ["repositoryIdentity", "commit", "tree", "catalogDigest", "catalogVersion", "scannerVersion", "approvalsDigest", "visibility", "registrationDigest", "registeredRoots", "contentDigest"]) {
    const unchanged = key === "registeredRoots"
      ? JSON.stringify(expected[key]) === JSON.stringify(current.evidence[key])
      : expected[key] === current.evidence[key];
    if (!unchanged) return { ok: false, reason: `ownership evidence ${key} changed after build` };
  }
  if (!current.ok) return { ok: false, reason: "current ownership scan fails or has incomplete coverage" };
  return { ok: true, evidence: current.evidence };
}
