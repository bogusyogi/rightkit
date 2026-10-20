#!/usr/bin/env node
// Diagnostic only. No qualification receipt, package mutation, sandbox bypass or replacement killer.
import assert from 'node:assert/strict';
import childProcess from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { syncBuiltinESMExports } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { consumerChildEnv } from './npm-recipe-consumer.mjs';

const FILE = fileURLToPath(import.meta.url);
const FROZEN = Object.freeze({ runId: '39496214838', runAttempt: '1', repository: 'bogusyogi/rightkit', commit: 'd37da6469cc4bf8a6819bc19778113c62d74f1f1', name: '@rightkit/qa-chrome', version: '0.1.9', file: 'rightkit-qa-chrome-0.1.9.tgz', size: 40513, sha256: 'bab8ff5fdbabfdc7bbdddcec0842fdc3aba398763960f0bf396ff0fd36608ba3' });
const MAX_BYTES = 2 * 1024 * 1024;
const errorRecord = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error), code: error?.code ?? null });
const requiredPath = name => { assert(process.env[name] && path.isAbsolute(process.env[name]), `${name} must be absolute`); return path.resolve(process.env[name]); };

function prepare(artifact, out) {
  const manifest = JSON.parse(fs.readFileSync(path.join(artifact, 'manifest.json'), 'utf8'));
  for (const key of ['runId', 'runAttempt', 'repository', 'commit']) assert.equal(manifest[key], FROZEN[key], `frozen manifest ${key}`);
  assert.equal(manifest.schema, 1); assert.equal(manifest.registry, 'npm');
  assert.equal(manifest.units?.length, 1);
  for (const key of ['name', 'version', 'file', 'size', 'sha256']) assert.equal(manifest.units[0][key], FROZEN[key], `frozen unit ${key}`);
  assert.equal(manifest.units[0].dir, 'packages/qa-chrome');
  const archive = path.join(artifact, FROZEN.file);
  const bytes = fs.readFileSync(archive);
  assert.equal(bytes.length, FROZEN.size);
  assert.equal(createHash('sha256').update(bytes).digest('hex'), FROZEN.sha256);
  const extracted = path.join(out, 'extracted');
  fs.mkdirSync(extracted); // Refuse reuse: no stale files can enter this import.
  const unpack = childProcess.spawnSync('tar', ['-xzf', archive, '-C', extracted], { env: consumerChildEnv(process.env, out), windowsHide: true, encoding: 'utf8', timeout: 30_000, maxBuffer: MAX_BYTES });
  assert.equal(unpack.error, undefined, 'sealed archive extraction failed');
  assert.equal(unpack.status, 0, `sealed archive extraction failed: ${unpack.stderr}`);
  const pkg = JSON.parse(fs.readFileSync(path.join(extracted, 'package/package.json'), 'utf8'));
  assert.equal(pkg.name, FROZEN.name); assert.equal(pkg.version, FROZEN.version);
  assert.deepEqual(pkg.dependencies ?? {}, {}); assert.deepEqual(pkg.optionalDependencies ?? {}, {}); assert.deepEqual(pkg.peerDependencies ?? {}, {});
  assert.equal(pkg.exports['.'].import, './dist/index.js');
  return path.join(extracted, 'package/dist/index.js');
}

async function diagnostic(entry, out) {
  const report = { diagnosticOnly: true, nativeHostQualified: false, frozen: FROZEN, host: { platform: process.platform, arch: process.arch, node: process.version }, nativeCalls: [], chrome: [], scenarios: [], errors: [], linuxPolicy: {}, windowsProbes: [] };
  const originalSpawn = childProcess.spawn;
  const originalSpawnSync = childProcess.spawnSync;
  let api;
  let activeBrowser;
  const launches = [];
  let captureScanner = false;
  let scannerCaptured = false;
  let nativeBytes = 0;
  // Output is a prefix bounded by a shared budget; loss is explicit, never silently treated as complete.
  const bound = value => {
    const bytes = Buffer.from(value ?? '');
    const take = Math.max(0, Math.min(bytes.length, MAX_BYTES - nativeBytes)); nativeBytes += take;
    return { text: bytes.subarray(0, take).toString('utf8'), bytes: bytes.length, truncated: take < bytes.length };
  };
  function windowsPhaseProbes(command, args, options) {
    // Capture only the real frozen scanner invocation; no copied script or fallback discovery.
    const commandIndex = args?.indexOf('-Command');
    const script = args?.[commandIndex + 1];
    if (commandIndex !== 2 || args.length !== 4 || args[0] !== '-NoProfile' || args[1] !== '-NonInteractive' || typeof script !== 'string' || !options?.env?.RK_QA_PROFILE || !script.includes('RK_QA_SCAN_COMPLETE')) {
      report.windowsProbes.push({ kind: 'scanner-capture-refused', reason: 'actual invocation did not match frozen scanner shape' });
      return;
    }
    scannerCaptured = true;
    const marker = phase => `[Console]::Error.WriteLine('RK_DIAGNOSTIC_PHASE ${phase}'); [Console]::Error.Flush(); `;
    const once = (source, anchor, replacement) => {
      assert.equal(source.split(anchor).length, 2, `diagnostic scanner anchor must occur exactly once: ${anchor}`);
      return source.replace(anchor, replacement);
    };
    function run(kind, body, extra = {}) {
      const invocation = [...args]; invocation[commandIndex + 1] = body;
      const started = performance.now();
      try {
        // Only diagnostic timeout differs; executable, arguments & isolated environment remain captured.
        const result = originalSpawnSync(command, invocation, { ...options, timeout: 15_000, maxBuffer: MAX_BYTES });
        report.windowsProbes.push({ kind, ...extra, timeoutMs: 15_000, durationMs: performance.now() - started, status: result.status, signal: result.signal, error: result.error ? errorRecord(result.error) : null, stdout: bound(result.stdout), stderr: bound(result.stderr) });
      } catch (error) { report.windowsProbes.push({ kind, ...extra, durationMs: performance.now() - started, error: errorRecord(error) }); }
    }
    run('powershell-entry', `${marker('ENTRY')}[ordered]@{ PSVersion = $PSVersionTable.PSVersion.ToString(); LanguageMode = [string]$ExecutionContext.SessionState.LanguageMode; PSHOME = $PSHOME } | ConvertTo-Json -Compress; ${marker('ENTRY_COMPLETE')}`);
    run('powershell-raw-console', `${marker('ENTRY')}
[Console]::Error.WriteLine('PSVersion=' + $PSVersionTable.PSVersion.ToString()); [Console]::Error.Flush();
[Console]::Error.WriteLine('LanguageMode=' + [string]$ExecutionContext.SessionState.LanguageMode); [Console]::Error.Flush();
[Console]::Error.WriteLine('PSHOME=' + $PSHOME); [Console]::Error.Flush();
[Console]::Error.WriteLine('PSModulePath=' + $env:PSModulePath); [Console]::Error.Flush();
${marker('EXIT')}`);
    for (const autoLoading of ['None', null]) {
      run(autoLoading ? 'powershell-explicit-cim' : 'powershell-explicit-cim-default-autoload', `function RkDiagnosticMark([string]$text) { [Console]::Error.WriteLine('RK_DIAGNOSTIC_PHASE ' + $text); [Console]::Error.Flush() }
$ErrorActionPreference = 'Stop'; ${autoLoading ? "$PSModuleAutoLoadingPreference = 'None';" : ''}
try {
  RkDiagnosticMark 'IMPORT_BEGIN';
  Import-Module ($PSHOME + '\\Modules\\CimCmdlets\\CimCmdlets.psd1') -ErrorAction Stop;
  RkDiagnosticMark 'IMPORT_END';
  RkDiagnosticMark 'QUERY_BEGIN';
  $rows = @(CimCmdlets\\Get-CimInstance Win32_Process -ErrorAction Stop);
  RkDiagnosticMark ('QUERY_END count=' + [string]$rows.Count);
} catch {
  RkDiagnosticMark ('ERROR_TYPE=' + $_.Exception.GetType().FullName);
  RkDiagnosticMark ('ERROR_ID=' + [string]$_.FullyQualifiedErrorId);
  RkDiagnosticMark ('ERROR_MESSAGE=' + $_.Exception.Message);
  if ($null -ne $_.Exception.InnerException) {
    RkDiagnosticMark ('INNER_ERROR_TYPE=' + $_.Exception.InnerException.GetType().FullName);
    RkDiagnosticMark ('INNER_ERROR_MESSAGE=' + $_.Exception.InnerException.Message);
  }
  RkDiagnosticMark ('ERROR_POSITION=' + [string]$_.InvocationInfo.PositionMessage);
  exit 1;
}`, { moduleAutoLoading: autoLoading ?? 'default' });
    }
    try {
      let instrumented = marker('SCRIPT_ENTRY') + script;
      instrumented = once(instrumented, '$assembly = [AppDomain]', `${marker('REFLECTION_EMIT_BEGIN')}$assembly = [AppDomain]`);
      instrumented = once(instrumented, '$native = $builder.CreateType();', `$native = $builder.CreateType(); ${marker('REFLECTION_EMIT_END')}`);
      instrumented = once(instrumented, '$rx = [regex]::new(', `${marker('REGEX_BEGIN')}$rx = [regex]::new(`);
      instrumented = once(instrumented, '; Get-CimInstance Win32_Process |', `; ${marker('REGEX_END')}& { ${marker('CIM_BEGIN')}Get-CimInstance Win32_Process; ${marker('CIM_END')} } |`);
      instrumented = once(instrumented, 'function RkProfile([uint32]$id, $rx) {', `function RkProfile([uint32]$id, $rx) { ${marker('PER_PID_NATIVE_BEGIN')}`);
      const rowQuery = "$row = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + [string]$id);";
      instrumented = once(instrumented, rowQuery, `${marker('PER_PID_NATIVE_END')}${marker('PER_PID_CIM_BEGIN')}${rowQuery} ${marker('PER_PID_CIM_END')}`);
      instrumented += `; ${marker('SCRIPT_END')}`;
      run('sealed-scanner-phases', instrumented, { originalScriptSha256: createHash('sha256').update(script).digest('hex'), instrumentedScriptSha256: createHash('sha256').update(instrumented).digest('hex') });
    } catch (error) { report.windowsProbes.push({ kind: 'scanner-instrumentation-refused', error: errorRecord(error) }); }
  }
  childProcess.spawnSync = function (command, args, options) {
    if (process.platform === 'win32' && captureScanner && !scannerCaptured && command === 'powershell.exe') windowsPhaseProbes(command, args, options);
    const started = performance.now();
    const result = originalSpawnSync.apply(this, arguments); // Preserve every option, timeout & return value.
    const durationMs = performance.now() - started;
    const profile = options?.env?.RK_QA_PROFILE;
    if (/powershell(?:\.exe)?$/i.test(String(command))) {
      report.nativeCalls.push({ kind: profile ? 'powershell-owned-probe' : 'powershell-native-call', profile: profile ?? null, durationMs, status: result.status, signal: result.signal, error: result.error ? errorRecord(result.error) : null, stdout: bound(result.stdout), stderr: bound(result.stderr) });
    }
    // ps scans enumerate internally; retain only rows bearing this launch's synthetic profile.
    if (command === 'ps') {
      const rows = String(result.stdout ?? '').split(/\r?\n/).filter(line => launches.some(launch => line.includes(`--user-data-dir=${launch.profileDir}`)));
      report.nativeCalls.push({ kind: 'posix-owned-probe', durationMs, status: result.status, signal: result.signal, error: result.error ? errorRecord(result.error) : null, stdout: bound(rows.join('\n')), stderr: bound(result.stderr) });
    }
    return result;
  };
  childProcess.spawn = function (command, args, options) {
    const child = originalSpawn.apply(this, arguments);
    const profileFlag = args?.find(arg => typeof arg === 'string' && arg.startsWith('--user-data-dir='));
    if (profileFlag) {
      const launch = { child, profileDir: profileFlag.slice('--user-data-dir='.length), wsEndpoint: undefined };
      launches.push(launch);
      const observation = { pid: child.pid ?? null, profileDir: launch.profileDir, executable: String(command), stderr: '', stderrBytes: 0, stderrTruncated: false, exit: null, error: null, recovery: null };
      report.chrome.push(observation); launch.observation = observation;
      child.stderr?.on('data', chunk => {
        const bytes = Buffer.from(chunk); observation.stderrBytes += bytes.length;
        const left = Math.max(0, MAX_BYTES - Buffer.byteLength(observation.stderr));
        observation.stderr += bytes.subarray(0, left).toString('utf8');
        if (bytes.length > left) observation.stderrTruncated = true;
        launch.wsEndpoint ??= observation.stderr.match(/DevTools listening on (ws:\/\/\S+)/)?.[1];
      });
      child.on('exit', (code, signal) => { observation.exit = { code, signal }; });
      child.on('error', error => { observation.error = errorRecord(error); });
    }
    return child;
  };
  syncBuiltinESMExports(); // Frozen module's named built-in imports see observational wrappers.
  async function clean(browser, close, beforeClones) {
    assert.equal(close.exited, true); assert.equal(close.profileRemoved, true); assert.equal(close.profileRemoval, 'removed');
    assert.equal(close.livenessUnknown, undefined); assert.deepEqual(close.leftoverPids, []);
    assert(!fs.existsSync(browser.profileDir), 'profile directory remains');
    const scan = api.probeProfileProcesses(browser.profileDir);
    report.scenarios.at(-1).finalScan = scan;
    assert.deepEqual(scan, [], 'profile process scan must succeed & be empty');
    assert.deepEqual(api.codeSignCloneEntries().filter(item => !beforeClones.has(item)), []);
  }
  try {
    if (process.platform === 'linux') {
      for (const name of ['kernel/unprivileged_userns_clone', 'kernel/apparmor_restrict_unprivileged_userns', 'user/max_user_namespaces']) {
        try { report.linuxPolicy[name] = fs.readFileSync(`/proc/sys/${name}`, 'utf8').trim(); }
        catch (error) { report.linuxPolicy[name] = errorRecord(error); }
      }
      for (const name of ['/proc/self/attr/current', '/sys/module/apparmor/parameters/enabled']) {
        try { report.linuxPolicy[name] = fs.readFileSync(name, 'utf8').trim(); }
        catch (error) { report.linuxPolicy[name] = errorRecord(error); }
      }
    }
    api = await import(pathToFileURL(entry).href);
    if (process.platform === 'win32') {
      const emptyProfile = fs.mkdtempSync(path.join(out, 'rk-empty-scan-'));
      try {
        captureScanner = true;
        const started = performance.now();
        const scan = api.probeProfileProcesses(emptyProfile);
        report.windowsProbes.push({ kind: 'original-empty-profile-scan', scan, includesDiagnosticProbes: true, durationMs: performance.now() - started });
        if (!scannerCaptured) report.windowsProbes.push({ kind: 'scanner-unavailable', reason: 'frozen API did not provide an actual scanner invocation; no diagnostic scanner was run' });
      } catch (error) { report.windowsProbes.push({ kind: 'empty-profile-probe-error', error: errorRecord(error) }); }
      finally {
        captureScanner = false;
        try { fs.rmdirSync(emptyProfile); }
        catch (error) { report.windowsProbes.push({ kind: 'empty-profile-cleanup-error', error: errorRecord(error) }); }
      }
    }
    const current = fs.realpathSync(api.managedCurrentDir());
    const key = api.chromeForTestingPlatform();
    const pin = api.PINNED_CHROME_FOR_TESTING;
    const files = pin.downloads.filter(item => item.platform === key).map(item => ({ kind: item.kind ?? 'chrome', url: item.url, sha256: item.sha256 }));
    assert(files.some(item => item.kind === 'chrome'));
    assert(files.every(item => /^[a-f0-9]{64}$/.test(item.sha256)));
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(current, 'install.json'), 'utf8')), { version: pin.version, platform: key, files });
    const executablePath = fs.realpathSync(api.managedExecutable(current, key));
    const relative = path.relative(current, executablePath);
    assert(relative && !path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`));
    for (const leaveProfile of [false, true]) {
      const scenario = { name: leaveProfile ? 'incomplete-cleanup-visible-then-recovered' : 'normal-close', assertionsCompleted: false, liveScan: null, close: null, recovery: null, error: null };
      report.scenarios.push(scenario);
      const beforeClones = new Set(api.codeSignCloneEntries());
      try {
        activeBrowser = await api.launchChromeForTesting({ executablePath, profilePrefix: 'rk-r2-qa-chrome-' });
        assert(fs.existsSync(activeBrowser.profileDir));
        scenario.liveScan = api.probeProfileProcesses(activeBrowser.profileDir);
        assert(scenario.liveScan !== null && scenario.liveScan.length > 0, 'live profile ownership scan must succeed');
        const response = await fetch(`http://127.0.0.1:${activeBrowser.port}/json/version`, { signal: AbortSignal.timeout(10_000) });
        assert(response.ok, 'DevTools version endpoint failed');
        const version = await response.json();
        assert.equal(version.Browser.split('/').at(-1), pin.version); assert.equal(version.webSocketDebuggerUrl, activeBrowser.wsEndpoint);
        scenario.close = await activeBrowser.close(leaveProfile ? { profileRemoveMs: 0 } : {});
        assert.equal(await activeBrowser.close(), scenario.close);
        if (leaveProfile) {
          assert.equal(scenario.close.exited, true); assert.equal(scenario.close.profileRemoved, false); assert.equal(scenario.close.profileRemoval, 'not-attempted');
          assert.equal(scenario.close.livenessUnknown, undefined); assert.deepEqual(scenario.close.leftoverPids, []);
          assert(fs.existsSync(activeBrowser.profileDir));
          scenario.recovery = await api.closeChromeGracefully({ child: activeBrowser.process, wsEndpoint: activeBrowser.wsEndpoint, profileDir: activeBrowser.profileDir });
          await clean(activeBrowser, scenario.recovery, beforeClones);
        } else await clean(activeBrowser, scenario.close, beforeClones);
        scenario.assertionsCompleted = true;
      } catch (error) { scenario.error = errorRecord(error); throw error; }
      finally {
        if (activeBrowser && !scenario.assertionsCompleted) {
          try { scenario.recovery = await api.closeChromeGracefully({ child: activeBrowser.process, wsEndpoint: activeBrowser.wsEndpoint, profileDir: activeBrowser.profileDir }); }
          catch (error) { report.errors.push({ phase: 'scenario-cleanup', ...errorRecord(error) }); }
        }
        activeBrowser = undefined;
      }
    }
  } catch (error) { report.errors.push({ phase: 'diagnostic', ...errorRecord(error) }); process.exitCode = 1; }
  finally {
    // Also captures startup failures, where launcher already closed internally & returned no browser.
    // Cleanup receives original ChildProcess only; never signal a saved PID or enumerate unrelated targets.
    for (const launch of launches) {
      try { launch.observation.recovery = await api.closeChromeGracefully({ child: launch.child, wsEndpoint: launch.wsEndpoint, profileDir: launch.profileDir }); }
      catch (error) { report.errors.push({ phase: 'final-cleanup', ...errorRecord(error) }); process.exitCode = 1; }
    }
    childProcess.spawn = originalSpawn; childProcess.spawnSync = originalSpawnSync; syncBuiltinESMExports();
    fs.writeFileSync(path.join(out, 'diagnostic.json'), `${JSON.stringify(report, null, 2)}\n`);
    console.log(JSON.stringify({ diagnosticOnly: true, nativeHostQualified: false, scenariosObserved: report.scenarios.length, errors: report.errors, report: 'diagnostic.json' }));
  }
}

async function main() {
  if (process.argv[2] === '--child') return diagnostic(process.argv[3], process.argv[4]);
  const artifact = requiredPath('CHROME_DIAGNOSTIC_ARTIFACT');
  const out = requiredPath('CHROME_DIAGNOSTIC_OUT');
  const managed = requiredPath('RIGHTKIT_CHROME_FOR_TESTING_ROOT');
  fs.mkdirSync(out, { recursive: true });
  try {
    const entry = prepare(artifact, out);
    const home = path.join(out, 'home'); fs.mkdirSync(home, { recursive: true });
    const env = consumerChildEnv(process.env, home);
    env.RIGHTKIT_CHROME_FOR_TESTING_ROOT = managed;
    const child = childProcess.spawnSync(process.execPath, [FILE, '--child', entry, out], { env, windowsHide: true, encoding: 'utf8', timeout: 240_000, maxBuffer: MAX_BYTES });
    fs.writeFileSync(path.join(out, 'child-status.json'), `${JSON.stringify({ diagnosticOnly: true, nativeHostQualified: false, status: child.status, signal: child.signal, error: child.error ? errorRecord(child.error) : null, stdout: child.stdout, stderr: child.stderr }, null, 2)}\n`);
    process.exitCode = child.error || child.status !== 0 ? 1 : 0;
  } catch (error) {
    fs.writeFileSync(path.join(out, 'preflight-error.json'), `${JSON.stringify({ diagnosticOnly: true, nativeHostQualified: false, error: errorRecord(error) }, null, 2)}\n`);
    process.exitCode = 1;
  }
}
await main();
