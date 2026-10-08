#!/usr/bin/env node
import { ownershipLookup } from './ownership-lookup.mjs';

const write = (value, stream) => { if (typeof value === 'string' && value) stream.write(value); };
const jobEvent = (event) => event.type === 'JOB_EVENT' ? event.event : event;
const streamJobEvent = (event) => { const value = jobEvent(event); if (value.type === 'STDOUT') write(value.data, process.stdout); if (value.type === 'STDERR') write(value.data, process.stderr); };

try {
  const argv = process.argv.slice(2);
  if (argv[0] === 'storage') {
    const { storageMain } = await import('./storage-cli.mjs');
    process.exitCode = await storageMain(argv.slice(1));
  } else if (argv[0] === 'owns') {
    if (argv.length !== 2) throw new TypeError('usage: rightkit owns <capability> | rightkit owns --compact');
    const output = ownershipLookup(argv[1]);
    process.stdout.write(`${output}\n`);
    if (output.startsWith('No matching RightKit capability.')) process.exitCode = 1;
  } else {
    const [{ cancelManagedAgentJob, createRightkitRequest, listManagedAgentJobs, loadRuntimeIdentity, statusManagedAgentJob, submitAndWatchManagedAgentJob, watchManagedAgentJob, rerunOnCurrentRuntime, attachCargoLoginCredential }, { createRemoteCoordinator }] = await Promise.all([import('./index.mjs'), import('./remote-coordinator.mjs')]);
    const runtimeIdentity = await loadRuntimeIdentity();
    if (argv[0] === 'remote') {
      // Dual-host routing: detached SUBMIT plus one-shot STATUS/CANCEL by durable remote request ID.
      if (argv[1] === 'windows' && argv[2] === 'cargo') {
        const coordinator = createRemoteCoordinator();
        const ack = await coordinator.submitRemoteCargo({ argv: argv.slice(3), workspace: process.cwd() });
        process.stdout.write(`${JSON.stringify(ack)}\n`);
        process.stderr.write(`rightkit remote: request ${ack.requestId} accepted; schedule one wake in 15 minutes, then issue one 'rightkit remote status ${ack.requestId}' per wake until terminal. Do not poll or attach.\n`);
      } else if (['status', 'cancel'].includes(argv[1]) && argv.length === 3) {
        const coordinator = createRemoteCoordinator();
        const result = argv[1] === 'status' ? await coordinator.remoteStatus(argv[2]) : await coordinator.remoteCancel(argv[2]);
        process.stdout.write(`${JSON.stringify(result)}\n`);
        if (result.terminal && typeof result.exitCode === 'number' && result.state === 'TERMINAL') process.exitCode = result.exitCode;
      } else {
        throw new TypeError('usage: rightkit remote windows cargo <check|clippy|test|build> [args...] | rightkit remote <status|cancel> <request-id>');
      }
    } else if (argv[0] === 'list') {
      if (argv.length !== 1) throw new TypeError('usage: rightkit list');
      process.stdout.write(`${JSON.stringify(await listManagedAgentJobs({ runtimeIdentity }))}\n`);
    } else if (['status', 'attach', 'cancel'].includes(argv[0])) {
      if (argv.length !== 2) throw new TypeError(`usage: rightkit ${argv[0]} <request-id>`);
      const operation = argv[0] === 'status' ? statusManagedAgentJob : argv[0] === 'attach' ? watchManagedAgentJob : cancelManagedAgentJob;
      const result = await operation(argv[1], { runtimeIdentity, onEvent: streamJobEvent });
      process.stdout.write(`${JSON.stringify(result)}\n`);
      process.exitCode = result.exitCode ?? 0;
    } else {
      const message = await attachCargoLoginCredential(createRightkitRequest({ argv, workspace: process.cwd(), environment: process.env, runtimeIdentity }));
      let requestId; let cancelling;
      const cancel = (signal) => {
        if (!requestId || cancelling) return;
        process.stderr.write(`rightkit: ${signal} cancelling request ${requestId}\n`);
        cancelling = cancelManagedAgentJob(requestId, { runtimeIdentity, source: 'CLIENT_SIGNAL' })
          .then((receipt) => process.stderr.write(`rightkit: cancel ${JSON.stringify(receipt)}\n`))
          .catch((error) => process.stderr.write(`rightkit: cancel failed for ${requestId}: ${error.message}\n`));
      };
      const result = await submitAndWatchManagedAgentJob(message, {
        onEvent: streamJobEvent,
        onWaiting() { process.stderr.write('rightkit: broker runtime update in progress; waiting to submit\n'); },
        onSubmitted(submitted) {
          requestId = submitted.id;
          process.once('SIGINT', () => cancel('SIGINT'));
          process.once('SIGTERM', () => cancel('SIGTERM'));
          process.stderr.write(`rightkit: request ${requestId}\n`);
        }
      });
      await cancelling;
      if (result.exitCode !== 0 && result.error) process.stderr.write(`rightkit: ${result.error}\n`);
      process.exitCode = result.exitCode;
    }
  }
} catch (error) {
  if (error?.code === 'RK_RUNTIME_CHANGED') {
    const { rerunOnCurrentRuntime } = await import('./index.mjs');
    const handedOff = rerunOnCurrentRuntime(import.meta.url);
    if (handedOff !== null) process.exitCode = handedOff;
  } else {
    if (process.argv[2] === 'storage') process.stderr.write(`${JSON.stringify({ type: 'STORAGE_FAILED', error: error.message })}\n`);
    else process.stderr.write(`rightkit: ${error.message}\n`);
    process.exitCode = process.argv[2] === 'owns' ? 2 : 125;
  }
}
