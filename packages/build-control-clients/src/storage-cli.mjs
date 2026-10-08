import { spawn } from 'node:child_process';
import { readFile, readdir } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';
import { managedStorageRoot, noSymlinkPath, inside, storageReport, pruneManaged, startStorageRun, moveToVolumeTrash } from '../../build-control-core/src/storage.mjs';
import { xcodeManagedArgs, discoverDeviceVersions, connectedDeviceVersions, pruneDeviceSupport, recordDeviceUse } from '../../build-control-core/src/xcode-storage.mjs';

const usage = 'rightkit storage report|doctor|prune --dry-run|--apply [--managed-root DIR] [--app NAME=DIR] [--external DIR] [--trash PATH] [--broker auto|skip] [--device-support DIR --devices-json FILE] | run --app NAME -- COMMAND [ARGS] | xcode-args --run-root DIR';
export function parseStorageArgs(argv) {
  const command = argv[0]; if (!['report', 'doctor', 'prune', 'run', 'xcode-args'].includes(command)) throw new Error(usage);
  const options = { command, apps: [], external: [], trash: [], apply: false, managedRoot: managedStorageRoot() }; const seen = new Set();
  for (let i = 1; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === '--') { options.child = argv.slice(i + 1); break; }
    if (['--apply', '--dry-run'].includes(flag)) { if (seen.has('mode')) throw new Error('select exactly one prune mode'); seen.add('mode'); options.apply = flag === '--apply'; continue; }
    if (!['--managed-root', '--app', '--external', '--trash', '--device-support', '--devices-json', '--run-root', '--workspace', '--broker'].includes(flag) || !argv[i + 1] || argv[i + 1].startsWith('--')) throw new Error(`invalid storage option: ${flag}`);
    const value = argv[++i];
    if (['--external', '--trash'].includes(flag)) options[flag.slice(2)].push(resolve(value));
    else if (flag === '--app') { if (command === 'run') options.app = value; else { const split = value.indexOf('='); if (split < 1) throw new Error('--app requires NAME=DIR'); options.apps.push({ app: value.slice(0, split), path: resolve(value.slice(split + 1)) }); } }
    else { if (seen.has(flag)) throw new Error(`duplicate option: ${flag}`); seen.add(flag); const key = { '--managed-root': 'managedRoot', '--device-support': 'deviceSupport', '--devices-json': 'devicesJson', '--run-root': 'runRoot', '--workspace': 'workspace', '--broker': 'broker' }[flag]; options[key] = key === 'broker' ? value : resolve(value); }
  }
  if (command === 'prune' && !seen.has('mode')) throw new Error('prune requires --dry-run or --apply');
  if (command !== 'prune' && (seen.has('mode') || options.trash.length || options.deviceSupport)) throw new Error('prune options require prune command');
  if (command === 'run' && (!options.app || !options.child?.length)) throw new Error('run requires --app NAME -- COMMAND');
  if (command === 'xcode-args' && !options.runRoot && process.env.RIGHTKIT_RUN_ROOT) options.runRoot = resolve(process.env.RIGHTKIT_RUN_ROOT);
  if (options.broker && !['auto', 'skip'].includes(options.broker)) throw new Error('--broker requires auto|skip');
  if (command === 'xcode-args' && !options.runRoot) throw new Error('xcode-args requires --run-root or RIGHTKIT_RUN_ROOT');
  return options;
}
async function appList(workspace) { const rows = []; for (const entry of await readdir(workspace, { withFileTypes: true })) if (entry.isDirectory() && /right$/i.test(entry.name)) rows.push({ app: entry.name, path: join(workspace, entry.name) }); return rows; }
export async function storageMain(argv, { stdout = process.stdout, stderr = process.stderr, brokerGc } = {}) {
  const options = parseStorageArgs(argv); await noSymlinkPath(options.managedRoot);
  if (options.workspace) options.apps.push(...await appList(options.workspace));
  let result;
  if (options.command === 'report' || options.command === 'doctor') {
    result = await storageReport(options);
    if (options.command === 'doctor') { const { ownershipStorageFindings } = await import('../../build-control-core/src/storage-ownership.mjs'); result.findings = []; for (const app of options.apps) result.findings.push(...await ownershipStorageFindings({ appRoot: app.path, managedRoot: options.managedRoot })); }
  } else if (options.command === 'xcode-args') {
    result = await xcodeManagedArgs({ managedRoot: options.managedRoot, runRoot: options.runRoot });
    const versions = options.devicesJson ? connectedDeviceVersions(JSON.parse(await readFile(options.devicesJson, 'utf8'))) : await discoverDeviceVersions();
    await recordDeviceUse({ managedRoot: options.managedRoot, versions });
  }
  else if (options.command === 'run') {
    const run = await startStorageRun({ managedRoot: options.managedRoot, app: options.app }); stderr.write(`${JSON.stringify({ type: 'STORAGE_RUN_STARTED', root: run.root })}\n`);
    // CLI owns lease until child fully exits; SIGTERM propagates, marker stays active until then.
    const child = spawn(options.child[0], options.child.slice(1), { env: { ...process.env, ...run.env }, stdio: 'inherit' });
    const forward = signal => child.kill(signal); const onInt = () => forward('SIGINT'), onTerm = () => forward('SIGTERM'); process.on('SIGINT', onInt); process.on('SIGTERM', onTerm);
    let code; try { code = await new Promise((done, reject) => { child.once('error', reject); child.once('close', (exit, signal) => done(exit ?? (signal ? 128 : 1))); }); }
    finally { process.off('SIGINT', onInt); process.off('SIGTERM', onTerm); await run.finish(); stderr.write(`${JSON.stringify({ type: 'STORAGE_RUN_FINISHED', root: run.root })}\n`); }
    return code;
  } else {
    for (const path of options.trash) {
      if (inside(path, options.managedRoot) || inside(options.managedRoot, path)) throw new Error('managed storage cannot be selected for one-off Trash');
      if (!/(archive|trash|research)/i.test(basename(path)) && !/\.(dmg|pkg|msi|exe)$/i.test(path)) throw new Error('one-off Trash requires explicitly named archive/trash/research/installer');
      await noSymlinkPath(path);
    }
    result = { mode: options.apply ? 'apply' : 'dry-run', managed: [], trash: [] };
    for (const kind of ['runs', 'artifacts']) for (const app of await readdir(join(options.managedRoot, kind), { withFileTypes: true }).catch(e => e.code === 'ENOENT' ? [] : Promise.reject(e))) if (app.isDirectory() && !app.name.startsWith('.')) result.managed.push(await pruneManaged({ managedRoot: options.managedRoot, app: app.name, kind, apply: options.apply }));
    for (const path of options.trash) {
      if (inside(path, options.managedRoot) || inside(options.managedRoot, path)) throw new Error('managed storage cannot be selected for one-off Trash');
      if (!/(archive|trash|research)/i.test(basename(path)) && !/\.(dmg|pkg|msi|exe)$/i.test(path)) throw new Error('one-off Trash requires explicitly named archive/trash/research/installer');
      result.trash.push(await moveToVolumeTrash(path, { apply: options.apply }));
    }
    if (options.deviceSupport) { const versions = options.devicesJson ? connectedDeviceVersions(JSON.parse(await readFile(options.devicesJson, 'utf8'))) : await discoverDeviceVersions(); result.deviceSupport = await pruneDeviceSupport({ managedRoot: options.managedRoot, directory: options.deviceSupport, connectedVersions: versions, apply: options.apply }); }
    if (options.broker === 'skip') result.broker = { skipped: true };
    else {
      try {
        const { storageManagedAgentGc, loadRuntimeIdentity } = await import('./index.mjs');
        const receipt = await (brokerGc ?? storageManagedAgentGc)({ dryRun: !options.apply, runtimeIdentity: await loadRuntimeIdentity() });
        result.broker = receipt;
        if (receipt.result?.gc?.unenforceable) result.broker.error = 'broker cache budget remains unenforceable';
        if (receipt.result?.cache?.overByBytes > 0) result.broker.error = 'compiler cache remains over native LRU budget';
        if (receipt.result?.error || receipt.result?.deferred || receipt.result?.status === 'BUSY' || receipt.status === 'ERROR' || !receipt.result) result.broker.error = receipt.result?.error ?? 'broker GC deferred';
      } catch (error) { result.broker = { error: error.message }; }
    }
  }
  stdout.write(`${JSON.stringify(result, null, 2)}\n`); return result.errors?.length || result.findings?.length || result.broker?.error || result.managed?.some(plan => plan.overCapBytes > 0 || (plan.publicationState === 'unknown' && plan.retain.length > 0)) ? 1 : 0;
}
