#!/usr/bin/env node
// Read-only source validation for a prepared migration draft set.
import { existsSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "../../packages/release/background-process.mjs";
import { canonicalRepositoryIdentity, occurrenceHash, policyEntryDigest, sha256, validateRecord } from "../../packages/release/ownership-approval.mjs";
import manifest from "../../packages/release/rightkit-ownership.json" with { type: "json" };

const MIGRATIONS = path.dirname(fileURLToPath(import.meta.url));
function git(root, args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true, maxBuffer: 4 * 1024 * 1024 });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed in ${root}`);
  return String(result.stdout).trim();
}
function validate() {
  const prepared = JSON.parse(readFileSync(path.join(MIGRATIONS, "prepared-migration-manifest.json"), "utf8"));
  const problems = [];
  if (prepared.schema !== 2) problems.push("prepared migration manifest must use schema 2");
  const repos = new Map();
  for (const source of prepared.sourceEvidence) {
    const root = source.repositoryRoot;
    const identity = canonicalRepositoryIdentity(git(root, ["remote", "get-url", "origin"]));
    if (identity !== source.repositoryIdentity) problems.push(`${source.app}: repository identity changed`);
    if (git(root, ["rev-parse", "HEAD"]) !== source.sourceCommit) problems.push(`${source.app}: HEAD changed`);
    for (const input of source.scan.inputDigests) {
      const absolute = path.resolve(root, ...input.sourceRepositoryPath.split("/"));
      const rootReal = realpathSync(root), real = realpathSync(absolute);
      if (real !== rootReal && !real.startsWith(`${rootReal}${path.sep}`)) { problems.push(`${source.app}: input escaped repository: ${input.path}`); continue; }
      const bytes = readFileSync(real);
      if (bytes.length !== input.size || sha256(bytes) !== input.sha256) problems.push(`${source.app}: source input changed: ${input.path}`);
    }
    repos.set(source.app, source);
  }
  const digest = sha256(manifest);
  if (prepared.catalog.digest !== digest) problems.push("catalog digest differs from current shipped manifest");
  const draftRoot = path.join(MIGRATIONS, "drafts");
  for (const item of prepared.drafts) {
    const file = path.resolve(MIGRATIONS, item.file);
    if (!file.startsWith(`${draftRoot}${path.sep}`) || !existsSync(file)) { problems.push(`draft file missing/outside drafts directory: ${item.file}`); continue; }
    const record = JSON.parse(readFileSync(file, "utf8"));
    const shapeProblems = validateRecord(record);
    if (shapeProblems.length) { problems.push(...shapeProblems.map((p) => `${item.file}: ${p}`)); continue; }
    if (record.signature !== null) problems.push(`${item.file}: draft is not unsigned`);
    if (record.payload.repositoryIdentity !== repos.get(item.app)?.repositoryIdentity) problems.push(`${item.file}: repository identity mismatch`);
    if (record.payload.policyDigest !== policyEntryDigest(manifest, record.payload.fingerprint)) problems.push(`${item.file}: fingerprint policy digest mismatch`);
    const source = repos.get(item.app);
    if (record.payload.minimumScannerVersion !== source?.scan.scannerVersion || record.payload.minimumCatalogVersion !== source?.scan.catalogVersion) problems.push(`${item.file}: draft version floors differ from source scan`);
    if (record.payload.issued !== prepared.generatedAt.slice(0, 10)) problems.push(`${item.file}: issued date differs from preparation date`);
    const duration = Date.parse(record.payload.expires) - Date.parse(record.payload.issued);
    if (!(duration > 0 && duration <= 90 * 86400000) || record.payload.issued > new Date().toISOString().slice(0, 10)) problems.push(`${item.file}: invalid issued/expiry window`);
    const sourceFindings = repos.get(item.app)?.findings ?? [];
    const observed = sourceFindings.filter((finding) => finding.fingerprint === record.payload.fingerprint && record.payload.occurrences.some((item) => item.path === finding.file && item.hash === occurrenceHash(finding) && item.kind === (finding.kind ?? (finding.ecosystem ? "dependency" : "source"))));
    if (observed.length !== record.payload.occurrences.length) problems.push(`${item.file}: occurrence pairs do not match scoped source findings`);
    if (!record.payload.occurrences.every((entry) => item.observedPaths.includes(entry.path))) problems.push(`${item.file}: payload path missing from prepared manifest`);
  }
  const result = {
    ok: problems.length === 0,
    repositoryCount: repos.size,
    draftCount: prepared.draftCount,
    inputFilesRechecked: prepared.sourceEvidence.reduce((n, source) => n + source.scan.inputDigests.length, 0),
    occurrenceSetsRechecked: prepared.drafts.length,
    unsignedDrafts: prepared.drafts.filter((item) => item.signature === null).length,
    problems,
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (!result.ok) process.exitCode = 1;
  return result;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { validate(); }
  catch (error) { process.stderr.write(`migration draft validation failed: ${error.message}\n`); process.exitCode = 1; }
}
