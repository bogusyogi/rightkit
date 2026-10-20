// Named PK11 recipe only. No registry credentials, builds, uploads or production adapters.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { childEnv, loadSealed, sha, tarEntry, integrity } from './upload.mjs';
import { isWithin } from './registry-consumer.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
export const RECIPE_FILES = ['ci/consumers/npm-logs/package.json', 'ci/consumers/npm-logs/README.md', 'ci/consumers/npm-logs/consumer.mjs', 'scripts/ci/npm-consumer.mjs'];
export const SOURCE_FILES = ['ci/consumer-recipes.json', 'package.json', 'packages/logs/package.json', 'packages/logs/README.md', 'packages/logs/src/index.ts', 'packages/logs/src/crash.ts', 'packages/logs/src/recorder.ts', 'crates/rightkit-logs/tests/scrub-parity.v1.json'];
export const RUNTIME_EXPORTS = ['RIGHTKIT_LOGS_WRITE_COMMAND', 'RIGHTKIT_LOGS_COLLECT_COMMAND', 'RIGHTKIT_LOGS_CLEAR_COMMAND', 'RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND', 'createLogsClient', 'createDiagnosticsRecorder', 'installWindowCrashCapture', 'scrubLogText', 'scrubWindowCrashText'];
export function validateVectors(doc) {
  if (doc?.schema !== 1 || !Array.isArray(doc.vectors) || doc.vectors.length !== 37) throw new Error('expected schema-1 source parity document with exactly 37 vectors');
  const names = new Set();
  for (const row of doc.vectors) {
    if (!row || typeof row.name !== 'string' || !row.name || names.has(row.name) || typeof row.expected !== 'string') throw new Error('invalid/duplicate parity vector');
    names.add(row.name);
    if (Object.keys(row).some(key => !['name', 'input', 'inputParts', 'expected', 'windowCrash'].includes(key))) throw new Error('unknown parity vector field');
    if ((typeof row.input === 'string') === Array.isArray(row.inputParts) || (row.inputParts && !row.inputParts.every(part => typeof part === 'string')) || (row.windowCrash !== undefined && typeof row.windowCrash !== 'boolean')) throw new Error('invalid parity input');
  }
  return doc.vectors.length;
}
export function validateSurface(pkg, readme) {
  if (pkg?.name !== '@rightkit/logs' || !pkg.exports || JSON.stringify(Object.keys(pkg.exports)) !== '["."]') throw new Error('unknown logs export subpaths');
  if (JSON.stringify(Object.keys(pkg.exports['.']).sort()) !== '["import","types"]' || pkg.exports['.'].import !== './dist/index.js' || pkg.exports['.'].types !== './dist/index.d.ts') throw new Error('unknown logs export conditions');
  const entry = readme.split(/\r?\n/).find(line => line.startsWith('| `@rightkit/logs` |'));
  if (!entry || RUNTIME_EXPORTS.some(name => !entry.includes('`' + name))) throw new Error('README runtime inventory differs from named recipe');
  const documented = [...entry.split('; types ')[0].matchAll(/`([^`]+)`/g)].map(match => match[1].split('(')[0]).filter(name => /^[A-Za-z_$][\w$]*$/.test(name) && !name.startsWith('rightkit_'));
  if (JSON.stringify(documented.sort()) !== JSON.stringify([...RUNTIME_EXPORTS].sort())) throw new Error('unknown README runtime name');
  // These packages deliberately have no dependency closure: no repository dependency may leak in.
  for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies']) if (Object.keys(pkg[field] ?? {}).length) throw new Error('new dependency closure requires recipe review');
  return RUNTIME_EXPORTS.length;
}
export function validateRuntime(result, vectors) {
  const counts = { exportNames: 9, parity: 37, crashParity: vectors.vectors.filter(row => row.windowCrash).length, diagnostics: 10, crashCapture: 3, logsExample: 1 };
  if (result?.schema !== 1 || result.recipe !== 'PK11-npm-logs' || result.passed !== true || JSON.stringify(Object.keys(result).sort()) !== '["counts","passed","recipe","schema"]' || JSON.stringify(result.counts) !== JSON.stringify(counts)) throw new Error('missing/unknown runtime proof or counts');
  return counts;
}
export function validateContext(env, platform = process.platform) {
  if (env.GITHUB_ACTIONS !== 'true') throw new Error('npm consumer requires GitHub CI');
  if (!['win32', 'darwin'].includes(platform) || env.RUNNER_OS !== (platform === 'win32' ? 'Windows' : 'macOS')) throw new Error('native Windows/macOS runner required');
  if (!/^[a-f0-9]{40}$/.test(env.GITHUB_SHA ?? '') || !/^\d+$/.test(env.GITHUB_RUN_ID ?? '') || !/^\d+$/.test(env.GITHUB_RUN_ATTEMPT ?? '')) throw new Error('missing CI source/run identity');
  for (const [key, value] of Object.entries(env)) if (value && /(?:TOKEN|PASSWORD|SECRET|CREDENTIAL|AUTHORIZATION|ACCESS_KEY|PRIVATE_KEY)/i.test(key)) throw new Error(`credential-bearing environment forbidden: ${key}`);
}
export function pnpmInvocation(args, platform = process.platform, env = process.env) {
  if (platform !== 'win32') return { command: 'pnpm', args };
  // cmd is needed for pnpm.cmd; reject expansion & command syntax before quoting.
  if (args.some(arg => /["%!*&|<>^\r\n]/.test(arg))) throw new Error('unsafe Windows pnpm argument');
  return { command: path.win32.join(env.SystemRoot || 'C:\\Windows', 'System32', 'cmd.exe'), args: ['/d', '/s', '/c', `pnpm ${args.map(arg => `"${arg}"`).join(' ')}`] };
}

/** Admission accepts authenticated expected identity, reviewed recipe & Git blob bindings.
 * No receipt self-assertion supplies trusted inventory or source bindings.
 */
export function npmConsumerQualificationProblems(input = {}) {
  try { return qualificationProblems(input); }
  catch { return ['npm PK11: malformed qualification evidence']; }
}
function qualificationProblems({ report, expected, recipe, sourceBindings } = {}) {
  const problems = [];
  const fail = message => problems.push(`npm PK11: ${message}`);
  if (!report || !expected || !recipe || !sourceBindings) return ['npm PK11: missing report/trusted identity/recipe/source bindings'];
  const identity = ['name', 'version', 'commit', 'runId', 'runAttempt', 'manifestRunId', 'manifestRunAttempt', 'manifestSha256', 'archiveSha256', 'archiveSize', 'archiveIntegrity', 'host', 'runnerOS', 'arch'];
  for (const field of identity) if (expected[field] === undefined || report[field] !== expected[field]) fail(`identity mismatch/missing ${field}`);
  if (report.schema !== 1 || report.kind !== 'npm-consumer' || report.qualification !== 'R2a' || report.recipeId !== 'PK11-npm-logs' || report.verified !== true || report.error !== undefined || report.name !== '@rightkit/logs') fail('unverified or unknown qualification');
  if (!/^[a-f0-9]{40}$/.test(report.commit ?? '') || !['windows', 'macos'].includes(report.host) || report.runnerOS !== (report.host === 'windows' ? 'Windows' : 'macOS') || !Number.isSafeInteger(report.archiveSize) || report.archiveSize < 1 || !/^[a-f0-9]{64}$/.test(report.archiveSha256 ?? '') || !/^[a-f0-9]{64}$/.test(report.manifestSha256 ?? '')) fail('malformed candidate/native identity');
  for (const field of ['runId', 'runAttempt', 'manifestRunId', 'manifestRunAttempt']) if (!/^\d+$/.test(String(report[field] ?? ''))) fail(`invalid ${field}`);
  const pin = expected.packageManager;
  if (!/^pnpm@\d+\.\d+\.\d+$/.test(pin ?? '') || report.toolchain?.packageManager !== pin || report.toolchain?.pnpm !== pin?.slice(5) || !/^v\d+\.\d+\.\d+$/.test(report.toolchain?.node ?? '')) fail('native toolchain/pnpm pin mismatch');
  if (recipe.pkId !== 'PK11' || recipe.id !== 'ci/consumers/npm-logs/consumer.mjs' || JSON.stringify(Object.keys(recipe.files ?? {}).sort()) !== JSON.stringify([...RECIPE_FILES].sort())) fail('unknown reviewed recipe inventory');
  const expectedRecipeFiles = {};
  if (JSON.stringify(Object.keys(report.sourceBindings ?? {}).sort()) !== JSON.stringify([...RECIPE_FILES, ...SOURCE_FILES].sort())) fail('unknown/missing source binding');
  for (const file of [...RECIPE_FILES, ...SOURCE_FILES]) {
    const trusted = sourceBindings[file]; const actual = report.sourceBindings?.[file];
    if (!trusted || !/^[a-f0-9]{64}$/.test(trusted.sha256 ?? '') || !/^[a-f0-9]{40}$/.test(trusted.gitBlob ?? '') || actual?.sha256 !== trusted.sha256 || actual?.gitBlob !== trusted.gitBlob) fail(`source blob mismatch ${file}`);
    if (RECIPE_FILES.includes(file)) {
      if (recipe.files?.[file] !== trusted?.sha256) fail(`reviewed recipe differs from candidate blob ${file}`);
      expectedRecipeFiles[file] = { sha256: trusted?.sha256, gitBlob: trusted?.gitBlob };
    }
  }
  if (report.recipe?.sourceCommit !== report.commit || JSON.stringify(report.recipe?.files) !== JSON.stringify(expectedRecipeFiles) || report.recipe?.sha256 !== sha('sha256', JSON.stringify(expectedRecipeFiles))) fail('recipe digest/source mismatch');
  const parityPath = SOURCE_FILES.at(-1);
  if (report.paritySource?.path !== parityPath || report.paritySource?.sha256 !== sourceBindings[parityPath]?.sha256 || report.paritySource?.gitBlob !== sourceBindings[parityPath]?.gitBlob) fail('parity source mismatch');
  const commands = report.commands;
  if (!Array.isArray(commands)) return [...problems, 'npm PK11: missing actual command transcript'];
  const stageCounts = { source: RECIPE_FILES.length + SOURCE_FILES.length, 'archive-read': 2, toolchain: 1, install: 1, runtime: 1 };
  for (const command of commands) if (!Object.hasOwn(stageCounts, command.stage) || command.status !== 0 || command.error || typeof command.stdout !== 'string' || typeof command.stderr !== 'string' || !Array.isArray(command.args) || command.args.some(arg => typeof arg !== 'string')) fail('unknown/failed/malformed actual command');
  for (const [stage, count] of Object.entries(stageCounts)) if (commands.filter(command => command.stage === stage).length !== count) fail(`missing/duplicate ${stage} stage`);
  const sources = commands.filter(command => command.stage === 'source');
  for (const file of [...RECIPE_FILES, ...SOURCE_FILES]) {
    const rows = sources.filter(command => command.command === 'git' && JSON.stringify(command.args) === JSON.stringify(['show', `${report.commit}:${file}`]));
    if (rows.length !== 1 || sha('sha256', rows[0]?.stdout ?? '') !== sourceBindings[file]?.sha256 || sha('sha1', Buffer.concat([Buffer.from(`blob ${Buffer.byteLength(rows[0]?.stdout ?? '')}\0`), Buffer.from(rows[0]?.stdout ?? '')])) !== sourceBindings[file]?.gitBlob) fail(`actual source command mismatch ${file}`);
  }
  const api = report.host === 'windows' ? path.win32 : path.posix;
  const iso = report.isolation;
  const inside = (parent, child) => { if (typeof parent !== 'string' || typeof child !== 'string' || !api.isAbsolute(parent) || !api.isAbsolute(child)) return false; const rel = api.relative(parent, child); return !rel || (!rel.startsWith(`..${api.sep}`) && rel !== '..' && !api.isAbsolute(rel)); };
  if (!iso || inside(iso.checkout, iso.scratch) || !api.isAbsolute(iso.scratch ?? '') || !api.isAbsolute(iso.checkout ?? '') || !api.isAbsolute(iso.originalArchive ?? '') || iso.ignoreScripts !== true || iso.inheritWorkspace !== false || iso.scriptLifecycle !== 'disabled' || !inside(iso.scratch, iso.home) || !inside(iso.home, iso.userconfig) || !inside(iso.home, iso.globalconfig) || !inside(iso.scratch, iso.store)) fail('scratch/lifecycle isolation mismatch');
  if (iso) {
    if (expected.archivePath !== undefined && iso.originalArchive !== expected.archivePath) fail('original archive path differs from candidate');
    const manifest = iso.manifest;
    if (!manifest || manifest.packageManager !== pin || manifest.private !== true || manifest.type !== 'module' || Object.keys(manifest.dependencies ?? {}).length !== 1 || manifest.dependencies?.['@rightkit/logs'] !== `file:${iso.originalArchive.replaceAll('\\', '/')}` || manifest.scripts || manifest.devDependencies || manifest.optionalDependencies || manifest.workspaces || sha('sha256', `${JSON.stringify(manifest, null, 2)}\n`) !== iso.consumerManifestSha256) fail('scratch manifest/original tgz mismatch');
    if (typeof report.lock !== 'string' || report.lockSha256 !== sha('sha256', report.lock) || !/^sha512-[A-Za-z0-9+/]{86}==$/.test(report.archiveIntegrity ?? '') || !report.lock.includes(report.archiveIntegrity) || !report.lock.includes('file:') || !report.lock.includes(api.basename(iso.originalArchive)) || /workspace:|link:|git\+|github:/.test(report.lock)) fail('fresh original-archive integrity lock missing or foreign source');
    const install = commands.find(command => command.stage === 'install');
    const version = commands.find(command => command.stage === 'toolchain');
    const installArgs = ['install', '--ignore-scripts', '--no-frozen-lockfile', '--ignore-workspace', '--store-dir', iso.store];
    const expectedCall = args => report.host === 'windows' ? { command: api.join(iso.systemRoot, 'System32', 'cmd.exe'), args: ['/d', '/s', '/c', `pnpm ${args.map(arg => `"${arg}"`).join(' ')}`] } : { command: 'pnpm', args };
    for (const [row, args] of [[install, installArgs], [version, ['--version']]]) {
      const call = expectedCall(args);
      if (!row || row.command !== call.command || JSON.stringify(row.args) !== JSON.stringify(call.args) || row.cwd !== iso.scratch) fail('actual pnpm command mismatch');
    }
    if (version?.stdout.trim() !== report.toolchain?.pnpm) fail('pnpm version output mismatch');
    const archiveRows = commands.filter(command => command.stage === 'archive-read');
    for (const entry of ['package/package.json', 'package/README.md']) if (!archiveRows.some(command => command.command === 'tar' && JSON.stringify(command.args) === JSON.stringify(['-xOzf', api.basename(iso.originalArchive), entry]) && command.cwd === api.dirname(iso.originalArchive))) fail('original archive command mismatch');
    try {
      const packed = JSON.parse(archiveRows.find(command => command.args.at(-1) === 'package/package.json').stdout);
      const readme = archiveRows.find(command => command.args.at(-1) === 'package/README.md').stdout;
      validateSurface(packed, readme);
      if (packed.name !== expected.name || packed.version !== expected.version || packed.packageManager !== pin || sha('sha256', readme) !== sourceBindings['packages/logs/README.md']?.sha256) fail('packed manifest/README/source mismatch');
    } catch { fail('actual packed manifest/README output invalid'); }
    for (const command of sources) if (command.cwd !== iso.checkout) fail('source command checkout mismatch');
    const runtime = commands.find(command => command.stage === 'runtime');
    if (!runtime || !api.isAbsolute(runtime.command ?? '') || runtime.cwd !== iso.scratch || JSON.stringify(runtime.args) !== '["consumer.mjs"]') fail('runtime command mismatch');
    try {
      const parityCommand = sources.find(command => command.args?.[1] === `${report.commit}:${parityPath}`);
      const vectors = JSON.parse(parityCommand.stdout); validateVectors(vectors);
      const output = JSON.parse(runtime.stdout.trim()); const counts = validateRuntime(output, vectors);
      if (JSON.stringify(report.counts) !== JSON.stringify(counts)) fail('runtime output/counts mismatch');
    } catch { fail('actual runtime/parity output invalid'); }
  }
  return problems;
}

export function runNpmConsumer({ packDir, name, reportFile, root = ROOT, scratchRoot = process.env.RUNNER_TEMP ?? os.tmpdir(), env = process.env, spawn = spawnSync } = {}) {
  validateContext(env);
  if (name !== '@rightkit/logs' || !packDir || !reportFile) throw new Error('named logs sealed pack & report required');
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
  if (!Number.isSafeInteger(unit.size) || unit.size <= 0 || !/^[a-f0-9]{64}$/.test(unit.sha256 ?? '') || unit.dir !== 'packages/logs') throw new Error('invalid sealed logs identity');
  const archive = path.resolve(packDir, unit.file);
  const commands = [];
  const scratch = fs.mkdtempSync(path.join(scratchRoot, 'rk-npm-logs-'));
  const home = path.join(scratch, 'home'); fs.mkdirSync(home);
  const child = Object.fromEntries(Object.entries(childEnv(env)).filter(([key]) => !/^(GITHUB_|ACTIONS_|RUNNER_)/.test(key)));
  Object.assign(child, { HOME: home, USERPROFILE: home, npm_config_userconfig: path.join(home, '.npmrc'), npm_config_globalconfig: path.join(home, 'global.npmrc'), npm_config_ignore_scripts: 'true', CI: 'true', COREPACK_ENABLE_PROJECT_SPEC: '1' });
  fs.writeFileSync(child.npm_config_userconfig, 'registry=https://registry.npmjs.org/\nignore-scripts=true\n'); fs.writeFileSync(child.npm_config_globalconfig, '');
  const run = (command, args, cwd = scratch, stage = 'source') => {
    const result = spawn(command, args, { cwd, env: child, encoding: 'utf8', timeout: 300_000, maxBuffer: 8 * 1024 * 1024 });
    commands.push({ stage, command, args, cwd, status: result.status, stdout: String(result.stdout ?? ''), stderr: String(result.stderr ?? ''), ...(result.error ? { error: result.error.message } : {}) });
    if (result.error || result.status !== 0) throw new Error(`consumer command failed: ${command} (${result.status})`);
    return String(result.stdout);
  };
  const pnpm = args => { const call = pnpmInvocation(args, process.platform, env); return run(call.command, call.args, scratch, args[0] === 'install' ? 'install' : 'toolchain'); };
  let report = { schema: 1, kind: 'npm-consumer', qualification: 'R2a', recipeId: 'PK11-npm-logs', name, version: unit.version, commit: sealed.manifest.commit, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, manifestRunId: String(sealed.manifest.runId), manifestRunAttempt: String(sealed.manifest.runAttempt), manifestSha256: sealed.manifestSha256, archiveSha256: unit.sha256, archiveSize: unit.size, host: process.platform === 'win32' ? 'windows' : 'macos', runnerOS: env.RUNNER_OS, arch: process.arch, commands, verified: false };
  try {
    const bindings = {};
    for (const file of [...RECIPE_FILES, ...SOURCE_FILES]) {
      const source = run('git', ['show', `${env.GITHUB_SHA}:${file}`], root);
      const bytes = fs.readFileSync(path.join(root, file));
      if (!bytes.equals(Buffer.from(source))) throw new Error(`recipe/source raw bytes differ from committed source: ${file}`);
      const canonical = Buffer.from(source);
      bindings[file] = { sha256: sha('sha256', canonical), executionSha256: sha('sha256', bytes), gitBlob: sha('sha1', Buffer.concat([Buffer.from(`blob ${canonical.length}\0`), canonical])) };
    }
    report.sourceBindings = bindings;
    const inventory = JSON.parse(fs.readFileSync(path.join(root, 'ci/consumer-recipes.json')));
    const reviewed = inventory.schema === 1 ? inventory.npm?.[name] : null;
    if (reviewed?.pkId !== 'PK11' || reviewed.id !== 'ci/consumers/npm-logs/consumer.mjs' || JSON.stringify(Object.keys(reviewed.files ?? {}).sort()) !== JSON.stringify([...RECIPE_FILES].sort())) throw new Error('missing/unknown reviewed npm recipe');
    for (const file of RECIPE_FILES) if (reviewed.files[file] !== bindings[file].sha256) throw new Error(`recipe file differs from reviewed inventory: ${file}`);
    const recipeFiles = Object.fromEntries(RECIPE_FILES.map(file => [file, { sha256: bindings[file].sha256, gitBlob: bindings[file].gitBlob }]));
    report.recipe = { sourceCommit: env.GITHUB_SHA, files: recipeFiles, sha256: sha('sha256', JSON.stringify(recipeFiles)) };
    report.paritySource = { path: SOURCE_FILES.at(-1), ...bindings[SOURCE_FILES.at(-1)] };
    const vectorsBytes = fs.readFileSync(path.join(root, SOURCE_FILES.at(-1)));
    const vectors = JSON.parse(vectorsBytes); validateVectors(vectors);
    const archiveSpawn = (command, args, options) => {
      const result = spawn(command, args, { ...options, env: child });
      commands.push({ stage: 'archive-read', command, args, cwd: options.cwd, status: result.status, stdout: Buffer.from(result.stdout ?? '').toString('utf8'), stderr: Buffer.from(result.stderr ?? '').toString('utf8'), ...(result.error ? { error: result.error.message } : {}) });
      return result;
    };
    const pkg = JSON.parse(tarEntry(archive, 'package/package.json', archiveSpawn));
    const readme = tarEntry(archive, 'package/README.md', archiveSpawn).toString('utf8');
    validateSurface(pkg, readme);
    if (pkg.version !== unit.version || fs.readFileSync(path.join(root, 'packages/logs/README.md'), 'utf8') !== readme) throw new Error('packed README/version differs from source identity');
    const manifest = JSON.parse(fs.readFileSync(path.join(root, 'ci/consumers/npm-logs/package.json')));
    const pin = JSON.parse(fs.readFileSync(path.join(root, 'package.json'))).packageManager;
    if (!/^pnpm@\d+\.\d+\.\d+$/.test(pin) || manifest.packageManager !== pin || pkg.packageManager !== pin) throw new Error('manifest pnpm pins differ');
    manifest.dependencies = { [name]: `file:${archive.replaceAll('\\', '/')}` };
    fs.writeFileSync(path.join(scratch, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
    fs.copyFileSync(path.join(root, 'ci/consumers/npm-logs/consumer.mjs'), path.join(scratch, 'consumer.mjs'));
    fs.writeFileSync(path.join(scratch, 'parity.json'), vectorsBytes);
    report.toolchain = { node: process.version, pnpm: pnpm(['--version']).trim(), packageManager: pin };
    if (report.toolchain.pnpm !== pin.slice(5)) throw new Error('pnpm executable differs from manifest pin');
    pnpm(['install', '--ignore-scripts', '--no-frozen-lockfile', '--ignore-workspace', '--store-dir', path.join(scratch, 'store')]);
    const installed = JSON.parse(fs.readFileSync(path.join(scratch, 'node_modules/@rightkit/logs/package.json')));
    validateSurface(installed, readme);
    if (installed.version !== unit.version || JSON.stringify(installed) !== JSON.stringify(pkg)) throw new Error('installed package differs from sealed manifest');
    const output = run(process.execPath, ['consumer.mjs'], scratch, 'runtime');
    report.counts = validateRuntime(JSON.parse(output.trim()), vectors);
    report.lock = fs.readFileSync(path.join(scratch, 'pnpm-lock.yaml'), 'utf8');
    report.archiveIntegrity = integrity(fs.readFileSync(archive));
    if (report.archiveIntegrity !== unit.integrity) throw new Error('original archive integrity differs from sealed manifest');
    report.lockSha256 = sha('sha256', report.lock);
    report.isolation = { checkout: root, scratch, originalArchive: archive, home, userconfig: child.npm_config_userconfig, globalconfig: child.npm_config_globalconfig, store: path.join(scratch, 'store'), systemRoot: env.SystemRoot || 'C:\\Windows', ignoreScripts: true, inheritWorkspace: false, scriptLifecycle: 'disabled', manifest, consumerManifestSha256: sha('sha256', fs.readFileSync(path.join(scratch, 'package.json'))) };
    if (sha('sha256', fs.readFileSync(archive)) !== unit.sha256 || fs.statSync(archive).size !== unit.size || sha('sha256', fs.readFileSync(path.join(packDir, 'manifest.json'))) !== sealed.manifestSha256) throw new Error('sealed bytes changed during consumer');
    for (const [file, binding] of Object.entries(bindings)) if (sha('sha256', fs.readFileSync(path.join(root, file))) !== binding.executionSha256) throw new Error('source/recipe changed during consumer');
    report.verified = true;
    const qualification = npmConsumerQualificationProblems({ report, expected: { name, version: unit.version, commit: sealed.manifest.commit, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, manifestRunId: String(sealed.manifest.runId), manifestRunAttempt: String(sealed.manifest.runAttempt), manifestSha256: sealed.manifestSha256, archiveSha256: unit.sha256, archiveSize: unit.size, archiveIntegrity: unit.integrity, host: report.host, runnerOS: env.RUNNER_OS, arch: process.arch, packageManager: pin, archivePath: archive }, recipe: reviewed, sourceBindings: bindings });
    if (qualification.length) { report.verified = false; throw new Error(qualification.join('\n')); }
    return report;
  } catch (error) { report.error = error.message; error.report = report; throw error; }
  finally { fs.writeFileSync(reportFile, `${JSON.stringify(report, null, 2)}\n`); fs.rmSync(scratch, { recursive: true, force: true }); }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [packDir, name, reportFile, ...extra] = process.argv.slice(2);
  if (extra.length) throw new Error('usage: npm-consumer.mjs <sealed-pack-dir> @rightkit/logs <report-file>');
  runNpmConsumer({ packDir, name, reportFile });
}
