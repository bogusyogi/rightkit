#!/usr/bin/env node
// Build reviewable unsigned records from bounded, first-party ownership scans.
// This utility never reads external trust, signs, imports, or edits app exception files.
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "../../packages/release/background-process.mjs";
import { createDraft, occurrenceHash, policyEntryDigest, sha256, canonicalRepositoryIdentity } from "../../packages/release/ownership-approval.mjs";
import { scanApp, SCANNER_VERSION } from "../../packages/release/ownership.mjs";
import manifest from "../../packages/release/rightkit-ownership.json" with { type: "json" };

const MODULE_DIR = path.dirname(fileURLToPath(import.meta.url));
const WORKSPACE = path.resolve(MODULE_DIR, "../../..");
const LEGACY = path.join(WORKSPACE, "tasks/handoffs/2026-10-07/ownership");
const OUT = MODULE_DIR;
const CODE_EXT = new Set([".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".swift", ".m", ".mm", ".c", ".cc", ".cpp", ".h", ".py", ".go", ".kt", ".java", ".cs"]);
const MANIFESTS = new Set(["Cargo.toml", "package.json", "Package.swift"]);

// Repository identities come from the checked-in standalone consumer declarations.
// The final three were added after that list and their current origin is read-only evidence.
const KNOWN_REMOTES = {
  coderight: "https://github.com/bogusyogi/coderight.git",
  viewright: "https://github.com/bogusyogi/viewright.git",
  scraperight: "https://github.com/bogusyogi/scraperight.git",
  heardright: "https://github.com/bogusyogi/heardright.git",
  mailright: "https://github.com/bogusyogi/mailright.git",
};
const APPS = [
  { key: "coderight", root: "coderight", appDir: "." },
  { key: "viewright", root: "viewright", appDir: "." },
  { key: "scraperight", root: "scraperight", appDir: "." },
  { key: "heardright", root: "heardright", appDir: "tauri-app-next" },
  { key: "mailright", root: "mailright", appDir: "." },
  { key: "cutright", root: "cutright", appDir: "." },
  { key: "genright", root: "genright", appDir: "." },
  { key: "rightsites", root: "rightsites", appDir: "." },
];

function runGit(root, args) {
  const r = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true, maxBuffer: 32 * 1024 * 1024 });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")} failed in ${root}: ${String(r.stderr ?? r.stdout).trim()}`);
  return String(r.stdout).trim();
}
function digestBytes(bytes) { return sha256(bytes); }
function sourceRemote(key, root) {
  const declared = KNOWN_REMOTES[key];
  const observed = runGit(root, ["remote", "get-url", "origin"]);
  const canonicalObserved = canonicalRepositoryIdentity(observed);
  if (declared && canonicalRepositoryIdentity(declared) !== canonicalObserved) throw new Error(`${key}: checked-in repository identity does not match current origin`);
  return { remote: observed, repositoryIdentity: canonicalObserved, identitySource: declared ? "standalone-clone-verify.mjs + git origin readback" : "git origin readback" };
}
function scopedFiles(repoRoot, appDir, backlog) {
  const prefix = appDir === "." ? "" : `${appDir.replaceAll("\\", "/").replace(/\/$/, "")}/`;
  const namedPaths = [...new Set((backlog.exceptions ?? []).flatMap((item) => item.paths ?? []))].sort();
  const selected = [];
  const coverage = [];
  for (const sourcePath of namedPaths) {
    if (sourcePath.startsWith("/") || sourcePath.includes("\\") || sourcePath.split("/").some((x) => !x || x === "." || x === "..")) {
      coverage.push({ path: sourcePath, status: "unscanned", reason: "legacy-path-not-exact-repository-relative" });
      continue;
    }
    if (prefix && !sourcePath.startsWith(prefix)) continue;
    const rel = prefix ? sourcePath.slice(prefix.length) : sourcePath;
    const base = path.posix.basename(rel), ext = path.posix.extname(rel).toLowerCase();
    if (MANIFESTS.has(base) || CODE_EXT.has(ext)) selected.push({ repoPath: sourcePath, rel });
    let parent = path.posix.dirname(rel);
    while (parent !== ".") {
      for (const manifestName of MANIFESTS) {
        const relative = path.posix.join(parent, manifestName);
        const repoPath = `${prefix}${relative}`;
        if (!existsSync(path.resolve(repoRoot, ...repoPath.split("/")))) continue;
        if (!selected.some((item) => item.rel === relative)) selected.push({ repoPath, rel: relative, supportManifest: true });
      }
      parent = path.posix.dirname(parent);
    }
    for (const manifestName of MANIFESTS) {
      const repoPath = `${prefix}${manifestName}`;
      if (!existsSync(path.resolve(repoRoot, ...repoPath.split("/")))) continue;
      if (!selected.some((item) => item.rel === manifestName)) selected.push({ repoPath, rel: manifestName, supportManifest: true });
    }
  }
  const files = [];
  for (const { repoPath } of [...selected]) if (MANIFESTS.has(path.posix.basename(repoPath))) {
    for (const name of ["Cargo.lock", "package-lock.json", "npm-shrinkwrap.json", "pnpm-lock.yaml", "yarn.lock", "Package.resolved"]) {
      const lockPath = path.posix.join(path.posix.dirname(repoPath), name);
      if (existsSync(path.resolve(repoRoot, ...lockPath.split("/"))) && !selected.some((item) => item.repoPath === lockPath)) selected.push({ repoPath: lockPath, rel: prefix ? lockPath.slice(prefix.length) : lockPath });
    }
  }
  for (const { repoPath: posixPath, rel } of selected) {
    const absolute = path.resolve(repoRoot, ...posixPath.split("/"));
    const rootReal = realpathSync(repoRoot);
    let real;
    try { real = realpathSync(absolute); } catch { files.push({ repoPath: posixPath, rel, status: "unreadable", reason: "path-unresolved" }); continue; }
    if (real !== rootReal && !real.startsWith(`${rootReal}${path.sep}`)) {
      files.push({ repoPath: posixPath, rel, status: "unreadable", reason: "symlink-outside-repository" });
      continue;
    }
    let st;
    try { st = lstatSync(absolute); } catch { files.push({ repoPath: posixPath, rel, status: "unreadable", reason: "stat-failed" }); continue; }
    if (st.isDirectory()) continue;
    try {
      const bytes = readFileSync(real);
      files.push({ repoPath: posixPath, rel, bytes, size: bytes.length, digest: digestBytes(bytes), status: "copied" });
    } catch { files.push({ repoPath: posixPath, rel, status: "unreadable", reason: "read-failed" }); }
  }
  return { files: files.sort((a, b) => a.rel.localeCompare(b.rel)), coverage, prefix };
}
function writeScanRoot(files) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rightkit-ownership-migration-"));
  for (const f of files) {
    if (!f.bytes) continue;
    const target = path.resolve(dir, ...f.rel.split("/"));
    if (!target.startsWith(`${dir}${path.sep}`)) throw new Error(`unsafe source path: ${f.rel}`);
    mkdirSync(path.dirname(target), { recursive: true });
    if (existsSync(target)) {
      if (!readFileSync(target).equals(f.bytes)) throw new Error(`different scoped inputs map to the same scanner path: ${f.rel}`);
      continue;
    }
    writeFileSync(target, f.bytes, { flag: "wx" });
  }
  return dir;
}
function appScan(app, timestamp) {
  const repoRoot = path.resolve(WORKSPACE, app.root);
  if (!existsSync(repoRoot)) throw new Error(`${app.key}: repository root missing`);
  const sourceRoot = app.appDir === "." ? repoRoot : path.resolve(repoRoot, app.appDir);
  if (!existsSync(sourceRoot)) throw new Error(`${app.key}: bounded app source root missing`);
  const backlog = JSON.parse(readFileSync(path.join(LEGACY, `${app.key}-ownership-exceptions.json`), "utf8"));
  const remote = sourceRemote(app.key, repoRoot);
  const commit = runGit(repoRoot, ["rev-parse", "HEAD"]);
  const scoped = scopedFiles(repoRoot, app.appDir, backlog);
  const requestedPaths = scoped.files.map((x) => x.repoPath);
  const dirtyPaths = requestedPaths.length ? runGit(repoRoot, ["status", "--porcelain=v1", "--untracked-files=all", "--", ...requestedPaths]).split(/\r?\n/).filter(Boolean) : [];
  const listedFiles = scoped.files;
  const selected = listedFiles.filter((x) => x.bytes);
  const temp = writeScanRoot(selected);
  try {
    const raw = scanApp(temp, manifest, { trust: null });
    const pathPrefix = scoped.prefix;
    const findings = raw.map((finding) => {
      const normalized = { ...finding, file: `${pathPrefix}${finding.file}` };
      normalized.occurrenceHash = occurrenceHash(normalized);
      return normalized;
    });
    const rawMetadata = raw.metadata ?? {};
    const contentDigests = (rawMetadata.contentDigests ?? []).map((x) => ({ ...x, path: `${pathPrefix}${x.path}` }));
    const skipped = listedFiles.filter((x) => !x.bytes).map(({ repoPath, rel, status, reason }) => ({ path: `${pathPrefix}${rel}`, status: "unscanned", reason, sourcePath: repoPath }));
    const coverage = [...new Map([...(raw.coverage ?? []), ...scoped.coverage, ...skipped,
      { path: ".", status: "unscanned", reason: "scan-is-limited-to-exact-paths-in-existing-migration-backlog" },
      ...(existsSync(path.join(sourceRoot, "node_modules")) ? [{ path: `${pathPrefix}node_modules`, status: "unscanned", reason: "node-modules-excluded; first-party-scope-only" }] : []),
    ].map((x) => [`${x.path}\0${x.reason}`, x])).values()];
    return {
      app: app.key,
      appSourceRoot: sourceRoot,
      repositoryRoot: repoRoot,
      ...remote,
      sourceCommit: commit,
      workingTreeDirtyPaths: dirtyPaths,
      scan: {
        scannerVersion: rawMetadata.scannerVersion ?? SCANNER_VERSION,
        catalogVersion: manifest.version ?? String(manifest.schema ?? "1"),
        catalogDigest: rawMetadata.catalogDigest,
        scannedAt: rawMetadata.scannedAt ?? timestamp,
        scope: app.appDir,
        strategy: "existing RightKit scanner over exact repository-relative paths named by the legacy migration backlog, copied byte-for-byte to isolated temporary root; no full-repository coverage claim",
        inputCount: selected.length,
        inputBytes: selected.reduce((n, x) => n + x.size, 0),
        inputDigests: selected.map(({ repoPath, rel, size, digest, supportManifest }) => ({ path: `${pathPrefix}${rel}`, sourceRepositoryPath: repoPath, size, sha256: digest, ...(supportManifest ? { role: "ancestor-workspace-manifest" } : {}) })),
        scannerContentDigests: contentDigests,
        coverage,
        completeRepositoryCoverage: coverage.length === 0,
      },
      findings,
    };
  } finally { rmSync(temp, { recursive: true, force: true }); }
}
function fileName(app, index, fingerprint) { return `${app}-${String(index + 1).padStart(3, "0")}-${fingerprint.replaceAll(".", "_")}.draft.json`; }

export function prepareMigrationDrafts({ outputDir = OUT, apps = APPS, now = new Date(), refresh = false } = {}) {
  const timestamp = now.toISOString();
  const scans = apps.map((app) => appScan(app, timestamp));
  const drafts = [];
  for (const scan of scans) {
    const backlog = JSON.parse(readFileSync(path.join(LEGACY, `${scan.app}-ownership-exceptions.json`), "utf8"));
    for (const [index, legacy] of (backlog.exceptions ?? []).entries()) {
      const paths = Array.isArray(legacy.paths) ? legacy.paths : [];
      const findings = scan.findings.filter((finding) => finding.fingerprint === legacy.fingerprint && paths.includes(finding.file));
      if (!findings.length) continue;
      const record = createDraft({
        repositoryIdentity: scan.repositoryIdentity,
        policyDigest: policyEntryDigest(manifest, legacy.fingerprint),
        minimumScannerVersion: scan.scan.scannerVersion,
        minimumCatalogVersion: scan.scan.catalogVersion,
        fingerprint: legacy.fingerprint,
        findings,
        reason: legacy.reason,
        owner: legacy.owner,
        issued: timestamp.slice(0, 10),
        requestId: legacy.requestId,
        trackingRef: legacy.trackingRef,
      });
      const name = fileName(scan.app, index, legacy.fingerprint);
      drafts.push({ app: scan.app, legacyIndex: index, file: name, legacy, findings, record });
    }
  }
  mkdirSync(outputDir, { recursive: true });
  const draftDir = path.join(outputDir, "drafts");
  mkdirSync(draftDir, { recursive: true });
  for (const item of drafts) {
    const target = path.join(draftDir, item.file);
    if (refresh && existsSync(target) && JSON.parse(readFileSync(target, "utf8")).signature !== null) throw new Error(`refusing to overwrite signed or malformed record: ${item.file}`);
    writeFileSync(target, `${JSON.stringify(item.record, null, 2)}\n`, { flag: refresh ? "w" : "wx" });
  }
  const draftRefs = drafts.map(({ app, legacyIndex, file, legacy, findings, record }) => ({
    app, legacyIndex, file: path.relative(outputDir, path.join(draftDir, file)).replaceAll("\\", "/"),
    fingerprint: legacy.fingerprint, owner: legacy.owner, reason: legacy.reason,
    backlogFile: `tasks/handoffs/2026-10-07/ownership/${app}-ownership-exceptions.json`,
    legacyApprovedByText: legacy.approvedBy ?? null,
    legacyTrackingText: typeof legacy.tracking === "string" ? legacy.tracking : null,
    observedPaths: [...new Set(record.payload.occurrences.map((item) => item.path))].sort(),
    occurrences: record.payload.occurrences,
    occurrenceCount: findings.length,
    missingUserFields: ["expires", "requestId", "trackingRef"].filter((key) => record.payload[key] === undefined || record.payload[key] === null || record.payload[key] === ""),
    signature: record.signature,
  }));
  const manifestOut = {
    schema: 2,
    kind: "unsigned-ownership-migration-draft-set",
    generatedAt: timestamp,
    authority: "review material only; every record remains unsigned and requires Adrian's native review/signing",
    catalog: { version: manifest.version ?? String(manifest.schema ?? "1"), digest: sha256(manifest) },
    sourceEvidence: scans,
    draftCount: draftRefs.length,
    drafts: draftRefs,
    legacyRecordsNotObserved: scans.flatMap((scan) => {
      const backlog = JSON.parse(readFileSync(path.join(LEGACY, `${scan.app}-ownership-exceptions.json`), "utf8"));
      const matched = new Set(drafts.filter((x) => x.app === scan.app).map((x) => x.legacyIndex));
      return (backlog.exceptions ?? []).flatMap((legacy, index) => matched.has(index) ? [] : [{ app: scan.app, legacyIndex: index, fingerprint: legacy.fingerprint, paths: legacy.paths ?? [], reason: "no current finding observed at exact legacy fingerprint/path in bounded scan" }]);
    }),
    pendingFields: {
      requestId: "Adrian must provide an exact RightKit request ID before native signing.",
      trackingRef: "Adrian must provide a verified HTTPS tracking reference before native signing; legacy free-text tracking is evidence only.",
    },
    signingImportVerifySequence: [
      "Review each listed source root, source commit, input digests, exact paths, occurrence hashes, reason, and owner; resolve each pending field.",
      "Save each completed schema-2 record with signature:null, then Adrian launches the native Windows or macOS ownership-approval helper and uses Review approval draft > Sign reviewed draft; native prompt is required per record.",
      "For each signed record, the import utility can preserve it as a separate file: node rightkit/packages/release/ownership-approval-command.mjs --import <signed.json> --out <imported-record.json>; its --import command does not merge application exception files.",
      "After both host keys are enrolled in protected trust, run: node rightkit/packages/release/ownership-approval-command.mjs --verify <signed-record.json> --context <fresh-scanner-context.json> per record; only a successful verifier result proves signature and baseline coverage.",
      "Only after successful verification, Adrian integrates the verified records into each repository's existing .rightkit/ownership-exceptions.json exceptions array, preserving existing entries.",
    ],
    validation: {
      allDraftsUnsigned: draftRefs.every((x) => x.signature === null),
      everyDraftBoundToCurrentObservedOccurrences: drafts.every((x) => x.record.payload.occurrences.map((item) => `${item.path}\0${item.hash}`).sort().join("\n") === x.findings.map((f) => `${f.file}\0${occurrenceHash(f)}`).sort().join("\n")),
      repositoryIdentityMatchesCurrentOrigin: true,
      sourceCommitRecordedPerRepository: scans.every((x) => /^[a-f0-9]{40,64}$/.test(x.sourceCommit)),
    },
  };
  writeFileSync(path.join(outputDir, "prepared-migration-manifest.json"), `${JSON.stringify(manifestOut, null, 2)}\n`, { flag: refresh ? "w" : "wx" });
  return manifestOut;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    const result = prepareMigrationDrafts({ refresh: process.argv.includes("--refresh") });
    process.stdout.write(`${JSON.stringify({ draftCount: result.draftCount, scans: result.sourceEvidence.map((x) => ({ app: x.app, commit: x.sourceCommit, findings: x.findings.length, coverageGaps: x.scan.coverage.length })), manifest: path.join(OUT, "prepared-migration-manifest.json") }, null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`ownership migration draft preparation failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
