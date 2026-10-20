// Reviewed-inventory npm packed consumer (R2a). One runner for every npm unit with a reviewed
// `npm.<name>` entry in ci/consumer-recipes.json; per-recipe parameters (recipe id, export surface,
// dependency closure, scenario count) come only from that entry. @rightkit/logs keeps its named
// PK11 runner (npm-consumer.mjs) untouched. No registry credentials, builds, uploads or adapters.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { childEnv, loadSealed, sha, tarEntry, integrity, loadApprovedArtifacts } from './upload.mjs';
import { isWithin } from './registry-consumer.mjs';
import { validateContext, validateManifestPins, pnpmInvocation, spawnPnpm } from './npm-consumer.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
export const RUNNER_FILE = 'scripts/ci/npm-recipe-consumer.mjs';
const NAME = /^@rightkit\/[a-z0-9][a-z0-9-]*$/;
const PACKAGE = /^(?:@[a-z0-9-]+\/)?[a-z0-9][a-z0-9._-]*$/;
const EXACT = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
const ENTRY_KEYS = ['closure', 'files', 'id', 'pkId', 'recipeId', 'scenarios', 'surface'];
const canon = v => Array.isArray(v) ? v.map(canon) : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map(k => [k, canon(v[k])])) : v;
const same = (a, b) => JSON.stringify(canon(a)) === JSON.stringify(canon(b));
const sorted = list => [...list].sort();
const need = (ok, why) => { if (!ok) throw new Error(why); };

// Hosts that must each hold PK evidence before a recipe may qualify, and the hosts this pipeline can run.
// A required host without a runner is a refusal, never a skip (PK14-2 requires Linux). Linux is its own host and is
// admitted ONLY for a unit whose reviewed host list names it; every other unit is macOS & Windows, as before.
export const REQUIRED_HOSTS = Object.freeze({ '@rightkit/qa-chrome': ['linux', 'macos', 'windows'] });
export const RUNNABLE_HOSTS = Object.freeze(['linux', 'macos', 'windows']);
export const DEFAULT_HOSTS = Object.freeze(['macos', 'windows']);
export const HOST_RUNNER_OS = Object.freeze({ linux: 'Linux', macos: 'macOS', windows: 'Windows' });
export const missingRequiredHosts = name => (REQUIRED_HOSTS[name] ?? []).filter(host => !RUNNABLE_HOSTS.includes(host));
/** The hosts this unit's recipe may be qualified on. */
export const allowedHosts = name => REQUIRED_HOSTS[name] ?? DEFAULT_HOSTS;
/** Host name of a Node platform: exactly win32, darwin or linux; anything else is unknown, never guessed as macOS. */
export const hostForPlatform = platform => ({ win32: 'windows', darwin: 'macos', linux: 'linux' })[platform];
// Reviewed runtime environment: besides the fixed RIGHTKIT_CONSUMER_* binding, only these variables reach the consumer.
export const RUNTIME_ENV = Object.freeze({ '@rightkit/qa-chrome': ['RIGHTKIT_CHROME_FOR_TESTING_ROOT'] });
export const runtimeEnvNames = name => RUNTIME_ENV[name] ?? [];
export function runtimeEnvironment(name, env, { checkout, scratch, platform = process.platform } = {}) {
  const out = {};
  for (const key of runtimeEnvNames(name)) {
    const value = env?.[key];
    need(typeof value === 'string' && path.isAbsolute(value), `${name} requires explicit absolute ${key}`);
    need(fs.existsSync(value) && fs.statSync(value).isDirectory(), `${key} must name an existing directory`);
    const real = fs.realpathSync(value); const fold = text => (platform === 'win32' ? text.toLowerCase() : text);
    need(fold(real) === fold(path.resolve(value)), `${key} must be canonical`);
    const inside = base => fold(real) === fold(fs.realpathSync(base)) || isWithin(base, real);
    need(!inside(checkout) && !inside(scratch), `${key} must be outside checkout and scratch`);
    out[key] = value;
  }
  return out;
}
/** Isolated child environment. pnpm 11 ignores auto-install-peers in a userconfig, so the pnpm_config_ variable is authoritative. */
export function consumerChildEnv(env, home) {
  const child = Object.fromEntries(Object.entries(childEnv(env)).filter(([key]) => !/^(GITHUB_|ACTIONS_|RUNNER_)/.test(key)));
  return Object.assign(child, { HOME: home, USERPROFILE: home, npm_config_userconfig: path.join(home, '.npmrc'), npm_config_globalconfig: path.join(home, 'global.npmrc'), npm_config_ignore_scripts: 'true', pnpm_config_auto_install_peers: 'false', CI: 'true', COREPACK_ENABLE_PROJECT_SPEC: '1' });
}

export const shortName = name => { need(NAME.test(name ?? ''), 'unsafe npm recipe name'); return name.slice('@rightkit/'.length); };
export const recipeDirectory = name => `ci/consumers/npm/${shortName(name)}`;
export const sourceFiles = name => ['ci/consumer-recipes.json', 'package.json', `packages/${shortName(name)}/package.json`, ...(['@rightkit/tauri', '@rightkit/qa'].includes(name) ? ['ci/approved-artifacts.json'] : [])];
export const hasReviewedNpmEntry = (inventory, name) => inventory?.schema === 1 && Object.hasOwn(inventory.npm ?? {}, name) && name !== '@rightkit/logs';

/** Missing, malformed or foreign entries are refused; nothing falls back to a default recipe. */
export function validateRecipeEntry(entry, name) {
  const dir = recipeDirectory(name);
  need(entry && typeof entry === 'object' && same(Object.keys(entry).sort(), entry.importClosures === undefined ? ENTRY_KEYS : [...ENTRY_KEYS, 'importClosures'].sort()), `missing reviewed PK example for ${name}: reviewed npm.${name} entry malformed`);
  need(/^PK\d{2}$/.test(entry.pkId) && entry.id === `${dir}/consumer.mjs` && /^[a-z0-9][a-z0-9-]*$/.test(entry.recipeId ?? '') && Number.isSafeInteger(entry.scenarios) && entry.scenarios > 0, `reviewed npm.${name} identity differs from named recipe layout`);
  need(Array.isArray(entry.surface) && entry.surface.length > 0 && entry.surface.every(s => typeof s === 'string' && (s === '.' || s.startsWith('./'))) && same(entry.surface, sorted(new Set(entry.surface))), `reviewed npm.${name} surface must be a sorted unique export list`);
  const { closure } = entry;
  need(closure && same(Object.keys(closure).sort(), ['declaredOnly', 'install']) && closure.install && typeof closure.install === 'object' && Array.isArray(closure.declaredOnly), `reviewed npm.${name} closure malformed`);
  for (const [dep, version] of Object.entries(closure.install)) need(dep !== name && typeof dep === 'string' && !/[:\\]/.test(dep) && typeof version === 'string' && EXACT.test(version), `closure install pin must be exact registry version: ${dep}`);
  need(closure.declaredOnly.every(dep => typeof dep === 'string' && PACKAGE.test(dep) && dep !== name && !Object.hasOwn(closure.install, dep)) && same(closure.declaredOnly, sorted(new Set(closure.declaredOnly))), `closure declaredOnly must be sorted unique, uninstalled package names`);
  if (entry.importClosures !== undefined) validateImportClosures(entry, name);
  const files = Object.keys(entry.files ?? {});
  need(files.length > 0 && same(files, sorted(files)) && Object.values(entry.files).every(h => /^[a-f0-9]{64}$/.test(h)), `reviewed npm.${name} file inventory malformed`);
  const expected = [`${dir}/consumer.mjs`, `${dir}/package.json`, RUNNER_FILE];
  need(expected.every(f => files.includes(f)) && files.every(f => f === RUNNER_FILE || (f.startsWith(`${dir}/`) && !f.slice(dir.length + 1).includes('..'))), `reviewed npm.${name} files must be the recipe directory plus ${RUNNER_FILE}`);
  return entry;
}

/** Explicit per-export install maps: exact reviewed pins, no implicit peer union. */
export function validateImportClosures(entry, name, runtimeSubpaths = entry.surface) {
  const maps = entry.importClosures;
  need(maps && typeof maps === 'object' && !Array.isArray(maps), `reviewed npm.${name} importClosures must be an object`);
  const keys = Object.keys(maps);
  need(same(keys, sorted(new Set(keys))) && same(keys, entry.surface), `reviewed npm.${name} importClosures must cover reviewed surface exactly once in sorted order`);
  need(runtimeSubpaths.every(subpath => Object.hasOwn(maps, subpath)), `reviewed npm.${name} importClosures missing runtime export`);
  for (const [subpath, install] of Object.entries(maps)) {
    need(install && typeof install === 'object' && !Array.isArray(install), `importClosures ${subpath} install map malformed`);
    need(same(Object.keys(install), sorted(Object.keys(install))), `importClosures ${subpath} install pins must be sorted`);
    for (const [dep, version] of Object.entries(install)) need(PACKAGE.test(dep) && dep !== name && EXACT.test(version ?? '') && entry.closure.install[dep] === version, `importClosures ${subpath} pin outside reviewed closure: ${dep}`);
  }
  return maps;
}

/** Packed unit may only depend on the declared closure; export surface is exactly the reviewed list. */
export function validatePackedClosure(packed, entry, name, version) {
  need(packed?.name === name && packed.version === version, 'packed package identity differs');
  need(packed.exports && typeof packed.exports === 'object' && same(Object.keys(packed.exports).sort(), entry.surface), `packed export surface differs from reviewed npm.${name} surface`);
  const allowed = new Set([...Object.keys(entry.closure.install), ...entry.closure.declaredOnly]);
  const seen = [];
  for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) for (const dep of Object.keys(packed[field] ?? {})) {
    seen.push(dep);
    need(allowed.has(dep), `dependency outside reviewed closure: ${dep}`);
  }
  for (const dep of entry.closure.declaredOnly) need(seen.includes(dep), `declared-only dependency not present in packed manifest: ${dep}`);
  return seen.length;
}

/** The committed consumer manifest is exactly the reviewed install closure plus the supplied archive. */
export function validateConsumerManifest(manifest, entry, pin) {
  need(manifest && (manifest.packageManager === undefined || manifest.packageManager === pin) && manifest.private === true && manifest.type === 'module', 'consumer manifest pin/privacy/type mismatch');
  need(Object.keys(manifest).every(k => ['name', 'version', 'private', 'type', 'packageManager', 'scripts', 'dependencies'].includes(k)), 'consumer manifest has unreviewed fields');
  need(manifest.scripts === undefined || Object.entries(manifest.scripts).every(([key, value]) => ['qualify', 'consumer'].includes(key) && value === 'node consumer.mjs'), 'consumer manifest scripts are not reviewed');
  need(same(manifest.dependencies ?? {}, entry.closure.install) && same(Object.keys(manifest.dependencies ?? {}).sort(), Object.keys(entry.closure.install).sort()), 'consumer dependencies differ from reviewed closure');
  return true;
}
// The root pin is bound into the scratch manifest, so a reviewed recipe need not repeat it.
export const withPin = (manifest, pin) => ({ ...manifest, packageManager: manifest.packageManager ?? pin });
export function scratchManifest(manifest, name, archive, pin) {
  const { scripts, ...rest } = withPin(manifest, pin);
  return { ...rest, dependencies: { ...manifest.dependencies, [name]: `file:${archive.replaceAll('\\', '/')}` } };
}

// These are exact reviewed dependency-byte exceptions, never a general release-age bypass.
export const CLOSURE_APPROVAL = Object.freeze({ name: '@rightkit/logs', version: '0.1.6', sha256: '72bb07be21cd4138458a635d9c48b7619f3b2874c0758089972e40015de0286c' });
export const QA_CLOSURE_APPROVAL = Object.freeze({ name: '@rightkit/qa-chrome', version: '0.1.10', sha256: '60e7bee4a1b92d409bf67decac5e60de208eede8c16f846e8e3272f11f964797' });
const closureApproval = name => name === '@rightkit/qa' ? QA_CLOSURE_APPROVAL : CLOSURE_APPROVAL;
export function closureAgeExceptions(entry, name) {
  if (name === '@rightkit/qa') {
    need(same(entry.closure.install, { '@rightkit/qa-chrome': '0.1.10' }), 'qa release-age exception requires exact selected reviewed closure');
    return [`${QA_CLOSURE_APPROVAL.name}@${QA_CLOSURE_APPROVAL.version}`];
  }
  if (name !== '@rightkit/tauri') return [];
  need(same(entry.closure.install, { '@rightkit/app-update': '0.4.0', '@rightkit/logs': '0.1.6', '@tauri-apps/plugin-dialog': '2.8.3' }), 'tauri release-age exception requires exact selected reviewed closure');
  return [`${CLOSURE_APPROVAL.name}@${CLOSURE_APPROVAL.version}`];
}
export function validateClosureApproval(artifacts, entry, name) {
  if (!closureAgeExceptions(entry, name).length) return;
  need(Array.isArray(artifacts), 'dependency byte approval list missing');
  const approved = closureApproval(name);
  const rows = artifacts.filter(row => row?.registry === 'npm' && row.name === approved.name && row.version === approved.version);
  need(rows.length === 1 && rows[0].sha256 === approved.sha256 && typeof rows[0].reviewer === 'string' && rows[0].reviewer.trim() && typeof rows[0].reviewRef === 'string' && rows[0].reviewRef.trim(), 'exact selected dependency bytes lack reviewer approval');
}
// Fixed child source is command-bound by admission. Existing reviewed helper checks every installed archive file.
export const CLOSURE_PROVENANCE_SCRIPT = `
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { validateInstalledArtifact } from './provenance.mjs';
const expected = ${JSON.stringify(CLOSURE_APPROVAL)};
const response = await fetch('https://registry.npmjs.org/@rightkit/logs/-/logs-0.1.6.tgz', { redirect: 'error', signal: AbortSignal.timeout(60000) });
if (!response.ok) throw new Error('approved dependency registry fetch failed');
const bytes = Buffer.from(await response.arrayBuffer());
if (!bytes.length || bytes.length > 32 * 1024 * 1024 || createHash('sha256').update(bytes).digest('hex') !== expected.sha256) throw new Error('registry dependency differs from approved bytes');
const archive = path.resolve('closure-logs.tgz');
fs.writeFileSync(archive, bytes);
const resolved = import.meta.resolve(expected.name);
const evidence = validateInstalledArtifact({ fixtureUrl: pathToFileURL(path.resolve('consumer.mjs')).href, archive, sha256: expected.sha256, version: expected.version, resolution: 'fetched', packageName: expected.name, rootResolvedUrl: resolved, entries: { [expected.name]: resolved } });
console.log(JSON.stringify({ archiveBase64: bytes.toString('base64'), artifact: evidence.artifact }));
`;
// Keep the Tauri command byte-for-byte stable for existing authenticated receipts.
export const QA_CLOSURE_PROVENANCE_SCRIPT = CLOSURE_PROVENANCE_SCRIPT
  .replace(JSON.stringify(CLOSURE_APPROVAL), JSON.stringify(QA_CLOSURE_APPROVAL))
  .replace('https://registry.npmjs.org/@rightkit/logs/-/logs-0.1.6.tgz', 'https://registry.npmjs.org/@rightkit/qa-chrome/-/qa-chrome-0.1.10.tgz')
  .replace('closure-logs.tgz', 'closure-qa-chrome.tgz');
export const closureProvenanceScript = name => name === '@rightkit/qa' ? QA_CLOSURE_PROVENANCE_SCRIPT : CLOSURE_PROVENANCE_SCRIPT;
export function validateClosureProvenance(evidence, lock, entry, name, scratch, api = path) {
  if (!closureAgeExceptions(entry, name).length) { need(evidence === undefined, 'unexpected dependency provenance exception'); return; }
  need(evidence && same(Object.keys(evidence).sort(), ['archiveBase64', 'artifact']) && typeof evidence.archiveBase64 === 'string' && evidence.archiveBase64.length <= 45 * 1024 * 1024, 'dependency provenance evidence malformed');
  const approved = closureApproval(name);
  const bytes = Buffer.from(evidence.archiveBase64, 'base64');
  need(bytes.length > 0 && bytes.toString('base64') === evidence.archiveBase64 && sha('sha256', bytes) === approved.sha256, 'registry dependency differs from approved bytes');
  const lockIdentity = `${approved.name}@${approved.version}`.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const rows = [...lock.matchAll(new RegExp(`^  '${lockIdentity}':\\r?\\n    resolution: \\{integrity: (sha512-[A-Za-z0-9+/]{86}==)\\}\\r?$`, 'gm'))];
  need(rows.length === 1 && rows[0][1] === integrity(bytes), 'dependency lock integrity differs from approved registry tarball');
  const artifact = evidence.artifact;
  need(artifact?.path === api.join(scratch, name === '@rightkit/qa' ? 'closure-qa-chrome.tgz' : 'closure-logs.tgz') && artifact.sha256 === approved.sha256 && artifact.version === approved.version && artifact.resolution === 'fetched' && artifact.installationBinding === 'all-archive-files-sha256' && Number.isSafeInteger(artifact.archiveFileCount) && artifact.archiveFileCount > 0, 'installed dependency provenance differs from approved registry tarball');
}

/** An owned, isolated workspace applies only exact reviewed RightKit closure pins, with only its explicit reviewed dependency-byte exception. */
export function scratchWorkspace(entry, name) {
  validateRecipeEntry(entry, name);
  const pins = Object.entries(entry.closure.install).filter(([dep]) => NAME.test(dep)).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
  const exceptions = closureAgeExceptions(entry, name);
  return `packages: []\n${exceptions.length ? `minimumReleaseAgeExclude:\n${exceptions.map(spec => `  - ${JSON.stringify(spec)}\n`).join('')}` : ''}${pins.length ? `overrides:\n${pins.map(([dep, version]) => `  ${JSON.stringify(dep)}: ${JSON.stringify(version)}\n`).join('')}` : 'overrides: {}\n'}`;
}

/** No config or policy mutation is admitted, including pnpm-generated minimumReleaseAgeExclude entries. */
export function validateWorkspaceBinding(workspace, entry, name, scratch, api = path) {
  const config = scratchWorkspace(entry, name); const digest = sha('sha256', config);
  need(same(workspace, { file: api.join(scratch, 'pnpm-workspace.yaml'), before: config, beforeSha256: digest, after: config, afterSha256: digest }), 'isolated workspace differs from exact reviewed overrides or pnpm changed policy/config during install');
  return true;
}

// --ignore-workspace would disable the owned config; this command is valid only with its bound isolated workspace.
export const consumerInstallArgs = store => ['install', '--ignore-scripts', '--no-frozen-lockfile', '--store-dir', store];

/** Every @rightkit resolution in the fresh lock is the supplied archive or an exact reviewed pin. */
export function validateLockClosure(lock, entry, name, archiveName) {
  need(typeof lock === 'string' && !/workspace:|link:|git\+|github:/.test(lock), 'lock contains foreign dependency source');
  const resolved = new Map();
  for (const line of lock.split(/\r?\n/)) {
    const match = line.match(/^ {2}'?(@rightkit\/[a-z0-9-]+)@(.+?)'?:\s*$/);
    if (match) resolved.set(match[1], new Set([...(resolved.get(match[1]) ?? []), match[2].replace(/\(.*$/, '')]));
  }
  need(resolved.has(name) && [...resolved.get(name)].every(v => v.startsWith('file:') && v.includes(archiveName)), 'lock does not resolve the unit from the supplied archive');
  for (const [dep, versions] of resolved) {
    if (dep === name) continue;
    need(Object.hasOwn(entry.closure.install, dep) && [...versions].every(v => v === entry.closure.install[dep]), `lock resolves @rightkit dependency outside reviewed closure: ${dep}`);
  }
  return resolved.size;
}

/** The consumer must echo the reviewed recipe and bind the exact archive & installed bytes. */
export function validateMountResult(result, entry, { sha256, version, archive }) {
  need(result && result.schemaVersion === 1 && result.recipeId === entry.recipeId && result.ok === true && result.nativeHostQualified === false, 'consumer result identity/ok/native flag mismatch');
  need(Array.isArray(result.errors) && result.errors.length === 0 && Array.isArray(result.scenarios) && result.scenarios.length === entry.scenarios && result.scenarios.every(s => s?.ok === true && typeof s.name === 'string' && !s.error), 'consumer scenarios not exactly the reviewed passing set');
  const art = result.artifact;
  need(art && art.sha256 === sha256 && art.version === version && art.resolution === 'packed' && art.installationBinding === 'all-archive-files-sha256' && Number.isSafeInteger(art.archiveFileCount) && art.archiveFileCount > 0 && (archive === undefined || art.path === archive), 'consumer artifact binding mismatch');
  return { scenarios: result.scenarios.length, archiveFileCount: art.archiveFileCount };
}

/** Admission: authenticated expected identity, verifier-revision recipe entry & Git blob bindings. */
export function npmRecipeQualificationProblems(input = {}) {
  try { return qualification(input); }
  catch (error) { return [`npm recipe: malformed qualification evidence (${error.message})`]; }
}
function qualification({ report, expected, recipe, sourceBindings } = {}) {
  if (!report || !expected || !recipe || !sourceBindings) return ['npm recipe: missing report/trusted identity/recipe/source bindings'];
  const problems = []; const fail = message => problems.push(`npm recipe: ${message}`);
  const name = expected.name; const entry = validateRecipeEntry(recipe, name);
  const missingHosts = missingRequiredHosts(name);
  if (missingHosts.length) fail(`required host evidence missing for ${missingHosts.join(', ')}; no runner exists, refusing instead of skipping`);
  const identity = ['name', 'version', 'commit', 'runId', 'runAttempt', 'manifestRunId', 'manifestRunAttempt', 'manifestSha256', 'archiveSha256', 'archiveSize', 'archiveIntegrity', 'host', 'runnerOS', 'arch'];
  for (const field of identity) if (expected[field] === undefined || report[field] !== expected[field]) fail(`identity mismatch/missing ${field}`);
  if (report.schema !== 1 || report.kind !== 'npm-consumer' || report.qualification !== 'R2a' || report.recipeId !== entry.recipeId || report.pkId !== entry.pkId || report.verified !== true || report.error !== undefined || report.name !== name) fail('unverified or unknown qualification');
  if (!/^[a-f0-9]{40}$/.test(report.commit ?? '') || !allowedHosts(name).includes(report.host) || report.runnerOS !== HOST_RUNNER_OS[report.host] || !Number.isSafeInteger(report.archiveSize) || report.archiveSize < 1 || !/^sha512-[A-Za-z0-9+/]{86}==$/.test(report.archiveIntegrity ?? '')) fail('malformed candidate/native identity');
  const pin = expected.packageManager;
  if (!/^pnpm@\d+\.\d+\.\d+$/.test(pin ?? '') || report.toolchain?.packageManager !== pin || report.toolchain?.pnpm !== pin?.slice(5) || !/^v\d+\.\d+\.\d+$/.test(report.toolchain?.node ?? '')) fail('native toolchain/pnpm pin mismatch');
  const recipeFiles = Object.keys(entry.files); const bound = [...recipeFiles, ...sourceFiles(name)];
  if (!same(Object.keys(report.sourceBindings ?? {}).sort(), sorted(bound))) fail('unknown/missing source binding');
  const expectedRecipe = {};
  for (const file of bound) {
    const trusted = sourceBindings[file]; const actual = report.sourceBindings?.[file];
    if (!/^[a-f0-9]{64}$/.test(trusted?.sha256 ?? '') || !/^[a-f0-9]{40}$/.test(trusted?.gitBlob ?? '') || actual?.sha256 !== trusted.sha256 || actual?.gitBlob !== trusted.gitBlob) fail(`source blob mismatch ${file}`);
    if (recipeFiles.includes(file)) { if (entry.files[file] !== trusted?.sha256) fail(`reviewed recipe differs from candidate blob ${file}`); expectedRecipe[file] = { sha256: trusted?.sha256, gitBlob: trusted?.gitBlob }; }
  }
  if (report.recipe?.sourceCommit !== report.commit || !same(report.recipe?.files, expectedRecipe) || report.recipe?.sha256 !== sha('sha256', JSON.stringify(expectedRecipe))) fail('recipe digest/source mismatch');
  const commands = report.commands;
  if (!Array.isArray(commands)) return [...problems, 'npm recipe: missing actual command transcript'];
  const counts = { source: bound.length, 'archive-read': 2, toolchain: 1, install: 1, runtime: 1, 'closure-provenance': closureAgeExceptions(entry, name).length ? 1 : 0 };
  for (const command of commands) if (!Object.hasOwn(counts, command.stage) || command.status !== 0 || command.error || typeof command.stdout !== 'string' || typeof command.stderr !== 'string' || !Array.isArray(command.args) || command.args.some(a => typeof a !== 'string')) fail('unknown/failed/malformed actual command');
  for (const [stage, count] of Object.entries(counts)) if (commands.filter(c => c.stage === stage).length !== count) fail(`missing/duplicate ${stage} stage`);
  const sources = commands.filter(c => c.stage === 'source');
  const sourceOut = file => sources.find(c => c.command === 'git' && same(c.args, ['show', `${report.commit}:${file}`]))?.stdout;
  for (const file of bound) {
    const rows = sources.filter(c => c.command === 'git' && same(c.args, ['show', `${report.commit}:${file}`]));
    if (rows.length !== 1 || sha('sha256', rows[0].stdout) !== sourceBindings[file]?.sha256 || sha('sha1', Buffer.concat([Buffer.from(`blob ${Buffer.byteLength(rows[0].stdout)}\0`), Buffer.from(rows[0].stdout)])) !== sourceBindings[file]?.gitBlob) fail(`source command output differs from authenticated blob ${file}`);
  }
  const api = report.host === 'windows' ? path.win32 : path.posix; const iso = report.isolation;
  const inside = (parent, child) => { if (typeof parent !== 'string' || typeof child !== 'string' || !api.isAbsolute(parent) || !api.isAbsolute(child)) return false; const rel = api.relative(parent, child); return !rel || (!rel.startsWith(`..${api.sep}`) && rel !== '..' && !api.isAbsolute(rel)); };
  if (!iso || inside(iso.checkout, iso.scratch) || !api.isAbsolute(iso.originalArchive ?? '') || iso.ignoreScripts !== true || iso.inheritWorkspace !== false || iso.scriptLifecycle !== 'disabled' || !inside(iso.scratch, iso.home) || !inside(iso.scratch, iso.store)) { fail('scratch/lifecycle isolation mismatch'); return problems; }
  if (expected.archivePath !== undefined && iso.originalArchive !== expected.archivePath) fail('original archive path differs from candidate');
  for (const command of sources) if (command.cwd !== iso.checkout) fail('source command checkout mismatch');
  try {
    // Re-derive every recipe decision from the recorded actual output & the trusted entry.
    const archiveRows = commands.filter(c => c.stage === 'archive-read');
    const entryRow = file => archiveRows.find(c => c.command === 'tar' && same(c.args, ['-xOzf', api.basename(iso.originalArchive), file]) && c.cwd === api.dirname(iso.originalArchive));
    const packed = JSON.parse(entryRow('package/package.json').stdout);
    if (!entryRow('package/README.md')) throw new Error('README archive read missing');
    validatePackedClosure(packed, entry, name, expected.version);
    const consumer = JSON.parse(sourceOut(`${recipeDirectory(name)}/package.json`));
    if (validateManifestPins(JSON.parse(sourceOut('package.json')), JSON.parse(sourceOut(`packages/${shortName(name)}/package.json`)), withPin(consumer, pin), packed, name, expected.version) !== pin) throw new Error('pnpm pin mismatch');
    validateConsumerManifest(consumer, entry, pin);
    const inventory = JSON.parse(sourceOut('ci/consumer-recipes.json'));
    if (!same(inventory.npm?.[name], entry)) throw new Error('candidate inventory entry differs from verifier-revision entry');
    if (!same(iso.manifest, scratchManifest(consumer, name, iso.originalArchive, pin)) || sha('sha256', `${JSON.stringify(iso.manifest, null, 2)}\n`) !== iso.consumerManifestSha256) throw new Error('scratch manifest differs from reviewed closure plus archive');
    validateWorkspaceBinding(report.workspace, entry, name, iso.scratch, api);
    if (closureAgeExceptions(entry, name).length) validateClosureApproval(JSON.parse(sourceOut('ci/approved-artifacts.json')).artifacts, entry, name);
    if (report.lockSha256 !== sha('sha256', report.lock ?? '') || !report.lock.includes(report.archiveIntegrity)) throw new Error('lock digest/integrity mismatch');
    validateLockClosure(report.lock, entry, name, api.basename(iso.originalArchive));
    const provenance = commands.find(c => c.stage === 'closure-provenance');
    if (closureAgeExceptions(entry, name).length) {
      if (!provenance || !api.isAbsolute(provenance.command ?? '') || provenance.cwd !== iso.scratch || !same(provenance.args, ['--input-type=module', '-e', closureProvenanceScript(name)])) throw new Error('dependency provenance command differs');
      const evidence = JSON.parse(provenance.stdout.trim());
      if (!same(report.closureProvenance, evidence)) throw new Error('dependency provenance output/report mismatch');
      validateClosureProvenance(evidence, report.lock, entry, name, iso.scratch, api);
    } else validateClosureProvenance(report.closureProvenance, report.lock, entry, name, iso.scratch, api);
  } catch (error) { fail(`actual packed/consumer/lock output invalid: ${error.message}`); }
  const runtimeKeys = runtimeEnvNames(name);
  if (!same(Object.keys(report.runtimeEnv ?? {}).sort(), [...runtimeKeys].sort()) || Object.values(report.runtimeEnv ?? {}).some(v => typeof v !== 'string' || !api.isAbsolute(v))) fail('runtime environment differs from reviewed list');
  const install = commands.find(c => c.stage === 'install'); const version = commands.find(c => c.stage === 'toolchain'); const runtime = commands.find(c => c.stage === 'runtime');
  const expectedCall = args => report.host === 'windows' ? { command: api.join(iso.systemRoot, 'System32', 'cmd.exe'), args: ['/d', '/s', '/c', `pnpm ${args.map(a => `"${a}"`).join(' ')}`] } : { command: 'pnpm', args };
  for (const [row, args] of [[install, consumerInstallArgs(iso.store)], [version, ['--version']]]) {
    const call = expectedCall(args);
    if (!row || row.command !== call.command || !same(row.args, call.args) || row.cwd !== iso.scratch) fail('actual pnpm command mismatch');
  }
  if (version?.stdout.trim() !== report.toolchain?.pnpm) fail('pnpm version output mismatch');
  if (!runtime || !api.isAbsolute(runtime.command ?? '') || runtime.cwd !== iso.scratch || !same(runtime.args, ['consumer.mjs'])) fail('runtime command mismatch');
  try {
    const result = JSON.parse(runtime.stdout.trim());
    const shown = validateMountResult(result, entry, { sha256: expected.archiveSha256, version: expected.version, archive: iso.originalArchive });
    if (!same(report.counts, shown) || !same(report.mount, result)) fail('runtime output/report mismatch');
  } catch (error) { fail(`actual runtime output invalid: ${error.message}`); }
  return problems;
}

export function runNpmRecipeConsumer({ packDir, name, reportFile, root = ROOT, scratchRoot = process.env.RUNNER_TEMP ?? os.tmpdir(), env = process.env, spawn = spawnSync } = {}) {
  validateContext(env, process.platform, { linux: allowedHosts(name).includes('linux') });
  need(missingRequiredHosts(name).length === 0, `${name} requires evidence from ${REQUIRED_HOSTS[name]?.join(', ')} but no runner exists for ${missingRequiredHosts(name).join(', ')}: refusing instead of skipping`);
  const host = hostForPlatform(process.platform);
  need(host !== undefined && allowedHosts(name).includes(host), `${name} is not qualified on ${host ?? process.platform}; its reviewed hosts are ${allowedHosts(name).join(', ')}: refusing`);
  need(env.RUNNER_OS === HOST_RUNNER_OS[host], `RUNNER_OS ${JSON.stringify(env.RUNNER_OS)} does not match the ${host} host`);
  need(packDir && reportFile, 'sealed pack & report required');
  const short = shortName(name);
  root = fs.realpathSync(root); scratchRoot = fs.realpathSync(scratchRoot);
  const reportParent = fs.realpathSync(path.dirname(path.resolve(reportFile)));
  if (isWithin(root, scratchRoot) || isWithin(root, reportParent)) throw new Error('scratch & report must be outside checkout');
  const raw = JSON.parse(fs.readFileSync(path.join(packDir, 'manifest.json')));
  const sealed = loadSealed({ packDir, registry: 'npm', unitsText: (raw.units ?? []).map(unit => unit.name).join(' '), run: { runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT } });
  if (sealed.problems.length) throw new Error(sealed.problems.join('\n'));
  if (sealed.manifest.commit !== env.GITHUB_SHA) throw new Error('candidate source differs from CI checkout');
  const matches = sealed.manifest.units.filter(unit => unit.name === name);
  if (matches.length !== 1 || new Set(sealed.manifest.units.map(unit => unit.name)).size !== sealed.manifest.units.length) throw new Error('missing/duplicate sealed identity');
  const unit = matches[0];
  if (!Number.isSafeInteger(unit.size) || unit.size <= 0 || !/^[a-f0-9]{64}$/.test(unit.sha256 ?? '') || unit.dir !== `packages/${short}`) throw new Error('invalid sealed unit identity');
  const archive = path.resolve(packDir, unit.file);
  const commands = [];
  const scratch = fs.mkdtempSync(path.join(scratchRoot, `rk-npm-${short}-`));
  const home = path.join(scratch, 'home'); fs.mkdirSync(home);
  const child = consumerChildEnv(env, home);
  // Peers are never auto-installed: every @rightkit dependency must be an explicit reviewed pin.
  fs.writeFileSync(child.npm_config_userconfig, 'registry=https://registry.npmjs.org/\nignore-scripts=true\nauto-install-peers=false\n'); fs.writeFileSync(child.npm_config_globalconfig, '');
  const run = (command, args, cwd = scratch, stage = 'source', spawnOptions = {}, invoke = spawn) => {
    const result = invoke(command, args, { cwd, env: child, encoding: 'utf8', timeout: 300_000, maxBuffer: 8 * 1024 * 1024, ...spawnOptions });
    commands.push({ stage, command, args, cwd, status: result.status, stdout: String(result.stdout ?? ''), stderr: String(result.stderr ?? ''), ...(result.error ? { error: result.error.message } : {}) });
    if (result.error || result.status !== 0) {
      // The failing command's own output is the only diagnosis; keep its tail in the host log (never in the sealed command rows).
      const tail = text => String(text ?? '').trim().slice(-4000);
      const detail = [['args', args.join(' ')], ['error', result.error?.message], ['stdout tail', tail(result.stdout)], ['stderr tail', tail(result.stderr)]].filter(([, value]) => value).map(([label, value]) => `
--- ${label} ---
${value}`).join('');
      throw new Error(`consumer command failed: ${command} (${result.status})${detail}`);
    }
    return String(result.stdout);
  };
  const pnpm = args => {
    const call = pnpmInvocation(args, process.platform, env);
    return run(call.command, call.args, scratch, args[0] === 'install' ? 'install' : 'toolchain', {}, (command, invocationArgs, options) => {
      const invoked = spawnPnpm(args, { spawn, platform: process.platform, env, options });
      if (invoked.call.command !== command || JSON.stringify(invoked.call.args) !== JSON.stringify(invocationArgs)) throw new Error('pnpm invocation changed during spawn');
      return invoked.result;
    });
  };
  const report = { schema: 1, kind: 'npm-consumer', qualification: 'R2a', recipeId: null, pkId: null, name, version: unit.version, commit: sealed.manifest.commit, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, manifestRunId: String(sealed.manifest.runId), manifestRunAttempt: String(sealed.manifest.runAttempt), manifestSha256: sealed.manifestSha256, archiveSha256: unit.sha256, archiveSize: unit.size, host, runnerOS: env.RUNNER_OS, arch: process.arch, commands, verified: false };
  try {
    // The reviewed entry is read first and names every file that may execute; nothing else is trusted.
    const inventoryPath = path.join(root, 'ci/consumer-recipes.json');
    const inventory = JSON.parse(fs.readFileSync(inventoryPath));
    if (!hasReviewedNpmEntry(inventory, name)) throw new Error(`missing reviewed PK example for ${name}; review ci/consumer-recipes.json npm.${name}`);
    const entry = validateRecipeEntry(inventory.npm[name], name);
    report.recipeId = entry.recipeId; report.pkId = entry.pkId;
    const dir = recipeDirectory(name);
    const onDisk = [];
    const visit = relative => {
      for (const ent of fs.readdirSync(path.join(root, relative), { withFileTypes: true })) {
        need(!ent.isSymbolicLink() && !ent.name.startsWith('.') && !['node_modules', 'pnpm-lock.yaml', 'package-lock.json', 'yarn.lock'].includes(ent.name), 'recipe contains symlink, hidden config, generated output or lock');
        const rel = `${relative}/${ent.name}`;
        if (ent.isDirectory()) visit(rel); else { need(ent.isFile(), 'recipe special file'); onDisk.push(rel); }
      }
    };
    need(fs.existsSync(path.join(root, dir)), `missing reviewed PK example directory ${dir}`);
    visit(dir);
    if (!same(sorted(onDisk), sorted(Object.keys(entry.files).filter(f => f !== RUNNER_FILE)))) throw new Error('recipe directory differs from reviewed file inventory');
    const bindings = {};
    for (const file of [...Object.keys(entry.files), ...sourceFiles(name)]) {
      const source = run('git', ['show', `${env.GITHUB_SHA}:${file}`], root);
      const bytes = fs.readFileSync(path.join(root, file));
      if (!bytes.equals(Buffer.from(source))) throw new Error(`recipe/source raw bytes differ from committed source: ${file}`);
      bindings[file] = { sha256: sha('sha256', bytes), executionSha256: sha('sha256', bytes), gitBlob: sha('sha1', Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
    }
    report.sourceBindings = Object.fromEntries(Object.entries(bindings).map(([file, b]) => [file, { sha256: b.sha256, gitBlob: b.gitBlob }]));
    for (const [file, hash] of Object.entries(entry.files)) if (hash !== bindings[file].sha256) throw new Error(`recipe file differs from reviewed inventory: ${file}`);
    const recipeFiles = Object.fromEntries(Object.keys(entry.files).map(file => [file, report.sourceBindings[file]]));
    report.recipe = { sourceCommit: env.GITHUB_SHA, files: recipeFiles, sha256: sha('sha256', JSON.stringify(recipeFiles)) };
    const archiveSpawn = (command, args, options) => {
      const result = spawn(command, args, { ...options, env: child });
      commands.push({ stage: 'archive-read', command, args, cwd: options.cwd, status: result.status, stdout: Buffer.from(result.stdout ?? '').toString('utf8'), stderr: Buffer.from(result.stderr ?? '').toString('utf8'), ...(result.error ? { error: result.error.message } : {}) });
      return result;
    };
    const packed = JSON.parse(tarEntry(archive, 'package/package.json', archiveSpawn));
    tarEntry(archive, 'package/README.md', archiveSpawn);
    validatePackedClosure(packed, entry, name, unit.version);
    const consumer = JSON.parse(fs.readFileSync(path.join(root, dir, 'package.json')));
    const rootManifest = JSON.parse(fs.readFileSync(path.join(root, 'package.json')));
    const pin = validateManifestPins(rootManifest, JSON.parse(fs.readFileSync(path.join(root, `packages/${short}/package.json`))), withPin(consumer, rootManifest.packageManager), packed, name, unit.version);
    validateConsumerManifest(consumer, entry, pin);
    const manifest = scratchManifest(consumer, name, archive, pin);
    fs.writeFileSync(path.join(scratch, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
    for (const file of Object.keys(entry.files)) {
      if (file === RUNNER_FILE || file === `${dir}/package.json`) continue;
      const target = path.join(scratch, file.slice(dir.length + 1)); fs.mkdirSync(path.dirname(target), { recursive: true }); fs.copyFileSync(path.join(root, file), target);
    }
    report.toolchain = { node: process.version, pnpm: pnpm(['--version']).trim(), packageManager: pin };
    if (report.toolchain.pnpm !== pin.slice(5)) throw new Error('pnpm executable differs from manifest pin');
    const workspaceFile = path.join(scratch, 'pnpm-workspace.yaml');
    if (closureAgeExceptions(entry, name).length) {
      const approval = loadApprovedArtifacts(path.join(root, 'ci/approved-artifacts.json'));
      need(approval.problems.length === 0, approval.problems.join('\n'));
      validateClosureApproval(approval.entries, entry, name);
    }
    const workspaceBefore = scratchWorkspace(entry, name);
    fs.writeFileSync(workspaceFile, workspaceBefore);
    report.workspace = { file: workspaceFile, before: workspaceBefore, beforeSha256: sha('sha256', workspaceBefore) };
    pnpm(consumerInstallArgs(path.join(scratch, 'store')));
    const workspaceAfter = fs.readFileSync(workspaceFile, 'utf8');
    Object.assign(report.workspace, { after: workspaceAfter, afterSha256: sha('sha256', workspaceAfter) });
    validateWorkspaceBinding(report.workspace, entry, name, scratch);
    const installed = JSON.parse(fs.readFileSync(path.join(scratch, 'node_modules', ...name.split('/'), 'package.json')));
    if (installed.version !== unit.version || JSON.stringify(installed) !== JSON.stringify(packed)) throw new Error('installed package differs from sealed manifest');
    report.archiveIntegrity = integrity(fs.readFileSync(archive));
    if (report.archiveIntegrity !== unit.integrity) throw new Error('original archive integrity differs from sealed manifest');
    report.lock = fs.readFileSync(path.join(scratch, 'pnpm-lock.yaml'), 'utf8');
    report.lockSha256 = sha('sha256', report.lock);
    validateLockClosure(report.lock, entry, name, path.basename(archive));
    if (closureAgeExceptions(entry, name).length) {
      report.closureProvenance = JSON.parse(run(process.execPath, ['--input-type=module', '-e', closureProvenanceScript(name)], scratch, 'closure-provenance').trim());
      validateClosureProvenance(report.closureProvenance, report.lock, entry, name, scratch);
    }
    const runtimeExtras = runtimeEnvironment(name, env, { checkout: root, scratch });
    report.runtimeEnv = runtimeExtras;
    const output = run(process.execPath, ['consumer.mjs'], scratch, 'runtime', { env: { ...child, ...runtimeExtras, RIGHTKIT_CONSUMER_ARTIFACT: archive, RIGHTKIT_CONSUMER_ARTIFACT_SHA256: unit.sha256, RIGHTKIT_CONSUMER_EXPECTED_VERSION: unit.version, RIGHTKIT_CONSUMER_RESOLUTION: 'packed' } });
    report.mount = JSON.parse(output.trim());
    report.counts = validateMountResult(report.mount, entry, { sha256: unit.sha256, version: unit.version, archive });
    report.isolation = { checkout: root, scratch, originalArchive: archive, home, userconfig: child.npm_config_userconfig, globalconfig: child.npm_config_globalconfig, store: path.join(scratch, 'store'), systemRoot: env.SystemRoot || 'C:\\Windows', ignoreScripts: true, inheritWorkspace: false, scriptLifecycle: 'disabled', manifest, consumerManifestSha256: sha('sha256', fs.readFileSync(path.join(scratch, 'package.json'))) };
    if (sha('sha256', fs.readFileSync(archive)) !== unit.sha256 || fs.statSync(archive).size !== unit.size || sha('sha256', fs.readFileSync(path.join(packDir, 'manifest.json'))) !== sealed.manifestSha256) throw new Error('sealed bytes changed during consumer');
    for (const [file, binding] of Object.entries(bindings)) if (sha('sha256', fs.readFileSync(path.join(root, file))) !== binding.executionSha256) throw new Error('source/recipe changed during consumer');
    if (fs.readFileSync(workspaceFile, 'utf8') !== workspaceBefore) throw new Error('isolated workspace changed during consumer runtime');
    report.verified = true;
    const problems = npmRecipeQualificationProblems({ report, expected: { name, version: unit.version, commit: sealed.manifest.commit, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, manifestRunId: String(sealed.manifest.runId), manifestRunAttempt: String(sealed.manifest.runAttempt), manifestSha256: sealed.manifestSha256, archiveSha256: unit.sha256, archiveSize: unit.size, archiveIntegrity: unit.integrity, host: report.host, runnerOS: env.RUNNER_OS, arch: process.arch, packageManager: pin, archivePath: archive }, recipe: entry, sourceBindings: report.sourceBindings });
    if (problems.length) { report.verified = false; throw new Error(problems.join('\n')); }
    return report;
  } catch (error) { report.error = error.message; error.report = report; throw error; }
  finally { fs.writeFileSync(reportFile, `${JSON.stringify(report, null, 2)}\n`); fs.rmSync(scratch, { recursive: true, force: true }); }
}
