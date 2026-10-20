// Postpublication proof: real npm resolution, never installation from a sealed/local tarball.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { declaredEntries, npmCliCommand } from "./consume.mjs";

export const APPROVED = Object.freeze({
  repository: "bogusyogi/rightkit", runId: "39403051223", attempt: 1,
  commit: "b2bfa82c3be12c3bd1b8a7ac19d89e07bdd68f85",
  name: "@rightkit/react-shell", version: "0.7.2",
  archiveSha256: "b0d6f495eaccfafaa5191d9c502c07a11cae84b58572597a10b69055103a8809",
  receiptSha256: "949ecb832ad3407dace39c071255cc90ac305853b59e0eaf0dcfa086450d750d",
});
const sha = (bytes, algorithm = "sha256", encoding = "hex") => createHash(algorithm).update(bytes).digest(encoding);
const check = (condition, message) => { if (!condition) throw new Error(message); };
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
const registry = "https://registry.npmjs.org";

export function authenticatePublication(run, jobs, runId, repository) {
  check(runId === APPROVED.runId && repository === APPROVED.repository, "publication is outside reviewed allowlist");
  check(String(run.id) === runId && run.repository?.full_name === repository && run.head_repository?.full_name === repository, "run repository/identity mismatch");
  check(run.path === ".github/workflows/publish.yml" && run.event === "workflow_dispatch" && run.head_branch === "main" && run.head_sha === APPROVED.commit, "publication workflow/source mismatch");
  check(run.status === "completed" && run.conclusion === "success" && run.run_attempt === APPROVED.attempt, "publication run did not succeed at pinned attempt");
  check(jobs.total_count === jobs.jobs?.length, "incomplete publication jobs response");
  const upload = jobs.jobs.filter((job) => job.name === "upload");
  check(upload.length === 1 && upload[0].conclusion === "success" && upload[0].status === "completed" && upload[0].run_attempt === APPROVED.attempt, "successful upload job required");
  check(upload[0].steps?.some((step) => step.name === "Upload to npm" && step.conclusion === "success"), "successful npm upload step required");
}

export function validatePublication(receiptBytes, manifest) {
  check(sha(receiptBytes) === APPROVED.receiptSha256, "publication receipt hash mismatch");
  const receipt = JSON.parse(receiptBytes);
  for (const doc of [receipt, manifest]) {
    check(doc.schema === 1 && doc.registry === "npm" && doc.repository === APPROVED.repository && doc.commit === APPROVED.commit && String(doc.runId) === APPROVED.runId && String(doc.runAttempt) === String(APPROVED.attempt), "publication provenance mismatch");
  }
  check(receipt.dryRun === false && receipt.receipts?.length === 1 && manifest.units?.length === 1, "exact single published unit required");
  const published = receipt.receipts[0]; const unit = manifest.units[0];
  check(published.unit === APPROVED.name && published.version === APPROVED.version && published.published === true && published.registryVerified === true && published.outcome === "published" && published.dryRun === false && published.commit === APPROVED.commit && published.registry === "npm" && published.attempt === String(APPROVED.attempt), "published entry mismatch");
  check(unit.name === published.unit && unit.version === published.version && unit.file === published.artifact && unit.sha256 === APPROVED.archiveSha256 && published.sha256 === APPROVED.archiveSha256, "manifest/receipt archive identity mismatch");
  check(/^sha512-[A-Za-z0-9+/]+={0,2}$/.test(unit.integrity), "sealed sha512 integrity required");
  return unit;
}

export function registryUrl(value) {
  const url = new URL(value);
  check(url.origin === registry && !url.username && !url.password && !url.search && !url.hash, "non-public npm registry URL");
  return url.href;
}

export function lockEvidence(lock, unit, metadata) {
  check(lock.lockfileVersion === 3 && lock.packages && !lock.packages[""]?.overrides, "npm v3 lock required");
  check(lock.packages[""].dependencies?.[unit.name] === unit.version, "consumer must request exact registry version");
  for (const [name, item] of Object.entries(lock.packages)) {
    if (!name) continue;
    check(!item.link && typeof item.resolved === "string", `non-registry lock entry ${name}`);
    registryUrl(item.resolved);
    check(/^sha512-[A-Za-z0-9+/]+={0,2}$/.test(item.integrity), `missing sha512 integrity ${name}`);
  }
  const item = lock.packages[`node_modules/${unit.name}`];
  check(item?.version === unit.version && item.resolved === metadata.dist.tarball && item.integrity === metadata.dist.integrity && item.integrity === unit.integrity, "installed lock differs from registry/sealed identity");
  return { name: unit.name, version: item.version, resolved: item.resolved, integrity: item.integrity, entriesChecked: Object.keys(lock.packages).length - 1 };
}

async function get(url, json = true) {
  const response = await fetch(url, { redirect: "error", signal: AbortSignal.timeout(60_000), headers: { "User-Agent": "rightkit-registry-readback", Accept: json ? "application/json" : "application/octet-stream" } });
  check(response.ok, `fetch failed ${response.status}: ${url}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  check(bytes.length <= (json ? 8 : 32) * 1024 * 1024, "registry response exceeds bounded size");
  return json ? JSON.parse(bytes) : bytes;
}

function bundleEvidence(bundle) {
  const receipt = fs.readFileSync(path.join(bundle, "publication-receipt.json"));
  const manifestBytes = fs.readFileSync(path.join(bundle, "manifest.json"));
  const unit = validatePublication(receipt, JSON.parse(manifestBytes));
  authenticatePublication(read(path.join(bundle, "publication-run.json")), read(path.join(bundle, "publication-jobs.json")), APPROVED.runId, APPROVED.repository);
  return { unit, manifestSha256: sha(manifestBytes) };
}

const smoke = `import assert from 'node:assert/strict';
import fs from 'node:fs';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import * as shell from '@rightkit/react-shell/react';
for (const name of ['Table','TableCaption','TableHead','TableBody','TableRow','TableHeaderCell','TableCell','TableMessage']) assert.ok(shell[name], name);
const e = React.createElement;
const html = renderToStaticMarkup(e(shell.Table, {label:'Registry readback',dense:true}, e(shell.TableBody,null,e(shell.TableRow,null,e(shell.TableCell,{numeric:true},'42')))));
assert.match(html, /<table[^>]*aria-label="Registry readback"/);
assert.match(html, /rk-table--dense/); assert.match(html, /<td[^>]*>42<\\/td>/);
const plan = JSON.parse(fs.readFileSync('entries.json','utf8'));
const entries=[];
for(const item of plan.subpaths) { const resolved = import.meta.resolve(item.spec); if(item.runtime.some(f => /\\.[cm]?js$/.test(f))) await import(item.spec); entries.push({spec:item.spec,resolved}); }
fs.writeFileSync('runtime.json', JSON.stringify({html,entries}));
`;
const typeSmoke = `import type { ComponentProps } from 'react';
import { Table, TableBody, TableRow, TableCell, TableCaption, TableHead, TableHeaderCell, TableMessage } from '@rightkit/react-shell/react';
import * as headless from '@rightkit/react-shell/headless';
const props: ComponentProps<typeof Table> = { label: 'Registry readback', dense: true, children: 'ready' };
const exports = [TableBody, TableRow, TableCell, TableCaption, TableHead, TableHeaderCell, TableMessage];
void props; void exports; void headless;
`;

export async function consume(bundle, output, env = process.env) {
  check(env.GITHUB_ACTIONS === "true", "execution requires GitHub CI");
  fs.mkdirSync(output, { recursive: true });
  const report = { schema: 1, kind: "postpublication-registry-readback", ...APPROVED, platform: process.platform, arch: process.arch, node: process.version, readbackRunId: env.GITHUB_RUN_ID, readbackSourceCommit: env.GITHUB_SHA, runnerOS: env.RUNNER_OS, verified: false, commands: [] };
  try {
    const { unit, manifestSha256 } = bundleEvidence(bundle); report.manifestSha256 = manifestSha256;
    const metadata = await get(`${registry}/${encodeURIComponent(unit.name)}/${unit.version}`);
    check(metadata.name === unit.name && metadata.version === unit.version && metadata.dist?.integrity === unit.integrity, "registry metadata differs from sealed package");
    const tarball = registryUrl(metadata.dist.tarball);
    const archive = await get(tarball, false);
    check(sha(archive) === unit.sha256 && `sha512-${sha(archive, "sha512", "base64")}` === unit.integrity, "registry tarball differs from sealed archive");
    write(path.join(output, "registry.json"), metadata);
    report.registryArchiveSha256 = sha(archive);
    const scratch = fs.mkdtempSync(path.join(path.resolve(env.RUNNER_TEMP), "rk-npm-readback-"));
    const consumer = path.join(scratch, "consumer"); fs.mkdirSync(consumer);
    const config = path.join(scratch, "npmrc"); const globalconfig = path.join(scratch, "global-npmrc");
    fs.writeFileSync(config, ""); fs.writeFileSync(globalconfig, "");
    // Allowlisted OS environment: no npm credentials, preload hooks, injected registry/config or GitHub tokens.
    const childEnv = Object.fromEntries(Object.entries(env).filter(([key]) => /^(?:PATH|PATHEXT|SYSTEMROOT|WINDIR|COMSPEC|TEMP|TMP|TMPDIR|HOME|USERPROFILE|APPDATA|LOCALAPPDATA|SYSTEMDRIVE|PROCESSOR_ARCHITECTURE|LANG|LC_ALL)$/i.test(key)));
    Object.assign(childEnv, { NODE_ENV: "production", NODE_OPTIONS: "", npm_config_userconfig: config, npm_config_globalconfig: globalconfig, npm_config_cache: path.join(scratch, "cache"), npm_config_registry: registry, npm_config_update_notifier: "false", npm_config_fetch_retries: "0", npm_config_fetch_timeout: "60000" });
    const run = (command, args, shell = false) => {
      const result = spawnSync(command, args, { cwd: consumer, env: childEnv, encoding: "utf8", timeout: 5 * 60_000, maxBuffer: 16 * 1024 * 1024, shell, windowsHide: true });
      report.commands.push({ command, args, status: result.status, stdout: result.stdout, stderr: result.stderr });
      check(!result.error && result.status === 0, `consumer command failed: ${result.stderr || result.error?.message || result.status}`);
    };
    // These public React 19/TypeScript pins satisfy published >=18 <21 peer contract.
    write(path.join(consumer, "package.json"), { name: "registry-readback", version: "0.0.0", private: true, type: "module", dependencies: { [unit.name]: unit.version, react: "19.1.1", "react-dom": "19.1.1", "@types/react": "19.1.10", "@types/react-dom": "19.1.9", typescript: "5.9.2" } });
    const npm = npmCliCommand();
    run(npm.command, [...npm.prefix, "install", "--ignore-scripts", "--no-audit", "--no-fund", "--registry", registry], npm.shell);
    const lockBytes = fs.readFileSync(path.join(consumer, "package-lock.json"));
    fs.copyFileSync(path.join(consumer, "package-lock.json"), path.join(output, "package-lock.json"));
    fs.copyFileSync(path.join(consumer, "package.json"), path.join(output, "package.json"));
    report.lockEvidence = lockEvidence(JSON.parse(lockBytes), unit, metadata);
    report.lockSha256 = sha(lockBytes);
    const dir = path.join(consumer, "node_modules", ...unit.name.split("/")); const installed = read(path.join(dir, "package.json"));
    check(installed.name === unit.name && installed.version === unit.version, "installed package identity mismatch");
    report.installed = { name: installed.name, version: installed.version };
    const entries = declaredEntries(installed);
    check(entries.hasEntry && entries.patterns.length === 0, "explicit entry targets required");
    for (const file of entries.files) {
      const target = path.resolve(dir, file); const relative = path.relative(dir, target);
      check(relative && !relative.startsWith("..") && !path.isAbsolute(relative) && fs.statSync(target).isFile(), `invalid/missing installed export target ${file}`);
    }
    report.exportTargets = entries.files;
    write(path.join(consumer, "entries.json"), entries);
    fs.writeFileSync(path.join(consumer, "smoke.mjs"), smoke);
    fs.writeFileSync(path.join(consumer, "consumer.ts"), typeSmoke);
    run(process.execPath, ["smoke.mjs"]);
    run(process.execPath, ["node_modules/typescript/bin/tsc", "--noEmit", "--strict", "--module", "NodeNext", "--moduleResolution", "NodeNext", "--target", "ES2022", "consumer.ts"]);
    check(fs.readFileSync(path.join(consumer, "package-lock.json")).equals(lockBytes), "lock changed during consumer checks");
    report.runtime = read(path.join(consumer, "runtime.json")); report.typesVerified = true; report.verified = true;
  } catch (error) { report.error = error.message; throw error; }
  finally { write(path.join(output, "registry-readback-receipt.json"), report); }
}

async function main() {
  const mode = process.argv[2]; const root = path.dirname(fileURLToPath(import.meta.url));
  const bundle = process.env.READBACK_BUNDLE;
  check(bundle, "READBACK_BUNDLE required");
  if (mode === "authenticate") {
    check(process.env.GITHUB_REF === "refs/heads/main", "readback must run trusted main");
    const runId = process.env.PUBLICATION_RUN; const repository = process.env.GITHUB_REPOSITORY;
    check(runId === APPROVED.runId && repository === APPROVED.repository, "unreviewed publication requested");
    const base = `https://api.github.com/repos/${APPROVED.repository}/actions/runs/${APPROVED.runId}`;
    const run = await get(base); const jobs = await get(`${base}/attempts/${APPROVED.attempt}/jobs?per_page=100`);
    authenticatePublication(run, jobs, runId, repository);
    fs.mkdirSync(bundle, { recursive: true }); write(path.join(bundle, "publication-run.json"), run); write(path.join(bundle, "publication-jobs.json"), jobs);
  } else if (mode === "prepare") {
    bundleEvidence(bundle);
    for (const name of ["registry-readback.mjs", "consume.mjs"]) fs.copyFileSync(path.join(root, name), path.join(bundle, name));
  } else if (mode === "consume") await consume(bundle, process.env.READBACK_OUTPUT);
  else throw new Error("expected authenticate, prepare or consume");
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
